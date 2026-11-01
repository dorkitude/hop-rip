//! Packed-shard key/value store: links live in 65,536 shard files,
//! `<root>/shards/<h0h1>/<h2h3>.json`, where `h` is the hex SHA-256 of the slug. Each shard
//! is a JSON object of slug -> link, so a 4 KB block holds a dozen or more links.
//!
//! One process owns a data folder (enforced with an exclusive lock on `<root>/hop.lock`).
//! Writes take an in-memory lock for their shard, rewrite it to `<root>/tmp` and rename it
//! into place; reads take no lock, because the rename is atomic.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Writes to shards that share a stripe wait for each other; 4,096 stripes keep that rare.
const LOCK_STRIPES: usize = 4096;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Link {
    pub slug: String,
    pub url: String,
    /// Hex SHA-256 of the link's edit key. The key itself is never stored.
    pub key_hash: String,
    pub created: u64,
    pub updated: u64,
}

type Shard = BTreeMap<String, Link>;

#[derive(Debug)]
pub enum CreateError {
    Exists,
    Io(io::Error),
}

impl From<io::Error> for CreateError {
    fn from(e: io::Error) -> Self {
        CreateError::Io(e)
    }
}

pub struct Store {
    root: PathBuf,
    locks: Vec<Mutex<()>>,
    /// Held for the store's lifetime: the exclusive lock on `hop.lock`.
    _lock_file: fs::File,
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("shards"))?;
        fs::create_dir_all(root.join("tmp"))?;
        let lock_file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(root.join("hop.lock"))?;
        lock_file.try_lock().map_err(|_| {
            io::Error::new(io::ErrorKind::WouldBlock, format!("{} is in use by another hop process", root.display()))
        })?;
        let store = Store { root, locks: (0..LOCK_STRIPES).map(|_| Mutex::new(())).collect(), _lock_file: lock_file };
        store.migrate_file_per_link()?;
        Ok(store)
    }

    /// Callers must pass a validated slug (see `valid_slug`).
    fn shard_id(slug: &str) -> (String, usize) {
        let h = sha256_hex(slug);
        let n = usize::from_str_radix(&h[0..4], 16).expect("hex");
        (h[0..4].to_string(), n)
    }

    fn shard_path(&self, id: &str) -> PathBuf {
        self.root.join("shards").join(&id[0..2]).join(format!("{}.json", &id[2..4]))
    }

    fn read_shard(&self, id: &str) -> io::Result<Shard> {
        match fs::read(self.shard_path(id)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Shard::new()),
            Err(e) => Err(e),
        }
    }

    /// Replaces the shard file in one atomic rename, or removes it once it's empty.
    fn write_shard(&self, id: &str, shard: &Shard) -> io::Result<()> {
        let path = self.shard_path(id);
        if shard.is_empty() {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        fs::create_dir_all(path.parent().expect("shard path has a parent"))?;
        let name: String = rand::thread_rng().sample_iter(&Alphanumeric).take(16).map(char::from).collect();
        let tmp = self.root.join("tmp").join(name);
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec(shard)?)?;
        f.sync_all()?;
        fs::rename(&tmp, &path)
    }

    /// Runs `f` on the slug's shard under that shard's write lock and saves the result.
    fn modify<T>(&self, slug: &str, f: impl FnOnce(&mut Shard) -> T) -> io::Result<T> {
        let (id, n) = Self::shard_id(slug);
        let _guard = self.locks[n % LOCK_STRIPES].lock().unwrap_or_else(|p| p.into_inner());
        let mut shard = self.read_shard(&id)?;
        let before = shard.clone();
        let out = f(&mut shard);
        if shard != before {
            self.write_shard(&id, &shard)?;
        }
        Ok(out)
    }

    pub fn get(&self, slug: &str) -> io::Result<Option<Link>> {
        let (id, _) = Self::shard_id(slug);
        Ok(self.read_shard(&id)?.remove(slug))
    }

    pub fn create(&self, link: &Link) -> Result<(), CreateError> {
        let created = self.modify(&link.slug, |shard| {
            if shard.contains_key(&link.slug) {
                return false;
            }
            shard.insert(link.slug.clone(), link.clone());
            true
        })?;
        if created { Ok(()) } else { Err(CreateError::Exists) }
    }

    pub fn replace(&self, link: &Link) -> io::Result<()> {
        self.modify(&link.slug, |shard| {
            shard.insert(link.slug.clone(), link.clone());
        })
    }

    pub fn delete(&self, slug: &str) -> io::Result<bool> {
        self.modify(slug, |shard| shard.remove(slug).is_some())
    }

    /// Moves links from the older one-file-per-link layout (`links/**/<slug>.json`, two or
    /// three folders deep) into shards, then removes the emptied `links` folder.
    fn migrate_file_per_link(&self) -> io::Result<()> {
        let old_root = self.root.join("links");
        if !old_root.is_dir() {
            return Ok(());
        }
        let mut dirs = vec![old_root.clone()];
        let mut files = Vec::new();
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "json") {
                    files.push(path);
                }
            }
        }
        for path in files {
            let link: Link = serde_json::from_slice(&fs::read(&path)?)?;
            match self.create(&link) {
                Ok(()) | Err(CreateError::Exists) => fs::remove_file(&path)?,
                Err(CreateError::Io(e)) => return Err(e),
            }
        }
        fs::remove_dir_all(&old_root)?;
        eprintln!("hop: moved one-file-per-link data into shards");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(slug: &str) -> Link {
        Link { slug: slug.into(), url: "https://example.com/".into(), key_hash: sha256_hex("k"), created: 1, updated: 1 }
    }

    #[test]
    fn create_get_replace_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.get("abc").unwrap(), None);
        store.create(&link("abc")).unwrap();
        assert!(matches!(store.create(&link("abc")), Err(CreateError::Exists)));
        let mut l = store.get("abc").unwrap().unwrap();
        l.url = "https://example.org/".into();
        store.replace(&l).unwrap();
        assert_eq!(store.get("abc").unwrap().unwrap().url, "https://example.org/");
        assert!(store.delete("abc").unwrap());
        assert!(!store.delete("abc").unwrap());
        assert_eq!(fs::read_dir(dir.path().join("tmp")).unwrap().count(), 0);
        let (id, _) = Store::shard_id("abc");
        assert!(!store.shard_path(&id).exists(), "empty shards are removed");
    }

    #[test]
    fn links_share_shard_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for i in 0..2000 {
            store.create(&link(&format!("s{i}"))).unwrap();
        }
        let shards: usize = fs::read_dir(dir.path().join("shards"))
            .unwrap()
            .map(|d| fs::read_dir(d.unwrap().path()).unwrap().count())
            .sum();
        assert!(shards < 2000, "2000 links should need fewer than 2000 files, got {shards}");
        let (id, _) = Store::shard_id("s7");
        assert!(store.read_shard(&id).unwrap().contains_key("s7"));
    }

    #[test]
    fn concurrent_creates_in_one_shard_all_land() {
        // Find 16 slugs that hash to the same shard, then create them from 4 threads at once.
        let target = Store::shard_id("x0").0;
        let same: Vec<String> =
            (0u64..).map(|i| format!("x{i}")).filter(|s| Store::shard_id(s).0 == target).take(16).collect();
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(Store::open(dir.path()).unwrap());
        let handles: Vec<_> = same
            .chunks(4)
            .map(|chunk| {
                let (store, chunk) = (store.clone(), chunk.to_vec());
                std::thread::spawn(move || chunk.iter().for_each(|s| store.create(&link(s)).unwrap()))
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(store.read_shard(&target).unwrap().len(), 16);
    }

    #[test]
    fn second_process_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let _store = Store::open(dir.path()).unwrap();
        assert!(Store::open(dir.path()).is_err());
    }

    #[test]
    fn migrates_file_per_link_layouts() {
        let dir = tempfile::tempdir().unwrap();
        for (slug, depth) in [("two", 2), ("three", 3)] {
            let h = sha256_hex(slug);
            let mut p = dir.path().join("links");
            for d in 0..depth {
                p = p.join(&h[d * 2..d * 2 + 2]);
            }
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join(format!("{slug}.json")), serde_json::to_vec(&link(slug)).unwrap()).unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        assert!(!dir.path().join("links").exists());
        assert_eq!(store.get("two").unwrap().unwrap().slug, "two");
        assert_eq!(store.get("three").unwrap().unwrap().slug, "three");
    }
}
