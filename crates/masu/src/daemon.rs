//! The server in the background: `masu serve` starts `masu serve --foreground` detached, its
//! output appended to `<logs>/server.log`, and returns once it answers on its control socket
//! (`<run>/server-<store>.sock`, one per store: see Dirs::socket). `stop` and `status` ask the
//! server there (one line: `stop`, `status`).

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
    rotate(&log_path, LOG_KEEP);
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

/// server.log past this at a start becomes server.log.1 (the one before that goes): a server
/// started for months appended every run to one file for ever
const LOG_KEEP: u64 = 32 << 20;

fn rotate(path: &std::path::Path, keep: u64) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > keep) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(path, old);
    }
}

/// The last `lines` lines of what `f` holds (`len` bytes), read back from the end in steps of
/// `step` bytes, each byte read once
fn tail_of(f: &mut (impl Read + Seek), len: u64, lines: usize, step: u64) -> std::io::Result<Vec<u8>> {
    let mut start = len;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut newlines = 0;
    while start > 0 && newlines <= lines {
        let n = start.min(step);
        start -= n;
        f.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0; n as usize];
        f.read_exact(&mut buf)?;
        newlines += buf.iter().filter(|&&b| b == b'\n').count();
        chunks.push(buf);
    }
    let tail: Vec<u8> = chunks.into_iter().rev().flatten().collect();
    let nl: Vec<usize> = tail.iter().enumerate().filter(|(_, &b)| b == b'\n').map(|(i, _)| i).collect();
    let cut = if nl.len() > lines { nl[nl.len() - lines - 1] + 1 } else { 0 };
    Ok(tail[cut..].to_vec())
}

pub fn logs(dirs: &Dirs, follow: bool, lines: usize) -> Result<()> {
    let path = dirs.server_log();
    let mut f = std::fs::File::open(&path).with_context(|| format!("{} (the server has not run yet?)", path.display()))?;
    let len = f.metadata()?.len();
    // the last `lines` lines: read back from the end in steps
    std::io::stdout().write_all(&tail_of(&mut f, len, lines, 64 << 10)?)?;
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

/// The server's side of the socket: answers `status`, exits on `stop`. A second server for
/// the same store does not start (servers of other stores have sockets of their own).
pub async fn listen(dirs: &Dirs, url: String, store: String) -> Result<()> {
    if let Some(s) = running(dirs) {
        bail!("{} (pid {}) already serves this store: `masu stop` it first, or give another --store", s.url, s.pid);
    }
    dirs.make_run()?;
    let sock = dirs.socket();
    let _ = std::fs::remove_file(&sock); // (left by one that was killed)
    let l = tokio::net::UnixListener::bind(&sock).with_context(|| sock.display().to_string())?;
    let status = serde_json::to_string(&Status { pid: std::process::id(), url, version: env!("MASU_VERSION").into(), store })?;
    tokio::spawn(control(l, status, std::sync::Arc::new(move || {
        eprintln!("masu: stop");
        let _ = std::fs::remove_file(&sock);
        std::process::exit(0);
    })));
    Ok(())
}

/// How long a control client may take to send its line
const CONTROL_WAIT: Duration = Duration::from_secs(5);

/// Answers the control socket's clients, each on its own task: a client that connects and says
/// nothing (a suspended `masu status`, `nc -U`) or sends no newline is dropped after
/// CONTROL_WAIT or 64 bytes, and holds up no other (it held the one accept loop for good: status
/// said "not running", stop could not stop the server and a second one started on the store).
async fn control(l: tokio::net::UnixListener, status: String, stop: std::sync::Arc<dyn Fn() + Send + Sync>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let status = std::sync::Arc::new(status);
    while let Ok((s, _)) = l.accept().await {
        let (status, stop) = (status.clone(), stop.clone());
        tokio::spawn(async move {
            let (r, mut w) = s.into_split();
            let mut line = String::new();
            let mut r = tokio::io::BufReader::new(r.take(64));
            if !matches!(tokio::time::timeout(CONTROL_WAIT, r.read_line(&mut line)).await, Ok(Ok(_))) {
                return;
            }
            match line.trim() {
                "status" => {
                    let _ = tokio::time::timeout(CONTROL_WAIT, w.write_all(format!("{status}\n").as_bytes())).await;
                }
                "stop" => {
                    let _ = tokio::time::timeout(CONTROL_WAIT, w.write_all(b"ok\n")).await;
                    stop();
                }
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Clone, Copy, Debug)]
    enum Client {
        /// connects and says nothing, stays connected
        Silent,
        /// sends bytes with no newline
        Garbage(usize),
        /// half a line, then nothing
        Partial,
        Status,
        Stop,
    }

    fn client() -> impl Strategy<Value = Client> {
        prop_oneof![Just(Client::Silent), (1usize..4096).prop_map(Client::Garbage), Just(Client::Partial), Just(Client::Status), Just(Client::Stop)]
    }

    /// Any mix of clients in any order: every `status` is answered at once and every `stop`
    /// stops, whatever silent or garbage clients came before (review b00: one silent client
    /// held the loop for good)
    fn run(clients: Vec<Client>) -> Result<(), TestCaseError> {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let dir = std::env::temp_dir().join(format!("masu-ctl-{}-{:?}", std::process::id(), std::thread::current().id()));
            let _ = std::fs::create_dir_all(&dir);
            let sock = dir.join("s.sock");
            let _ = std::fs::remove_file(&sock);
            let l = tokio::net::UnixListener::bind(&sock).unwrap();
            let stops = Arc::new(AtomicUsize::new(0));
            let s2 = stops.clone();
            let task = tokio::spawn(control(l, "{\"pid\":1}".into(), Arc::new(move || {
                s2.fetch_add(1, Ordering::SeqCst);
            })));
            let mut held = Vec::new();
            let mut want_stops = 0;
            for c in clients {
                let mut s = tokio::net::UnixStream::connect(&sock).await.unwrap();
                match c {
                    Client::Silent => held.push(s),
                    Client::Garbage(n) => {
                        let _ = s.write_all(&vec![b'x'; n]).await;
                        held.push(s);
                    }
                    Client::Partial => {
                        let _ = s.write_all(b"sta").await;
                        held.push(s);
                    }
                    Client::Status | Client::Stop => {
                        let stop = matches!(c, Client::Stop);
                        s.write_all(if stop { b"stop\n" } else { b"status\n" }).await.unwrap();
                        let mut line = String::new();
                        let r = tokio::time::timeout(Duration::from_secs(2), tokio::io::BufReader::new(&mut s).read_line(&mut line)).await;
                        prop_assert!(matches!(r, Ok(Ok(_))), "{c:?} not answered");
                        prop_assert_eq!(line.trim(), if stop { "ok" } else { "{\"pid\":1}" });
                        if stop {
                            want_stops += 1;
                            let t = Instant::now();
                            while stops.load(Ordering::SeqCst) < want_stops && t.elapsed() < Duration::from_secs(2) {
                                tokio::task::yield_now().await;
                            }
                            prop_assert_eq!(stops.load(Ordering::SeqCst), want_stops);
                        }
                    }
                }
            }
            task.abort();
            drop(held);
            let _ = std::fs::remove_dir_all(&dir);
            Ok(())
        })
    }

    #[test]
    fn a_silent_client_holds_up_no_other() {
        run(vec![Client::Silent, Client::Garbage(100_000), Client::Partial, Client::Status, Client::Stop, Client::Status]).unwrap();
    }

    /// the last lines as a split of the whole text gives them, for any text, count and step
    fn tail_model(text: &[u8], lines: usize) -> Vec<u8> {
        let nl: Vec<usize> = text.iter().enumerate().filter(|(_, &b)| b == b'\n').map(|(i, _)| i).collect();
        let cut = if nl.len() > lines { nl[nl.len() - lines - 1] + 1 } else { 0 };
        text[cut..].to_vec()
    }

    #[test]
    fn rotation_keeps_one_old_log() {
        let dir = std::env::temp_dir().join(format!("masu-rot-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("server.log");
        std::fs::write(&p, vec![b'a'; 100]).unwrap();
        rotate(&p, 100);
        assert!(p.exists());
        rotate(&p, 99);
        assert!(!p.exists());
        assert_eq!(std::fs::read(dir.join("server.log.1")).unwrap().len(), 100);
        std::fs::write(&p, b"new").unwrap();
        rotate(&p, 1);
        assert_eq!(std::fs::read(dir.join("server.log.1")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    proptest! {
        #[test]
        fn tail_is_the_last_lines(text in proptest::collection::vec(prop_oneof![Just(b'\n'), Just(b'x'), Just(b'y')], 0..600), lines in 0usize..20, step in 1u64..80) {
            let mut c = std::io::Cursor::new(text.clone());
            prop_assert_eq!(tail_of(&mut c, text.len() as u64, lines, step).unwrap(), tail_model(&text, lines));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]
        #[test]
        fn any_clients_in_any_order(clients in proptest::collection::vec(client(), 1..12)) {
            run(clients)?;
        }
    }
}
