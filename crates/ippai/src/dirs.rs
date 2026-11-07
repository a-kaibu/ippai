//! Where ippai keeps things, apart from the binary (an update replaces only the binary):
//!
//! ```text
//! Linux                         macOS                                  what
//! ~/.local/share/ippai/store    ~/Library/Application Support/ippai/store  chunks/ trees/ games/ saves
//! ~/.cache/ippai/jit            ~/Library/Caches/ippai/jit             saved x86 regions (wasm)
//! ~/.cache/ippai/web            ~/Library/Caches/ippai/web             the built-in web app, unpacked
//! ~/.local/state/ippai/logs     <data>/logs                            server.log, the pages' run logs
//! ~/.config/ippai/config.toml   ~/Library/Application Support/ippai/config.toml
//! ~/.config/ippai/token         ~/Library/Application Support/ippai/token  other devices' key (auth.rs)
//! $XDG_RUNTIME_DIR/ippai        <cache>/run (0700)                     server-<store>.sock
//! ```
//!
//! config.toml (all optional): `store = "/data/ippai"`, `cache = "..."`, `logs = "..."`,
//! `listen = "0.0.0.0:8656"`, `trust_loopback = true` (auth.rs). `--store` and serve's flags win over it.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    store: Option<PathBuf>,
    cache: Option<PathBuf>,
    logs: Option<PathBuf>,
    listen: Option<SocketAddr>,
    trust_loopback: Option<bool>,
}

pub struct Dirs {
    pub store: PathBuf,
    /// jit/ and web/ go under it
    pub cache: PathBuf,
    pub logs: PathBuf,
    /// the servers' control sockets
    pub run: PathBuf,
    pub listen: SocketAddr,
    /// the key other devices' browsers sign in with (auth.rs)
    pub token: PathBuf,
    /// requests from this machine need no token (serve's --trust-loopback; auth.rs)
    pub trust_loopback: bool,
}

pub const DEFAULT_LISTEN: SocketAddr = SocketAddr::V4(std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 8656));

impl Dirs {
    pub fn load(store: Option<PathBuf>) -> Result<Dirs> {
        let p = directories::ProjectDirs::from("", "", "ippai").context("no home directory")?;
        let file = p.config_dir().join("config.toml");
        let cfg: Config = match std::fs::read_to_string(&file) {
            Ok(s) => toml::from_str(&s).with_context(|| file.display().to_string())?,
            Err(_) => Config::default(),
        };
        let data = p.data_local_dir().to_path_buf();
        let cache = cfg.cache.unwrap_or_else(|| p.cache_dir().to_path_buf());
        let run = p.runtime_dir().map(|d| d.to_path_buf()).unwrap_or_else(|| cache.join("run"));
        Ok(Dirs {
            store: store.or(cfg.store).unwrap_or_else(|| data.join("store")),
            logs: cfg.logs.unwrap_or_else(|| p.state_dir().unwrap_or(&data).join("logs")),
            listen: cfg.listen.unwrap_or(DEFAULT_LISTEN),
            token: p.config_dir().join("token"),
            trust_loopback: cfg.trust_loopback.unwrap_or(false),
            cache,
            run,
        })
    }

    /// The control socket of the server of this store (one per store: `stop` and `status` with
    /// `--store` find the server of that store).
    pub fn socket(&self) -> PathBuf {
        let store = std::path::absolute(&self.store).unwrap_or_else(|_| self.store.clone());
        let key = blake3::hash(store.as_os_str().as_encoded_bytes()).to_hex();
        self.run.join(format!("server-{}.sock", &key[..12]))
    }

    pub fn server_log(&self) -> PathBuf {
        self.logs.join("server.log")
    }

    /// The runtime directory, made private (the socket stops the server).
    pub fn make_run(&self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&self.run).with_context(|| self.run.display().to_string())?;
        std::fs::set_permissions(&self.run, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
}
