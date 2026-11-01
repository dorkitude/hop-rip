//! `hop`: the server and the CLI in one binary.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "hop", version, about = "Tiny URL shortener: server, API client and CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (web app, API and redirects).
    Serve {
        /// Directory for the sharded link store.
        #[arg(long, env = "HOP_DATA", default_value = "./data")]
        data: PathBuf,
        #[arg(long, env = "HOP_ADDR", default_value = "127.0.0.1:8080")]
        addr: String,
        /// Public base URL for short links, e.g. https://hop.rip (default: from the Host header).
        #[arg(long, env = "HOP_BASE_URL")]
        base_url: Option<String>,
    },
    /// Shorten a URL. Prints the short URL and saves the edit key locally.
    New {
        url: String,
        /// Custom slug (a-z, 0-9, - and _).
        #[arg(long)]
        slug: Option<String>,
        #[command(flatten)]
        remote: Remote,
    },
    /// Show where a short link goes.
    Get {
        slug: String,
        #[command(flatten)]
        remote: Remote,
    },
    /// Point an existing short link at a new URL (needs its edit key).
    Edit {
        slug: String,
        url: String,
        #[command(flatten)]
        remote: Remote,
        /// Edit key (default: the one `hop new` saved).
        #[arg(long)]
        key: Option<String>,
    },
    /// Delete a short link (needs its edit key).
    Rm {
        slug: String,
        #[command(flatten)]
        remote: Remote,
        #[arg(long)]
        key: Option<String>,
    },
}

#[derive(clap::Args)]
struct Remote {
    /// Server to talk to.
    #[arg(long, env = "HOP_SERVER", default_value = "https://hop.rip")]
    server: String,
}

impl Remote {
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.server.trim_end_matches('/'), path)
    }
}

/// Edit keys `hop new` received, in `~/.config/hop/keys.json` as {"<server>/<slug>": "<key>"}.
fn keys_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("hop").join("keys.json")
}

fn load_keys() -> BTreeMap<String, String> {
    fs::read(keys_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_keys(keys: &BTreeMap<String, String>) -> std::io::Result<()> {
    let path = keys_path();
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, serde_json::to_vec_pretty(keys)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn key_id(remote: &Remote, slug: &str) -> String {
    format!("{}/{}", remote.server.trim_end_matches('/'), slug.to_lowercase())
}

fn key_for(remote: &Remote, slug: &str, given: Option<String>) -> Result<String, String> {
    given
        .or_else(|| load_keys().remove(&key_id(remote, slug)))
        .ok_or_else(|| format!("no edit key for {slug}: pass --key"))
}

/// Runs a request and returns its JSON body (or an error message from the server).
fn call(req: ureq::Request, body: Option<Value>) -> Result<Value, String> {
    let res = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match res {
        Ok(r) if r.status() == 204 => Ok(Value::Null),
        Ok(r) => r.into_json().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, r)) => {
            let msg = r.into_json::<Value>().ok().and_then(|v| v["error"].as_str().map(String::from));
            Err(format!("{code}: {}", msg.unwrap_or_else(|| "request failed".into())))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Raise the soft open-file limit toward the hard limit. Each request can hold a socket and a shard file,
/// and the usual soft default of 1,024 runs out under load long before the CPU does.
#[cfg(unix)]
fn raise_nofile() {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return;
    }
    let before = lim.rlim_cur;
    // Some systems (macOS) report an unlimited hard limit but refuse a soft limit that large, so step down.
    for want in [lim.rlim_max.min(1 << 20), 65_536, 10_240] {
        if want <= before {
            break;
        }
        let new = libc::rlimit { rlim_cur: want, rlim_max: lim.rlim_max };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &new) } == 0 {
            eprintln!("hop: open-file limit {before} -> {want}");
            return;
        }
    }
}

#[cfg(not(unix))]
fn raise_nofile() {}

async fn serve(data: PathBuf, addr: String, base_url: Option<String>) -> Result<(), String> {
    raise_nofile();
    let store = hop_rip::store::Store::open(&data).map_err(|e| format!("open {}: {e}", data.display()))?;
    let app = hop_rip::server::router(hop_rip::server::App { store, base_url });
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| format!("bind {addr}: {e}"))?;
    eprintln!("hop: serving {} on http://{addr}", data.display());
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Serve { data, addr, base_url } => {
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(serve(data, addr, base_url))
        }
        Cmd::New { url, slug, remote } => {
            let v = call(ureq::post(&remote.url("/api/links")), Some(json!({ "url": url, "slug": slug })))?;
            let slug = v["slug"].as_str().unwrap_or_default();
            let key = v["edit_key"].as_str().unwrap_or_default();
            let mut keys = load_keys();
            keys.insert(key_id(&remote, slug), key.to_string());
            let saved = save_keys(&keys).is_ok();
            println!("{}", v["short_url"].as_str().unwrap_or_default());
            eprintln!("edit key: {key}{}", if saved { format!(" (saved in {})", keys_path().display()) } else { String::new() });
            Ok(())
        }
        Cmd::Get { slug, remote } => {
            let v = call(ureq::get(&remote.url(&format!("/api/links/{slug}"))), None)?;
            println!("{} -> {}", v["short_url"].as_str().unwrap_or_default(), v["url"].as_str().unwrap_or_default());
            Ok(())
        }
        Cmd::Edit { slug, url, remote, key } => {
            let key = key_for(&remote, &slug, key)?;
            let req = ureq::put(&remote.url(&format!("/api/links/{slug}"))).set("Authorization", &format!("Bearer {key}"));
            let v = call(req, Some(json!({ "url": url })))?;
            println!("{} -> {}", v["short_url"].as_str().unwrap_or_default(), v["url"].as_str().unwrap_or_default());
            Ok(())
        }
        Cmd::Rm { slug, remote, key } => {
            let key = key_for(&remote, &slug, key)?;
            let req = ureq::delete(&remote.url(&format!("/api/links/{slug}"))).set("Authorization", &format!("Bearer {key}"));
            call(req, None)?;
            let mut keys = load_keys();
            if keys.remove(&key_id(&remote, &slug)).is_some() {
                let _ = save_keys(&keys);
            }
            println!("deleted {slug}");
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hop: {e}");
            ExitCode::FAILURE
        }
    }
}
