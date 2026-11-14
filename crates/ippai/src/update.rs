//! The server updating itself from GitHub Releases (the list page's 設定 → サーバー → 更新):
//! - `GET /api/update`: `{"current": "0.3.3", "latest": "0.3.4", "newer": true, "supported": true}`
//!   (`latest` null when the releases could not be read, with `error`)
//! - `POST /api/update`: installs the latest release as install.sh does (its
//!   `ippai_<ver>_<os>_<arch>.tar.gz`, checked against the release's checksums.txt; the binary
//!   put in place of this one, the old kept as `<exe>.bak`; the release's fonts in the data
//!   directory), answers `{"installed": "0.3.4"}`, then runs the new binary in this process
//!   (exec, the same arguments: the same PID for systemd, the same socket and listen address).
//!   409 when there is nothing newer, 501 where there are no builds (not Linux).
//!
//! IPPAI_UPDATE_URL: the releases' address (default https://github.com/a-kaibu/ippai/releases):
//! `<url>/latest` redirects to `<url>/tag/v<ver>`, files are under `<url>/download/v<ver>/`.
use anyhow::{bail, ensure, Context, Result};
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const RELEASES: &str = "https://github.com/a-kaibu/ippai/releases";
/// The release API (IPPAI_UPDATE_API overrides it): asset digests when checksums.txt fails.
const API: &str = "https://api.github.com/repos/a-kaibu/ippai/releases";

fn base() -> String {
    std::env::var("IPPAI_UPDATE_URL").unwrap_or_else(|_| RELEASES.into()).trim_end_matches('/').to_string()
}

/// This build's release asset name parts (install.sh's): Linux on x86-64 and ARM64 only.
pub fn target() -> Option<(&'static str, &'static str)> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    (std::env::consts::OS == "linux").then_some(("linux", arch))
}

/// `a` newer than `b`: dotted numbers compared as numbers ("0.3.10" > "0.3.9"), a leading `v`
/// ignored, a missing part as 0; anything after a `-` (a prerelease) is older than without.
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| {
        let s = s.trim().trim_start_matches('v');
        let (core, pre) = s.split_once('-').map_or((s, None), |(c, p)| (c, Some(p.to_string())));
        let mut n: Vec<u64> = core.split('.').map(|x| x.parse().unwrap_or(0)).collect();
        while n.last() == Some(&0) {
            n.pop();
        }
        (n, pre)
    };
    let ((an, ap), (bn, bp)) = (parse(a), parse(b));
    match an.cmp(&bn) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match (ap, bp) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        },
    }
}

/// (a proxy from the environment, but not for this machine: ureq 2 does not read NO_PROXY)
fn agent_for(url: &str) -> ureq::Agent {
    let local = url.split("://").nth(1).is_some_and(|h| h.starts_with("127.") || h.starts_with("localhost") || h.starts_with("[::1]"));
    ureq::AgentBuilder::new().timeout(Duration::from_secs(300)).user_agent(concat!("ippai/", env!("IPPAI_VERSION"))).try_proxy_from_env(!local).build()
}
fn get_url(url: &str) -> std::result::Result<ureq::Response, ureq::Error> {
    agent_for(url).get(url).call()
}

/// The latest release's version (without `v`), from where `<url>/latest` leads.
pub fn latest() -> Result<String> {
    let r = get_url(&format!("{}/latest", base())).context("releases")?;
    let url = r.get_url().to_string();
    let tag = url.rsplit_once("/tag/").map(|x| x.1).context("no release")?;
    let v = tag.trim_start_matches('v');
    ensure!(!v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c)), "odd release tag {tag}");
    Ok(v.to_string())
}

/// Downloads `url` into the file `to` (at most 512 MiB, not held in memory: review b01); its
/// SHA-256 in hex.
fn download_to(url: &str, to: &Path) -> Result<String> {
    use std::io::Write as _;
    let r = get_url(url).with_context(|| url.to_string())?;
    let mut rd = r.into_reader().take((512 << 20) + 1);
    let mut f = std::io::BufWriter::new(std::fs::File::create(to)?);
    let mut h = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 256 << 10];
    let mut n = 0u64;
    loop {
        let k = rd.read(&mut buf)?;
        if k == 0 {
            break;
        }
        n += k as u64;
        ensure!(n <= 512 << 20, "{url}: more than 512 MiB");
        h.update(&buf[..k]);
        f.write_all(&buf[..k])?;
    }
    f.flush()?;
    Ok(h.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

fn download(url: &str) -> Result<Vec<u8>> {
    let r = get_url(url).with_context(|| url.to_string())?;
    let mut b = Vec::new();
    std::io::Read::read_to_end(&mut r.into_reader().take(512 << 20), &mut b)?;
    Ok(b)
}

/// Release `version` installed: the binary at `exe` (the old one kept as `<exe>.bak`), its fonts
/// in `fonts` and its benchmark programs (bench/) beside them. Nothing changes unless the archive matches checksums.txt and holds `ippai`.
pub fn install(version: &str, exe: &Path, fonts: &Path) -> Result<()> {
    let (os, arch) = target().context("there are Linux builds only")?;
    let file = format!("ippai_{version}_{os}_{arch}.tar.gz");
    let dir = format!("{}/download/v{version}", base());
    // (unpacked beside the binary: the renames stay on one file system)
    let parent = exe.parent().context("no directory")?;
    let tmp = parent.join(format!(".ippai-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let r = (|| {
        let archive = tmp.join(".archive.tar.gz");
        let got = download_to(&format!("{dir}/{file}"), &archive)?;
        let want = checksum(&dir, version, &file)?;
        ensure!(want.eq_ignore_ascii_case(&got), "checksum mismatch for {file}");
        let mut a = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(std::fs::File::open(&archive)?)));
        for e in a.entries()? {
            let mut e = e?;
            let p = e.path()?.into_owned();
            // (only plain relative paths)
            if p.components().any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)) {
                bail!("odd path in the archive: {}", p.display());
            }
            e.unpack_in(&tmp)?;
        }
        let new = tmp.join("ippai");
        ensure!(new.is_file(), "no ippai in {file}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
        }
        if exe.exists() {
            std::fs::copy(exe, exe.with_extension("bak"))?;
        }
        // (a new file in place of the running one, not written into it)
        std::fs::rename(&new, exe)?;
        // (and the benchmark's programs beside the fonts: dirs.rs's bench, pack::ensure_bench)
        for (name, to) in [("fonts", fonts.to_path_buf()), ("bench", fonts.with_file_name("bench"))] {
            let f = tmp.join(name);
            if !f.is_dir() {
                continue;
            }
            let next = to.with_extension("new");
            let _ = std::fs::remove_dir_all(&next);
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::rename(&f, &next).or_else(|_| copy_dir(&f, &next))?;
            let _ = std::fs::remove_dir_all(&to);
            std::fs::rename(&next, &to)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

/// `file`'s SHA-256 (hex) from the release's checksums.txt, or when that does not come (GitHub's
/// download host answered 504 for it on worker2, 2026-11-09) from the release API's asset digest.
fn checksum(dir: &str, version: &str, file: &str) -> Result<String> {
    let listed = |sums: &str| sums.lines().find_map(|l| l.strip_suffix(&format!(" {file}")).or_else(|| l.strip_suffix(&format!(" *{file}")))).map(|s| s.trim().to_string());
    let sums = download(&format!("{dir}/checksums.txt")).map(|b| String::from_utf8_lossy(&b).into_owned());
    if let Ok(s) = &sums {
        if let Some(w) = listed(s) {
            return Ok(w);
        }
    }
    let api = std::env::var("IPPAI_UPDATE_API").unwrap_or_else(|_| API.into());
    let rel: serde_json::Value = get_url(&format!("{}/tags/v{version}", api.trim_end_matches('/'))).context("release API")?.into_json()?;
    let digest = rel["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"] == file)).and_then(|x| x["digest"].as_str());
    match digest.and_then(|d| d.strip_prefix("sha256:")) {
        Some(d) => Ok(d.to_string()),
        None => match sums {
            Err(e) => Err(e.context("checksums.txt, and no digest in the release API")),
            Ok(_) => bail!("{file} not in checksums.txt nor the release API"),
        },
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()))?;
        } else {
            std::fs::copy(e.path(), to.join(e.file_name()))?;
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Up {
    exe: PathBuf,
    fonts: PathBuf,
    busy: Arc<tokio::sync::Mutex<()>>,
}

/// What [`latest`] said lately, and when: GitHub asked at most every CHECK_OK (CHECK_ERR after
/// a failure) however often the page asks (review b01: each GET asked, up to 300 s each).
/// (with the releases' address it is for)
static LATEST: std::sync::Mutex<Option<(String, std::time::Instant, std::result::Result<String, String>)>> = std::sync::Mutex::new(None);
const CHECK_OK: Duration = Duration::from_secs(600);
const CHECK_ERR: Duration = Duration::from_secs(60);

/// Whether an answer of `age` is still good (`ok`: it was a version).
fn fresh(age: Duration, ok: bool) -> bool {
    age < if ok { CHECK_OK } else { CHECK_ERR }
}

fn latest_kept() -> std::result::Result<String, String> {
    let at = base();
    if let Some((u, t, r)) = LATEST.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *u == at && fresh(t.elapsed(), r.is_ok()) {
            return r.clone();
        }
    }
    let r = latest().map_err(|e| format!("{e:#}"));
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = Some((at, std::time::Instant::now(), r.clone()));
    r
}

async fn check() -> Json<serde_json::Value> {
    let current = env!("IPPAI_VERSION");
    let l = tokio::task::spawn_blocking(latest_kept).await;
    let (latest, error) = match l {
        Ok(Ok(v)) => (Some(v), None),
        Ok(Err(e)) => (None, Some(e)),
        Err(e) => (None, Some(e.to_string())),
    };
    Json(serde_json::json!({
        "current": current,
        "latest": latest,
        "newer": latest.as_deref().is_some_and(|l| newer(l, current)),
        "supported": target().is_some(),
        "error": error,
    }))
}

async fn update(State(u): State<Up>) -> axum::response::Response {
    if target().is_none() {
        return (StatusCode::NOT_IMPLEMENTED, "この環境のビルドはありません（Linux のみ）").into_response();
    }
    let Ok(_g) = u.busy.try_lock() else { return (StatusCode::CONFLICT, "更新中です").into_response() };
    let (exe, fonts) = (u.exe.clone(), u.fonts.clone());
    let r = tokio::task::spawn_blocking(move || {
        let v = latest()?;
        if !newer(&v, env!("IPPAI_VERSION")) {
            return Ok(None);
        }
        install(&v, &exe, &fonts)?;
        Ok::<_, anyhow::Error>(Some(v))
    })
    .await;
    match r {
        Ok(Ok(Some(v))) => {
            eprintln!("ippai: updated to {v}, restarting");
            restart_soon(u.exe.clone());
            Json(serde_json::json!({ "installed": v })).into_response()
        }
        Ok(Ok(None)) => (StatusCode::CONFLICT, "最新です").into_response(),
        Ok(Err(e)) => {
            eprintln!("ippai: update: {e:#}");
            (StatusCode::BAD_GATEWAY, format!("更新できませんでした: {e:#}")).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// After the answer has gone out: this process becomes the new binary (the same arguments).
fn restart_soon(exe: PathBuf) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        // (writes going on finish first: saves, regions, uploads; review b01, exec cut them)
        if !wait_idle(&WRITES, DRAIN) {
            eprintln!("ippai: restarting with {} writes still going on", WRITES.load(std::sync::atomic::Ordering::SeqCst));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let e = std::process::Command::new(&exe).args(std::env::args_os().skip(1)).exec();
            eprintln!("ippai: restart {}: {e}", exe.display());
        }
        // (elsewhere the binary is not replaced in place: see install)
        #[cfg(not(unix))]
        drop(exe);
        std::process::exit(1);
    });
}

/// Requests that write (any but GET and HEAD) going on now: [`Writing`].
pub static WRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How long a restart waits for them at most.
const DRAIN: Duration = Duration::from_secs(30);

/// A write request's mark in [`WRITES`], for as long as it is handled (serve.rs `headers`).
pub struct Writing(&'static std::sync::atomic::AtomicUsize);

impl Writing {
    pub fn new(n: &'static std::sync::atomic::AtomicUsize) -> Self {
        n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Writing(n)
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Waits until `n` is 0 or `deadline` passed: whether it got to 0.
fn wait_idle(n: &std::sync::atomic::AtomicUsize, deadline: Duration) -> bool {
    let t0 = std::time::Instant::now();
    while n.load(std::sync::atomic::Ordering::SeqCst) > 0 {
        if t0.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// The routes; `exe` is this binary's path (taken at the start: once replaced, /proc/self/exe
/// names a deleted file), `fonts` the data directory's fonts.
pub fn router(exe: PathBuf, fonts: PathBuf) -> Router {
    Router::new().route("/api/update", get(check).post(update)).with_state(Up { exe, fonts, busy: Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn versions() {
        assert!(newer("0.3.4", "0.3.3") && newer("v0.3.10", "0.3.9") && newer("1.0.0", "0.99.99"));
        assert!(!newer("0.3.3", "0.3.3") && !newer("0.3.3", "v0.3.3") && !newer("0.3.2", "0.3.3"));
        assert!(newer("0.4", "0.3.9") && !newer("0.3", "0.3.0") && !newer("0.3.0", "0.3"));
        assert!(newer("0.3.3", "0.3.3-rc1") && !newer("0.3.3-rc1", "0.3.3") && newer("0.3.3-rc2", "0.3.3-rc1"));
    }

    proptest! {
        /// An order: never both ways, never of itself, as the numbers compare.
        #[test]
        fn versions_order(a in proptest::collection::vec(0u64..20, 1..4), b in proptest::collection::vec(0u64..20, 1..4)) {
            let s = |v: &[u64]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
            let (x, y) = (s(&a), s(&b));
            prop_assert!(!(newer(&x, &y) && newer(&y, &x)));
            prop_assert!(!newer(&x, &x));
            let pad = |v: &[u64]| { let mut v = v.to_vec(); v.resize(4, 0); v };
            prop_assert_eq!(newer(&x, &y), pad(&a) > pad(&b));
        }
    }

    /// A release server as GitHub's: /latest redirects to /tag/v<ver>, /download/v<ver>/ has
    /// the archive and checksums.txt (`bad`: a checksum that does not match).
    pub(crate) async fn releases(version: &'static str, tgz: Vec<u8>, bad: bool) -> String {
        use axum::response::Redirect;
        let (os, arch) = target().unwrap();
        let file = format!("ippai_{version}_{os}_{arch}.tar.gz");
        let sum: String = ring::digest::digest(&ring::digest::SHA256, &tgz).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let sum = if bad { "0".repeat(64) } else { sum };
        let sums = format!("{}  other.zip\n{sum}  {file}\n", "1".repeat(64));
        let app = Router::new()
            .route("/r/latest", get(move || async move { Redirect::temporary(&format!("/r/tag/v{version}")) }))
            .route("/r/tag/{t}", get(|| async { "release page" }))
            .route(&format!("/r/download/v{version}/{file}"), get(move || async move { tgz.clone() }))
            .route(&format!("/r/download/v{version}/checksums.txt"), get(move || async move { sums.clone() }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{a}/r")
    }

    pub(crate) fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    /// (IPPAI_UPDATE_URL is the process's: the tests that set it run one at a time)
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The latest release's binary in place of the old (kept as .bak), its fonts in place of
    /// the old fonts; a checksum that does not match or an archive without ippai changes nothing.
    #[tokio::test]
    async fn installs_the_latest_release() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ippai-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("data/fonts")).unwrap();
        let (exe, fonts) = (dir.join("bin/ippai"), dir.join("data/fonts"));
        std::fs::write(&exe, b"old binary").unwrap();
        std::fs::write(fonts.join("old.ttf"), b"old").unwrap();
        let tgz = archive(&[("ippai", b"new binary"), ("README.md", b"r"), ("fonts/msgothic.ttc", b"font"), ("bench/bench.exe", b"MZ"), ("bench/VERSION", b"v9")]);
        std::fs::create_dir_all(dir.join("data/bench")).unwrap();
        std::fs::write(dir.join("data/bench/old.exe"), b"old").unwrap();
        std::env::set_var("IPPAI_UPDATE_URL", releases("9.8.7", tgz.clone(), true).await);
        assert_eq!(tokio::task::spawn_blocking(latest).await.unwrap().unwrap(), "9.8.7");
        let (e2, f2) = (exe.clone(), fonts.clone());
        let r = tokio::task::spawn_blocking(move || install("9.8.7", &e2, &f2)).await.unwrap();
        assert!(format!("{:#}", r.unwrap_err()).contains("checksum mismatch"));
        assert_eq!(std::fs::read(&exe).unwrap(), b"old binary");
        assert!(fonts.join("old.ttf").exists());
        // no ippai in it
        std::env::set_var("IPPAI_UPDATE_URL", releases("9.8.7", archive(&[("README.md", b"r")]), false).await);
        let (e2, f2) = (exe.clone(), fonts.clone());
        assert!(tokio::task::spawn_blocking(move || install("9.8.7", &e2, &f2)).await.unwrap().is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old binary");
        // a path out of the directory
        std::env::set_var("IPPAI_UPDATE_URL", releases("9.8.7", raw_tar_with("../escape", b"x"), false).await);
        let (e2, f2) = (exe.clone(), fonts.clone());
        assert!(tokio::task::spawn_blocking(move || install("9.8.7", &e2, &f2)).await.unwrap().is_err());
        assert!(!dir.join("escape").exists() && !dir.join("bin/../escape").exists());
        // the right one
        std::env::set_var("IPPAI_UPDATE_URL", releases("9.8.7", tgz, false).await);
        let (e2, f2) = (exe.clone(), fonts.clone());
        tokio::task::spawn_blocking(move || install("9.8.7", &e2, &f2)).await.unwrap().unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new binary");
        assert_eq!(std::fs::read(exe.with_extension("bak")).unwrap(), b"old binary");
        assert_eq!(std::fs::read(fonts.join("msgothic.ttc")).unwrap(), b"font");
        assert!(!fonts.join("old.ttf").exists());
        assert_eq!(std::fs::read(dir.join("data/bench/bench.exe")).unwrap(), b"MZ");
        assert!(!dir.join("data/bench/old.exe").exists() && !dir.join("data/bench.new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o755);
        }
        // nothing left of the unpacking
        assert_eq!(std::fs::read_dir(dir.join("bin")).unwrap().count(), 2);
        std::env::remove_var("IPPAI_UPDATE_URL");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// checksums.txt answered 504 (GitHub's download host, worker2 2026-11-09): the release API's
    /// asset digest instead; a digest that does not match, or none, changes nothing.
    #[tokio::test]
    async fn checksums_from_the_release_api_when_the_file_fails() {
        use axum::response::Redirect;
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ippai-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let (exe, fonts) = (dir.join("bin/ippai"), dir.join("fonts"));
        std::fs::write(&exe, b"old binary").unwrap();
        let tgz = archive(&[("ippai", b"new binary")]);
        let (os, arch) = target().unwrap();
        let file = format!("ippai_9.8.8_{os}_{arch}.tar.gz");
        let sum: String = ring::digest::digest(&ring::digest::SHA256, &tgz).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let serve = |digest: Option<String>| {
            let (tgz, file) = (tgz.clone(), file.clone());
            async move {
                let assets = serde_json::json!({"assets": [{"name": "other.zip", "digest": format!("sha256:{}", "1".repeat(64))}, {"name": file, "digest": digest.map(|d| format!("sha256:{d}"))}]});
                let app = Router::new()
                    .route("/r/latest", get(|| async { Redirect::temporary("/r/tag/v9.8.8") }))
                    .route(&format!("/r/download/v9.8.8/{file}"), get(move || async move { tgz.clone() }))
                    .route("/r/download/v9.8.8/checksums.txt", get(|| async { (StatusCode::GATEWAY_TIMEOUT, "504") }))
                    .route("/api/tags/v9.8.8", get(move || async move { Json(assets.clone()) }));
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let a = l.local_addr().unwrap();
                tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
                std::env::set_var("IPPAI_UPDATE_URL", format!("http://{a}/r"));
                std::env::set_var("IPPAI_UPDATE_API", format!("http://{a}/api"));
            }
        };
        let try_install = || {
            let (e2, f2) = (exe.clone(), fonts.clone());
            tokio::task::spawn_blocking(move || install("9.8.8", &e2, &f2))
        };
        serve(Some("0".repeat(64))).await;
        assert!(format!("{:#}", try_install().await.unwrap().unwrap_err()).contains("checksum mismatch"));
        serve(None).await;
        assert!(try_install().await.unwrap().is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old binary");
        serve(Some(sum)).await;
        try_install().await.unwrap().unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new binary");
        std::env::remove_var("IPPAI_UPDATE_URL");
        std::env::remove_var("IPPAI_UPDATE_API");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A gzip'd tar whose one entry's name is written as it is (tar::Builder refuses `..`).
    fn raw_tar_with(name: &str, data: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut t = h.to_vec();
        t.extend_from_slice(data);
        t.resize(t.len().div_ceil(512) * 512 + 1024, 0);
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut z, &t).unwrap();
        z.finish().unwrap()
    }

    /// GET /api/update: this version, the latest and whether it is newer; POST when there is
    /// nothing newer: 409 and nothing changes.
    #[tokio::test]
    async fn checks_and_refuses_what_is_not_newer() {
        use tower::ServiceExt;
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ippai-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("ippai");
        std::fs::write(&exe, b"old").unwrap();
        let app = router(exe.clone(), dir.join("fonts"));
        let call = |m: &str| axum::http::Request::builder().method(m).uri("/api/update").body(axum::body::Body::empty()).unwrap();
        let json = |b: axum::body::Bytes| serde_json::from_slice::<serde_json::Value>(&b).unwrap();
        std::env::set_var("IPPAI_UPDATE_URL", releases("999.0.0", archive(&[("ippai", b"x")]), false).await);
        let r = app.clone().oneshot(call("GET")).await.unwrap();
        let v = json(axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap());
        assert_eq!((v["current"].as_str(), v["latest"].as_str(), v["newer"].as_bool()), (Some(env!("IPPAI_VERSION")), Some("999.0.0"), Some(true)));
        std::env::set_var("IPPAI_UPDATE_URL", releases("0.0.1", archive(&[("ippai", b"x")]), false).await);
        let r = app.clone().oneshot(call("GET")).await.unwrap();
        assert_eq!(json(axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap())["newer"], false);
        let r = app.clone().oneshot(call("POST")).await.unwrap();
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        // releases that cannot be read: no latest, an error
        std::env::set_var("IPPAI_UPDATE_URL", "http://127.0.0.1:9/none");
        let r = app.clone().oneshot(call("GET")).await.unwrap();
        let v = json(axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap());
        assert!(v["latest"].is_null() && v["error"].is_string() && v["newer"] == false);
        std::env::remove_var("IPPAI_UPDATE_URL");
        let _ = std::fs::remove_dir_all(dir);
    }

    proptest! {
        /// A version is kept longer than a failure, and nothing is kept past CHECK_OK.
        #[test]
        fn answers_kept_as_long_as_they_are_good(ms in 0u64..2_000_000, ok: bool) {
            let age = Duration::from_millis(ms);
            prop_assert_eq!(fresh(age, ok), age < if ok { CHECK_OK } else { CHECK_ERR });
            prop_assert!(!fresh(age, false) || fresh(age, true));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// However the write requests start and end on other threads, wait_idle says true only
        /// once none is left, and false when one is held past the deadline.
        #[test]
        fn a_restart_waits_for_writes(holds in proptest::collection::vec(0u64..40, 0..6), stuck: bool) {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let ts: Vec<_> = holds.iter().map(|&ms| { let w = Writing::new(&N); std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(ms)); drop(w) }) }).collect();
            let held = stuck.then(|| { let w = Writing::new(&N); std::thread::spawn(move || { let _ = rx.recv(); drop(w) }) });
            let ok = wait_idle(&N, Duration::from_millis(300));
            prop_assert_eq!(ok, !stuck);
            if ok { prop_assert_eq!(N.load(std::sync::atomic::Ordering::SeqCst), 0); }
            drop(tx);
            for t in ts { t.join().unwrap(); }
            if let Some(t) = held { t.join().unwrap(); }
            prop_assert_eq!(N.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }

    /// The archive goes to the file as it comes, its SHA-256 the file's; more than 512 MiB is
    /// refused (the body is never read whole into memory).
    #[tokio::test(flavor = "multi_thread")]
    async fn downloads_go_to_a_file() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let d2 = data.clone();
        let app = Router::new().route("/a", get(move || async move { d2.clone() })).route("/big", get(|| async { axum::body::Body::from_stream(futures_util::stream::iter((0..513).map(|_| Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![7u8; 1 << 20]))))) }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let dir = std::env::temp_dir().join(format!("ippai-dl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (f, g) = (dir.join("a"), dir.join("big"));
        let (sum, big) = tokio::task::spawn_blocking(move || (download_to(&format!("http://{at}/a"), &f).map(|s| (s, std::fs::read(&f).unwrap())), download_to(&format!("http://{at}/big"), &g))).await.unwrap();
        let (sum, got) = sum.unwrap();
        assert_eq!(got, data);
        let want: String = ring::digest::digest(&ring::digest::SHA256, &data).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(sum, want);
        assert!(format!("{:#}", big.unwrap_err()).contains("512 MiB"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Checks asked often reach the releases once while the answer is fresh; a new releases
    /// address is asked again.
    #[tokio::test(flavor = "multi_thread")]
    async fn checks_are_kept() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        let app = Router::new()
            .route("/r/latest", get(move || { h.fetch_add(1, std::sync::atomic::Ordering::SeqCst); async { axum::response::Redirect::temporary("/r/tag/v5.4.3") } }))
            .route("/r/tag/{t}", get(|| async { "page" }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        std::env::set_var("IPPAI_UPDATE_URL", format!("http://{at}/r"));
        let rs = tokio::task::spawn_blocking(|| (0..20).map(|_| latest_kept()).collect::<Vec<_>>()).await.unwrap();
        assert!(rs.iter().all(|r| r.as_deref() == Ok("5.4.3")), "{rs:?}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        std::env::set_var("IPPAI_UPDATE_URL", format!("http://{at}/r/"));
        tokio::task::spawn_blocking(latest_kept).await.unwrap().unwrap();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "the same address, written with a slash");
        std::env::set_var("IPPAI_UPDATE_URL", format!("http://localhost:{}/r", at.port()));
        tokio::task::spawn_blocking(latest_kept).await.unwrap().unwrap();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
        std::env::remove_var("IPPAI_UPDATE_URL");
    }
}
