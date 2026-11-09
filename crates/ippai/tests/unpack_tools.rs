//! `ippai add` of a download through the tools (7z, unar, ...), with stand-ins for them on the
//! PATH (the child's PATH only): a tool failing on part of an archive still gives the game, and
//! what failed is kept in the game's entry (unpack_issues), which the game list shows; a tool
//! writing past the caps is stopped and nothing is added.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ippai-unpack-tools-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A stand-in `unar` (`unar -q -f -o OUT SRC`) running `body` in OUT.
fn fake_unar(bin: &Path, body: &str) {
    fs::create_dir_all(bin).unwrap();
    let p = bin.join("unar");
    fs::write(&p, format!("#!/bin/sh\nwhile [ \"$1\" != -o ]; do shift; done\ncd \"$2\" || exit 9\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn ippai(bin: &Path, store: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let mut c = Command::new(env!("CARGO_BIN_EXE_ippai"));
    c.arg("--store").arg(store).args(args).env("PATH", path).env("XDG_CONFIG_HOME", store.join("cfg"));
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

#[test]
fn partly_unpacked_archive_is_added_with_its_issues() {
    let d = tmp("partial");
    let bin = d.join("bin");
    fake_unar(&bin, "mkdir -p game && printf 'MZ game' > game/Game.exe && echo 'game/data.xp3: CRC error, file damaged' >&2 && exit 1");
    fs::write(d.join("trial.rar"), b"Rar!\x1a\x07\x00 not really").unwrap();
    let store = d.join("store");
    let o = ippai(&bin, &store, &["add", d.join("trial.rar").to_str().unwrap(), "--id", "trial", "--no-meta"], &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("unpacked in part"), "{err}");
    let g: serde_json::Value = serde_json::from_slice(&fs::read(store.join("games/trial.json")).unwrap()).unwrap();
    assert_eq!(g["run"], "Z:\\vnvm\\game\\Game.exe");
    let issues = g["unpack_issues"].as_array().expect("unpack_issues");
    assert_eq!(issues.len(), 1, "{issues:?}");
    let i = issues[0].as_str().unwrap();
    assert!(i.starts_with("unar: exit status: 1") && i.contains("game/data.xp3: CRC error"), "{i}");

    // all of it unpacked: no issues, and a re-add clears the old ones
    fake_unar(&bin, "mkdir -p game && printf 'MZ game' > game/Game.exe");
    let o = ippai(&bin, &store, &["add", d.join("trial.rar").to_str().unwrap(), "--id", "trial", "--no-meta"], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let g: serde_json::Value = serde_json::from_slice(&fs::read(store.join("games/trial.json")).unwrap()).unwrap();
    assert!(g.get("unpack_issues").is_none(), "{g}");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn tool_over_the_cap_is_stopped_and_nothing_added() {
    let d = tmp("cap");
    let bin = d.join("bin");
    // 256 MiB from a few bytes: over IPPAI_UNPACK_MAX_GIB=0.01 (10 MiB)
    fake_unar(&bin, "printf 'MZ' > Game.exe; head -c 268435456 /dev/zero > big.bin");
    fs::write(d.join("bomb.7z"), b"7z\xbc\xaf\x27\x1c tiny").unwrap();
    let store = d.join("store");
    let t = std::time::Instant::now();
    let o = ippai(&bin, &store, &["add", d.join("bomb.7z").to_str().unwrap(), "--id", "bomb", "--no-meta"], &[("IPPAI_UNPACK_MAX_GIB", "0.01")]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    assert!(err.contains("more than") && err.contains("IPPAI_UNPACK_MAX_GIB"), "{err}");
    assert!(t.elapsed() < std::time::Duration::from_secs(60));
    assert!(!store.join("games/bomb.json").exists());
    // (the staged files are gone)
    assert!(!store.join("tmp").exists() || fs::read_dir(store.join("tmp")).unwrap().next().is_none());
    fs::remove_dir_all(&d).unwrap();
}

/// `ippai --version`: the version and the commit built (deploy.sh prints it after a deploy).
#[test]
fn version_line_has_the_commit() {
    let o = Command::new(env!("CARGO_BIN_EXE_ippai")).arg("--version").output().unwrap();
    assert!(o.status.success());
    let line = String::from_utf8(o.stdout).unwrap();
    let rest = line.trim().strip_prefix("ippai ").unwrap();
    let (v, c) = rest.split_once(" (").unwrap();
    assert!(!v.is_empty() && !v.contains(' '), "{line}");
    let c = c.strip_suffix(')').unwrap();
    assert!(c.len() == 7 && c.bytes().all(|b| b.is_ascii_hexdigit()), "{line}");
}
