//! The web app (web/dist) built into the binary (rust-embed; a debug build reads web/dist from
//! the checkout instead). The server serves files from a directory (and writes `.zst` beside
//! them), so the first start of a build unpacks them into `<cache>/web/<hash>`. The benchmark
//! pages (bench.html, game.html, benchwasm/) are left out.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(rust_embed::Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
// the benchmark pages are for development (serve them with --web web/dist)
#[exclude = "bench.html"]
#[exclude = "game.html"]
#[exclude = "benchwasm/*"]
#[exclude = "*.zst"]
struct Web;

pub fn unpack(cache: &Path) -> Result<PathBuf> {
    let mut names: Vec<_> = Web::iter().collect();
    if names.is_empty() {
        bail!("this ippai was built without the web app (build web/dist before ippai, or give --web)");
    }
    names.sort();
    let mut h = blake3::Hasher::new();
    for n in &names {
        h.update(n.as_bytes());
        h.update(&Web::get(n).unwrap().metadata.sha256_hash());
    }
    let key = h.finalize().to_hex()[..16].to_string();
    let root = cache.join("web");
    let dir = root.join(&key);
    if !dir.join(".done").exists() {
        let tmp = root.join(format!(".{key}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for n in &names {
            let p = tmp.join(n.as_ref());
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(&p, Web::get(n).unwrap().data).with_context(|| p.display().to_string())?;
        }
        std::fs::write(tmp.join(".done"), "")?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&tmp, &dir).with_context(|| dir.display().to_string())?;
    }
    // older builds' copies
    for e in std::fs::read_dir(&root)?.flatten() {
        let n = e.file_name();
        if n != key.as_str() && !n.to_string_lossy().starts_with('.') {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    Ok(dir)
}
