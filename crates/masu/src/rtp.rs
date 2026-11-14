//! RPG Maker's run time packages (RTPs) for sake's C:\ (docs/rtp.md, #155 #232): each server
//! downloads the official packages from their publisher (cdn.tkool.jp; free to download, not
//! to redistribute, so they are in no release and nowhere on GitHub) and installs them as their
//! setups would: the files where the setup puts them, sake's registry pointing there (sake
//! misc.rs RTPS). The setups are not run: Inno Setup ones (VX Ace, VX, XP) are read with the
//! `inno` crate (their default folder and files as they declare them), the InstallShield ones
//! (2000, 2003) with iscab.rs.
//!
//! `masu rtp [--out DIR]` installs them into DIR (default: the data directory's rtp/) as
//! `drive_c/...`; `masu serve` does it in the background at start (MASU_RTP=0: not) and then
//! puts them in the store's C:\ (pack::ensure_prefix). Downloads are kept in `<cache>/rtp/`.

use anyhow::{bail, ensure, Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How a package's setup carries its files.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Setup {
    Inno,
    /// an InstallShield setup in a cabinet self-extractor: (files' folder in the setup, the
    /// install folder under drive_c)
    InstallShield { from: &'static str, to: &'static str },
}

pub struct Package {
    pub id: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub setup: Setup,
}

const CDN: &str = "https://cdn.tkool.jp/updata/rtp/";

/// The official packages (https://rpgmakerofficial.com/support/rtp/, 2026-11-14).
pub const PACKAGES: &[Package] = &[
    Package { id: "rpgvxace", url: "vxace_rtp100.zip", sha256: "b3dad56dc143ed3ccd911f73a706a5707484e70c89dca67a55a70c3fabe00730", size: 194_718_847, setup: Setup::Inno },
    Package { id: "rpgvx", url: "vx_rtp202.zip", sha256: "ac668cd1ea09abd6fcd66fc67f8203a2ae8a0c4e696389241549973dcdde42d0", size: 36_819_395, setup: Setup::Inno },
    Package { id: "rpgxp", url: "xp_rtp103.zip", sha256: "96f00a114c541f527171b4f581cba1b3aa321468e3417e68e6cd6edecb95736f", size: 21_525_955, setup: Setup::Inno },
    Package {
        id: "rpg2000",
        url: "2000rtp.zip",
        sha256: "c130f0078c0d6ecc3404f94af8258e4557ab85c3b204397c7f21dbc57efeef8e",
        size: 12_249_974,
        setup: Setup::InstallShield { from: "RTP", to: "Program Files/ASCII/RPG2000/RTP" },
    },
    Package {
        id: "rpg2003",
        url: "2003rtp.zip",
        sha256: "d388b183cc3a8206db53f58db4ea88c6661c9cf289c03aea1bf9ccd425f49cd1",
        size: 15_298_011,
        setup: Setup::InstallShield { from: "RTP", to: "Program Files/Enterbrain/RPG2003/RTP" },
    },
];

/// The publisher's fixed pictures for the 2000 RTP (the support page: モンスター.png into
/// FaceSet, 主人公3.png into CharSet, the rest into ChipSet).
pub const PATCH_2000: Package =
    Package { id: "rpg2000-patch", url: "RTP_patchk_190612.zip", sha256: "3041c57af04675b990da5709928fe7794cac2e4b21f6b526a81d310b353db3ad", size: 222_682, setup: Setup::Inno };

/// Where a setup's folder constant points under drive_c (sake is a 32-bit Windows).
fn constant(c: &str) -> Option<&'static str> {
    Some(match c.to_ascii_lowercase().as_str() {
        "cf" | "commoncf" | "cf32" | "commoncf32" => "Program Files/Common Files",
        "pf" | "commonpf" | "pf32" | "commonpf32" => "Program Files",
        "sys" | "syswow64" => "windows/system32",
        "win" => "windows",
        _ => return None,
    })
}

/// A setup path ("{app}\Graphics\x.png", "{cf}\Enterbrain\...") under drive_c, with `app` the
/// setup's folder there; None for what does not go into drive_c (temporary files, ...).
pub fn map_path(path: &str, app: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let rest = path.strip_prefix('{')?;
    let (c, tail) = rest.split_once('}')?;
    let base = if c.eq_ignore_ascii_case("app") { app.to_string() } else { constant(c)?.to_string() };
    let tail = tail.trim_start_matches('/');
    let parts: Vec<&str> = tail.split('/').filter(|s| !s.is_empty()).collect();
    if parts.iter().any(|s| *s == ".." || *s == "." || s.contains(':')) {
        return None;
    }
    Some(if parts.is_empty() { base } else { format!("{base}/{}", parts.join("/")) })
}

/// Downloads `p` into `cache` (kept; checked against its SHA-256) and returns its path.
fn download(p: &Package, cache: &Path) -> Result<PathBuf> {
    fs::create_dir_all(cache)?;
    let to = cache.join(p.url);
    if to.exists() && sha256_file(&to)? == p.sha256 {
        return Ok(to);
    }
    let url = format!("{CDN}{}", p.url);
    eprintln!("rtp: downloading {url}");
    let tmp = to.with_extension("part");
    let mut last = None;
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2 << attempt));
        }
        let r = (|| -> Result<()> {
            let resp = ureq::get(&url).timeout(std::time::Duration::from_secs(1800)).call()?;
            let mut f = fs::File::create(&tmp)?;
            std::io::copy(&mut resp.into_reader().take(p.size + (1 << 20)), &mut f)?;
            Ok(())
        })();
        match r {
            Ok(()) => {
                let got = sha256_file(&tmp)?;
                if got == p.sha256 {
                    fs::rename(&tmp, &to)?;
                    return Ok(to);
                }
                last = Some(anyhow::anyhow!("{url}: SHA-256 {got}, expected {}", p.sha256));
            }
            Err(e) => last = Some(e),
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(last.unwrap())
}

fn sha256_file(p: &Path) -> Result<String> {
    let mut f = fs::File::open(p)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// The zip's files into `dir` (names in Shift-JIS when not marked UTF-8); their paths.
fn unzip(zip: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    let mut a = zip::ZipArchive::new(fs::File::open(zip)?)?;
    let mut out = Vec::new();
    for i in 0..a.len() {
        let mut e = a.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let raw = e.name_raw().to_vec();
        let name = match std::str::from_utf8(&raw) {
            Ok(s) if raw.is_ascii() => s.to_string(),
            _ => encoding_rs::SHIFT_JIS.decode(&raw).0.into_owned(),
        };
        let base = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
        ensure!(!base.is_empty() && base != ".." && base != ".", "odd name in {}: {name}", zip.display());
        let to = dir.join(&base);
        let mut f = fs::File::create(&to)?;
        std::io::copy(&mut e, &mut f)?;
        out.push(to);
    }
    Ok(out)
}

/// Writes `data` at `root`/`rel` (a drive_c path).
fn put(root: &Path, rel: &str, data: &[u8]) -> Result<()> {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(p, data)?;
    Ok(())
}

/// An Inno Setup RTP: its files under its default folder (and the system folder), as its
/// setup installs them. Returns the install folder (drive_c relative).
fn install_inno(exe: &Path, drive_c: &Path) -> Result<String> {
    let mut inno = inno::Inno::open(exe).map_err(|e| anyhow::anyhow!("{}: {e}", exe.display()))?;
    let dir = inno.header().default_dir_name().context("the setup names no folder")?.to_string();
    let app = map_path(&dir, "").with_context(|| format!("setup folder {dir}"))?;
    let mut n = 0;
    for f in inno.files() {
        let (e, data) = f.map_err(|e| anyhow::anyhow!("{}: {e}", exe.display()))?;
        let Some(dest) = e.file().destination() else { continue };
        let Some(rel) = map_path(dest, &app) else {
            eprintln!("rtp: {}: {dest} skipped", exe.display());
            continue;
        };
        put(drive_c, &rel, &data)?;
        n += 1;
    }
    ensure!(n > 0, "{}: no files", exe.display());
    Ok(app)
}

/// An InstallShield RTP: the cabinet out of the self-extractor, then the files under `from`
/// in the setup into `to`.
fn install_installshield(exe: &Path, from: &str, to: &str, drive_c: &Path, work: &Path) -> Result<()> {
    let b = fs::read(exe)?;
    // (the stub's code can hold the bytes "MSCF" too: a real header has zero reserved fields and version 1.3)
    let at = (0..b.len().saturating_sub(36))
        .find(|&p| &b[p..p + 4] == b"MSCF" && b[p + 4..p + 8] == [0; 4] && b[p + 12..p + 16] == [0; 4] && b[p + 24..p + 26] == [3, 1])
        .context("no cabinet in the self-extractor")?;
    let mut cab = cab::Cabinet::new(std::io::Cursor::new(&b[at..]))?;
    let names: Vec<String> = cab.folder_entries().flat_map(|f| f.file_entries().map(|e| e.name().to_string())).collect();
    for n in &names {
        let base = n.rsplit(['/', '\\']).next().unwrap_or(n);
        ensure!(!base.is_empty() && base != "..", "odd name in the cabinet: {n}");
        let mut r = cab.read_file(n)?;
        let mut f = fs::File::create(work.join(base))?;
        std::io::copy(&mut r, &mut f)?;
    }
    let hdr = work.join("data1.hdr");
    let hdr = if hdr.exists() { hdr } else { work.join("data1.cab") };
    let c = crate::iscab::Cabinet::open(&hdr)?;
    let mut n = 0;
    for e in &c.entries {
        let dir = e.dir.replace('\\', "/");
        let Some(rel) = below(&dir, from) else { continue };
        let rel = rel.to_string();
        if e.name.contains(['/', '\\', ':']) || rel.split('/').any(|s| s == "..") {
            bail!("odd path in the setup: {dir}/{}", e.name);
        }
        let path = if rel.is_empty() { format!("{to}/{}", e.name) } else { format!("{to}/{rel}/{}", e.name) };
        put(drive_c, &path, &c.read(e)?)?;
        n += 1;
    }
    ensure!(n > 0, "{}: no files under {from}", exe.display());
    Ok(())
}

/// `dir` below `from` (ASCII case ignored): "" for `from` itself, the rest after "from/",
/// None elsewhere. By `get`: a multi-byte character across `from`'s length in a name from the
/// cabinet is no match, where slicing it panicked (review b01).
fn below<'a>(dir: &'a str, from: &str) -> Option<&'a str> {
    let head = dir.get(..from.len())?;
    if !head.eq_ignore_ascii_case(from) {
        return None;
    }
    let rest = &dir[from.len()..];
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix('/')
    }
}

/// The 2000 RTP's fixed pictures over the installed ones.
fn patch_2000(zip: &Path, drive_c: &Path, work: &Path) -> Result<()> {
    let rtp = "Program Files/ASCII/RPG2000/RTP";
    for f in unzip(zip, work)? {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let folder = match name.as_str() {
            "モンスター.png" => "FaceSet",
            "主人公3.png" => "CharSet",
            "基本.png" | "ダンジョン.png" | "船.png" | "内装.png" | "外観.png" => "ChipSet",
            _ => continue,
        };
        put(drive_c, &format!("{rtp}/{folder}/{name}"), &fs::read(&f)?)?;
    }
    Ok(())
}

/// Installs every package into `out`/drive_c (made anew; downloads kept in `cache`). Returns
/// the packages installed. A package that fails is reported and left out.
pub fn install_all(out: &Path, cache: &Path) -> Result<Vec<&'static str>> {
    let next = out.with_extension(format!("new.{}", std::process::id()));
    let _ = fs::remove_dir_all(&next);
    let drive_c = next.join("drive_c");
    fs::create_dir_all(&drive_c)?;
    let mut done = Vec::new();
    for p in PACKAGES {
        let work = next.join(format!(".work-{}", p.id));
        fs::create_dir_all(&work)?;
        let r = (|| -> Result<()> {
            let zip = download(p, cache)?;
            let files = unzip(&zip, &work)?;
            let exe = files.iter().find(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))).context("no setup in the zip")?;
            match p.setup {
                Setup::Inno => {
                    install_inno(exe, &drive_c)?;
                }
                Setup::InstallShield { from, to } => {
                    let cab = work.join("cab");
                    fs::create_dir_all(&cab)?;
                    install_installshield(exe, from, to, &drive_c, &cab)?;
                    if p.id == "rpg2000" {
                        let w = work.join("patch");
                        fs::create_dir_all(&w)?;
                        patch_2000(&download(&PATCH_2000, cache)?, &drive_c, &w)?;
                    }
                }
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(&work);
        match r {
            Ok(()) => done.push(p.id),
            Err(e) => eprintln!("rtp: {}: {e:#}", p.id),
        }
    }
    ensure!(!done.is_empty(), "no RTP installed");
    fs::write(next.join("INSTALLED"), done.join("\n") + "\n")?;
    let _ = fs::remove_dir_all(out);
    fs::rename(&next, out)?;
    Ok(done)
}

/// How long after an install that left packages out the server tries again (review b01: a
/// package whose download always fails was fetched again, up to 4 times ~195 MB, and the
/// others installed again, at every start).
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(86_400);

/// Whether the server is to install the RTPs now: not all are in `out`, and the last try that
/// left some out (its stamp, `out`.tried) is [`RETRY_AFTER`] old or more.
pub fn due(out: &Path, now: std::time::SystemTime) -> bool {
    !complete(out) && tried_at(out).is_none_or(|t| now.duration_since(t).map_or(true, |d| d >= RETRY_AFTER))
}

fn tried_path(out: &Path) -> PathBuf {
    out.with_extension("tried")
}

fn tried_at(out: &Path) -> Option<std::time::SystemTime> {
    let s = fs::read_to_string(tried_path(out)).ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(s.trim().parse().ok()?))
}

/// Records a try at `now` (called before it starts: a try cut short by a restart counts too).
pub fn mark_tried(out: &Path, now: std::time::SystemTime) -> Result<()> {
    let secs = now.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Some(p) = out.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(tried_path(out), format!("{secs}\n"))?;
    Ok(())
}

/// Whether `out` has every package installed (what `INSTALLED` lists).
pub fn complete(out: &Path) -> bool {
    let Ok(s) = fs::read_to_string(out.join("INSTALLED")) else { return false };
    PACKAGES.iter().all(|p| s.lines().any(|l| l == p.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_the_setup_folder() {
        assert_eq!(below("Program Files/RTP", "program files/rtp"), Some(""));
        assert_eq!(below("Program Files/RTP/System", "Program Files/RTP"), Some("System"));
        assert_eq!(below("Program Files/RTPX", "Program Files/RTP"), None);
        assert_eq!(below("Prog", "Program Files/RTP"), None);
        // (a multi-byte character across the length: no match, no panic)
        assert_eq!(below("ab日本", "abc"), None);
        assert_eq!(below("日本語/x", "日本語"), Some("x"));
    }

    proptest::proptest! {
        #[test]
        fn below_never_panics(dir in "\\PC{0,8}", from in "[a-zA-Z/]{0,6}") {
            if let Some(rel) = below(&dir, &from) {
                proptest::prop_assert!(dir.len() >= from.len() && dir[..from.len()].eq_ignore_ascii_case(&from));
                proptest::prop_assert!(dir.ends_with(rel));
                proptest::prop_assert!(rel.is_empty() && dir.len() == from.len() || dir.len() == from.len() + 1 + rel.len());
            }
        }

        /// A try that left packages out waits RETRY_AFTER before the next, whatever the clock
        /// says; a complete install is never due.
        #[test]
        fn retries_wait(after in 0u64..3 * 86_400, back in proptest::bool::ANY) {
            let d = std::env::temp_dir().join(format!("masu-rtp-due-{}", uuid::Uuid::new_v4()));
            let out = d.join("rtp");
            let t0 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
            proptest::prop_assert!(due(&out, t0), "never tried");
            mark_tried(&out, t0).unwrap();
            let now = if back { t0 - std::time::Duration::from_secs(after) } else { t0 + std::time::Duration::from_secs(after) };
            // (a clock set back: tried "in the future", so due now: never stuck)
            let want = back && after > 0 || !back && after >= RETRY_AFTER.as_secs();
            proptest::prop_assert_eq!(due(&out, now), want);
            fs::create_dir_all(&out).unwrap();
            fs::write(out.join("INSTALLED"), PACKAGES.iter().map(|p| p.id).collect::<Vec<_>>().join("\n")).unwrap();
            proptest::prop_assert!(!due(&out, now + RETRY_AFTER));
            let _ = fs::remove_dir_all(&d);
        }
    }
    use proptest::prelude::*;

    #[test]
    fn setup_paths() {
        let app = map_path("{cf}\\Enterbrain\\RGSS3\\RPGVXAce", "").unwrap();
        assert_eq!(app, "Program Files/Common Files/Enterbrain/RGSS3/RPGVXAce");
        assert_eq!(map_path("{app}\\Graphics\\System\\Window.png", &app).unwrap(), format!("{app}/Graphics/System/Window.png"));
        assert_eq!(map_path("{sys}\\RGSS102J.dll", &app).unwrap(), "windows/system32/RGSS102J.dll");
        assert_eq!(map_path("{tmp}\\x.dll", &app), None);
        assert_eq!(map_path("{app}\\..\\..\\windows\\x", &app), None);
        assert_eq!(map_path("C:\\x", &app), None);
    }

    proptest! {
        /// Whatever a setup names, the path stays under drive_c: relative, no "..", no drive.
        #[test]
        fn paths_stay_inside(c in "(app|cf|sys|pf|tmp|win|[a-z]{1,4})", parts in proptest::collection::vec("[a-zA-Z.:]{0,4}", 0..5)) {
            let p = format!("{{{c}}}\\{}", parts.join("\\"));
            if let Some(m) = map_path(&p, "Program Files/X") {
                prop_assert!(!m.starts_with('/'));
                prop_assert!(m.split('/').all(|s| !s.is_empty() && s != ".." && s != "." && !s.contains(':')), "{}", m);
            }
        }
    }

    /// Every official package installed as its setup would (network and ~280 MB: run by hand
    /// and in the Corpus workflow, MASU_RTP_TEST=<cache dir>), compared with innoextract and
    /// unshield when they are installed (MASU_RTP_ORACLE=1).
    #[test]
    fn official_packages() {
        let Ok(cache) = std::env::var("MASU_RTP_TEST") else { return };
        let out = std::env::temp_dir().join(format!("masu-rtp-{}", std::process::id()));
        let done = install_all(&out, Path::new(&cache)).unwrap();
        assert_eq!(done.len(), PACKAGES.len(), "{done:?}");
        assert!(complete(&out));
        let c = out.join("drive_c");
        for f in [
            "Program Files/Common Files/Enterbrain/RGSS3/RPGVXAce/Graphics/System/Window.png",
            "Program Files/Common Files/Enterbrain/RGSS3/RPGVXAce/Fonts/VL-Gothic-Regular.ttf",
            "Program Files/Common Files/Enterbrain/RGSS2/RPGVX/RGSS202J.dll",
            "Program Files/Common Files/Enterbrain/RGSS/Standard/Graphics/Windowskins/001-Blue01.png",
            "windows/system32/RGSS102J.dll",
            "Program Files/ASCII/RPG2000/RTP/Harmony.dll",
            "Program Files/ASCII/RPG2000/RTP/ChipSet/基本.png",
            "Program Files/Enterbrain/RPG2003/RTP/Harmony.dll",
        ] {
            assert!(c.join(f).is_file(), "{f}");
        }
        let count = |d: &str| walk(&c.join(d)).len();
        assert_eq!(count("Program Files/Common Files/Enterbrain/RGSS3/RPGVXAce"), 780);
        assert_eq!(count("Program Files/Common Files/Enterbrain/RGSS2/RPGVX"), 444);
        if std::env::var_os("MASU_RTP_ORACLE").is_some() {
            oracle(Path::new(&cache), &c);
        }
        let _ = fs::remove_dir_all(&out);
    }

    fn walk(d: &Path) -> Vec<PathBuf> {
        let mut v = Vec::new();
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            if e.file_type().unwrap().is_dir() {
                v.extend(walk(&e.path()));
            } else {
                v.push(e.path());
            }
        }
        v
    }

    /// The same bytes as innoextract (Inno Setup) and unshield (InstallShield) give.
    fn oracle(cache: &Path, c: &Path) {
        let t = std::env::temp_dir().join(format!("masu-rtp-oracle-{}", std::process::id()));
        for p in PACKAGES {
            let w = t.join(p.id);
            fs::create_dir_all(&w).unwrap();
            let files = unzip(&cache.join(p.url), &w).unwrap();
            let exe = files.iter().find(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))).unwrap();
            let (got, want): (PathBuf, PathBuf) = match p.setup {
                Setup::Inno => {
                    let o = w.join("o");
                    assert!(std::process::Command::new("innoextract").arg("-s").arg("-d").arg(&o).arg(exe).status().unwrap().success());
                    let mut inno = inno::Inno::open(exe).unwrap();
                    let app = map_path(inno.header().default_dir_name().unwrap(), "").unwrap();
                    let _ = inno.files();
                    (c.join(app), o.join("app"))
                }
                Setup::InstallShield { to, .. } => {
                    let o = w.join("o");
                    let cab = w.join("cab");
                    fs::create_dir_all(&cab).unwrap();
                    assert!(std::process::Command::new("7z").arg("x").arg("-y").arg(format!("-o{}", cab.display())).arg(exe).output().unwrap().status.success());
                    assert!(std::process::Command::new("unshield").env("LC_ALL", "C.UTF-8").arg("-D0").arg("-e").arg("cp932").arg("-d").arg(&o).arg("x").arg(cab.join("data1.cab")).output().unwrap().status.success());
                    let group = fs::read_dir(&o).unwrap().flatten().map(|e| e.path()).find(|p| p.join("RTP").is_dir()).unwrap();
                    (c.join(to), group.join("RTP"))
                }
            };
            let mut a: Vec<(String, Vec<u8>)> = walk(&want).into_iter().map(|f| (f.strip_prefix(&want).unwrap().to_string_lossy().into_owned(), fs::read(&f).unwrap())).collect();
            a.sort();
            for (rel, data) in &a {
                // (the 2000 RTP's patched pictures differ from the setup's)
                if p.id == "rpg2000" && ["ChipSet/基本.png", "ChipSet/ダンジョン.png", "ChipSet/船.png", "ChipSet/内装.png", "ChipSet/外観.png", "FaceSet/モンスター.png", "CharSet/主人公3.png"].contains(&rel.as_str()) {
                    continue;
                }
                assert_eq!(&fs::read(got.join(rel)).unwrap_or_default(), data, "{}: {rel}", p.id);
            }
            assert_eq!(walk(&got).len(), a.len(), "{}: file count", p.id);
        }
        let _ = fs::remove_dir_all(&t);
    }
}
