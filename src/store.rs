//! Sharded-folder key/value store: one JSON file per link at
//! `<root>/links/<h0h1>/<h2h3>/<slug>.json`, where `h` is the hex SHA-256 of the slug.
//! Writes go to `<root>/tmp` first; creates are hard-linked into place (fails if the
//! slug exists, so two creates can't race), updates are renamed over the old file.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Link {
    pub slug: String,
    pub url: String,
    /// Hex SHA-256 of the link's edit key. The key itself is never stored.
    pub key_hash: String,
    pub created: u64,
    pub updated: u64,
}

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
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("links"))?;
        fs::create_dir_all(root.join("tmp"))?;
        Ok(Store { root })
    }

    /// Callers must pass a validated slug (see `valid_slug`): it becomes a file name.
    fn path(&self, slug: &str) -> PathBuf {
        let h = sha256_hex(slug);
        self.root.join("links").join(&h[0..2]).join(&h[2..4]).join(format!("{slug}.json"))
    }

    fn write_tmp(&self, link: &Link) -> io::Result<PathBuf> {
        let name: String = rand::thread_rng().sample_iter(&Alphanumeric).take(16).map(char::from).collect();
        let tmp = self.root.join("tmp").join(name);
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(link)?)?;
        f.sync_all()?;
        Ok(tmp)
    }

    pub fn get(&self, slug: &str) -> io::Result<Option<Link>> {
        match fs::read(self.path(slug)) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn create(&self, link: &Link) -> Result<(), CreateError> {
        let path = self.path(&link.slug);
        fs::create_dir_all(path.parent().expect("sharded path has a parent"))?;
        let tmp = self.write_tmp(link)?;
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(CreateError::Exists),
            Err(e) => Err(e.into()),
        }
    }

    pub fn replace(&self, link: &Link) -> io::Result<()> {
        let tmp = self.write_tmp(link)?;
        fs::rename(&tmp, self.path(&link.slug))
    }

    pub fn delete(&self, slug: &str) -> io::Result<bool> {
        match fs::remove_file(self.path(slug)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
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
    }

    #[test]
    fn files_are_sharded() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.create(&link("abc")).unwrap();
        let h = sha256_hex("abc");
        assert!(dir.path().join("links").join(&h[0..2]).join(&h[2..4]).join("abc.json").is_file());
    }
}
