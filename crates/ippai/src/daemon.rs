//! The server in the background: `ippai serve` starts `ippai serve --foreground` detached, its
//! output appended to `<logs>/server.log`, and returns once it answers on its control socket
//! (`<run>/server.sock`). `stop` and `status` ask the server there (one line: `stop`, `status`).

use crate::dirs::Dirs;
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

/// What the server answers to `status`.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub pid: u32,
    pub url: String,
    pub version: String,
    pub store: String,
}

/// Asks the running server (None: none answers).
fn ask(dirs: &Dirs, cmd: &str) -> Option<String> {
    let mut s = UnixStream::connect(dirs.socket()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(s, "{cmd}").ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    Some(line)
}

pub fn running(dirs: &Dirs) -> Option<Status> {
    serde_json::from_str(&ask(dirs, "status")?).ok()
}

pub fn start(dirs: &Dirs, a: &crate::serve::Args) -> Result<()> {
    if let Some(s) = running(dirs) {
        println!("already running: {} (pid {})", s.url, s.pid);
        return Ok(());
    }
    let logs = a.logs.clone().unwrap_or_else(|| dirs.logs.clone());
    std::fs::create_dir_all(&logs).with_context(|| logs.display().to_string())?;
    let log_path = logs.join("server.log");
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).with_context(|| log_path.display().to_string())?;
    let from = log.metadata()?.len();
    // the same command line, kept in this process
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(std::env::args_os().skip(1))
        .arg("--foreground")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    let t0 = Instant::now();
    // (the first start of a build unpacks the web app and compresses it)
    while t0.elapsed() < Duration::from_secs(120) {
        if let Some(s) = running(dirs) {
            println!("{}", s.url);
            return Ok(());
        }
        if let Some(st) = child.try_wait()? {
            print_from(&log_path, from);
            bail!("the server stopped ({st}); log: {}", log_path.display());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("the server did not answer in 120 s (pid {}); log: {}", child.id(), log_path.display())
}

pub fn stop(dirs: &Dirs) -> Result<()> {
    let Some(s) = running(dirs) else {
        println!("not running");
        return Ok(());
    };
    ask(dirs, "stop");
    let t0 = Instant::now();
    while running(dirs).is_some() {
        if t0.elapsed() > Duration::from_secs(10) {
            bail!("pid {} did not stop", s.pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("stopped (pid {})", s.pid);
    Ok(())
}

pub fn status(dirs: &Dirs) -> Result<()> {
    match running(dirs) {
        Some(s) => {
            println!("running: {}\npid {}, {}\nstore {}\nlog {}", s.url, s.pid, s.version, s.store, dirs.server_log().display());
            Ok(())
        }
        None => {
            println!("not running");
            std::process::exit(3)
        }
    }
}

pub fn logs(dirs: &Dirs, follow: bool, lines: usize) -> Result<()> {
    let path = dirs.server_log();
    let mut f = std::fs::File::open(&path).with_context(|| format!("{} (the server has not run yet?)", path.display()))?;
    let len = f.metadata()?.len();
    // the last `lines` lines: read back from the end in steps
    let mut start = len;
    let mut tail = Vec::new();
    while start > 0 && tail.iter().filter(|&&b| b == b'\n').count() <= lines {
        let step = start.min(64 << 10);
        start -= step;
        f.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0; step as usize];
        f.read_exact(&mut buf)?;
        buf.extend_from_slice(&tail);
        tail = buf;
    }
    let newlines: Vec<usize> = tail.iter().enumerate().filter(|(_, &b)| b == b'\n').map(|(i, _)| i).collect();
    let cut = if newlines.len() > lines { newlines[newlines.len() - lines - 1] + 1 } else { 0 };
    std::io::stdout().write_all(&tail[cut..])?;
    if !follow {
        return Ok(());
    }
    let mut pos = len;
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let n = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if n < pos {
            pos = 0; // (truncated)
        }
        if n > pos {
            f = std::fs::File::open(&path)?;
            f.seek(SeekFrom::Start(pos))?;
            std::io::copy(&mut (&mut f).take(n - pos), &mut std::io::stdout())?;
            pos = n;
        }
    }
}

fn print_from(path: &std::path::Path, from: u64) {
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.seek(SeekFrom::Start(from));
        let _ = std::io::copy(&mut f, &mut std::io::stderr());
    }
}

/// The server's side of the socket: answers `status`, exits on `stop`. When another server
/// answers there already (a second one on another port and store: tests run several), this
/// one goes without (stop it with a signal).
pub async fn listen(dirs: &Dirs, url: String, store: String) -> Result<()> {
    if let Some(s) = running(dirs) {
        eprintln!("ippai: {} (pid {}) has the control socket: `ippai stop` and `status` will not see this server", s.url, s.pid);
        return Ok(());
    }
    dirs.make_run()?;
    let sock = dirs.socket();
    let _ = std::fs::remove_file(&sock); // (left by one that was killed)
    let l = tokio::net::UnixListener::bind(&sock).with_context(|| sock.display().to_string())?;
    let status = serde_json::to_string(&Status { pid: std::process::id(), url, version: env!("IPPAI_VERSION").into(), store })?;
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        while let Ok((s, _)) = l.accept().await {
            let (r, mut w) = s.into_split();
            let mut line = String::new();
            if tokio::io::BufReader::new(r).read_line(&mut line).await.is_err() {
                continue;
            }
            match line.trim() {
                "status" => {
                    let _ = w.write_all(format!("{status}\n").as_bytes()).await;
                }
                "stop" => {
                    let _ = w.write_all(b"ok\n").await;
                    eprintln!("ippai: stop");
                    let _ = std::fs::remove_file(&sock);
                    std::process::exit(0);
                }
                _ => {}
            }
        }
    });
    Ok(())
}
