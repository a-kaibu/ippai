//! Other processes: CreateProcess starts a program as a process of its own (another instance of
//! sake that the host runs: node's runner, the page), sharing the kernel (crate::kern) with this
//! one. A child starts with its command line, current directory, inherited handles (the same
//! numbers) and standard handles from its record in the kernel, and waits there while it is
//! suspended. Every process of a shared kernel has a watcher thread: it wakes the process's
//! waits when another process changes the kernel, and ends the process when another
//! terminated it, and hands its windows the messages other processes posted or sent to them
//! (crate::user::deliver_remote).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::Ctx;
use crate::files;
use crate::k32::{arg_str, last_error};
use crate::kern::{self, KRef, Proc, PENDING};
use crate::mem::{self, r32, w32};
use crate::obj::{self, Obj};

pub const PROCESS_ALL_ACCESS: u32 = 0x1f_ffff;
const STILL_ACTIVE: u32 = 0x103;

/// Whether `pid` is this process.
pub fn is_self(pid: u32) -> bool {
    pid == crate::pid()
}

/// The arguments of a command line by CommandLineToArgvW's rules. The first one, the program, is
/// taken as it is: up to the next quote when it starts with one, else up to the first space or
/// tab, so a line that starts with a space has an empty program (" /i x.msi", which Visual
/// Studio's MSI loader passes to msiexec with lpApplicationName). The others: spaces and tabs
/// outside quotes separate, 2n backslashes before a quote are n and the quote opens or closes,
/// 2n+1 are n and a literal quote, other backslashes are themselves, and "" inside quotes is
/// a literal quote that closes them.
pub fn argv(cmd: &str) -> Vec<String> {
    let s: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let mut prog = String::new();
    if s.first() == Some(&'"') {
        i = 1;
        while i < s.len() && s[i] != '"' {
            prog.push(s[i]);
            i += 1;
        }
        i += 1;
    } else {
        while i < s.len() && s[i] != ' ' && s[i] != '\t' {
            prog.push(s[i]);
            i += 1;
        }
    }
    arguments(&s, i, prog)
}

/// The arguments as the C runtime (msvcrt's __getmainargs, __argv) splits them: as argv, but
/// the program's quotes open and close anywhere in it ("a b"c is one program, a bc).
pub fn crt_argv(cmd: &str) -> Vec<String> {
    let s: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let mut prog = String::new();
    let mut quoted = false;
    while i < s.len() && (quoted || (s[i] != ' ' && s[i] != '\t')) {
        if s[i] == '"' {
            quoted = !quoted;
        } else {
            prog.push(s[i]);
        }
        i += 1;
    }
    arguments(&s, i, prog)
}

/// The program, then the arguments of s[i..].
fn arguments(s: &[char], mut i: usize, prog: String) -> Vec<String> {
    let mut out = vec![prog];
    loop {
        while i < s.len() && (s[i] == ' ' || s[i] == '\t') {
            i += 1;
        }
        if i >= s.len() {
            break;
        }
        let mut cur = String::new();
        let mut quoted = false;
        while i < s.len() {
            match s[i] {
                ' ' | '\t' if !quoted => break,
                '\\' => {
                    let n = s[i..].iter().take_while(|&&c| c == '\\').count();
                    i += n;
                    if s.get(i) == Some(&'"') {
                        cur.extend(std::iter::repeat_n('\\', n / 2));
                        if n % 2 == 1 {
                            cur.push('"');
                            i += 1;
                        }
                    } else {
                        cur.extend(std::iter::repeat_n('\\', n));
                    }
                    continue;
                }
                '"' if quoted && s.get(i + 1) == Some(&'"') => {
                    cur.push('"');
                    quoted = false;
                    i += 2;
                    continue;
                }
                '"' => quoted = !quoted,
                c => cur.push(c),
            }
            i += 1;
        }
        out.push(cur);
    }
    out
}

/// Splits a command line into the program and the rest.
pub fn split_command(cmd: &str) -> (String, String) {
    let cmd = cmd.trim_start();
    if let Some(q) = cmd.strip_prefix('"') {
        let end = q.find('"').unwrap_or(q.len());
        (q[..end].to_string(), q.get(end + 1..).unwrap_or("").trim().to_string())
    } else {
        let end = cmd.find([' ', '\t']).unwrap_or(cmd.len());
        (cmd[..end].to_string(), cmd[end..].trim().to_string())
    }
}

/// Finds the program CreateProcess runs: (Windows path, host path), or the error
/// (ERROR_FILE_NOT_FOUND; ERROR_PATH_NOT_FOUND when its directory isn't there). A bare name
/// is looked for in the program's directory, the current one, system32, the Windows
/// directory; without an extension it gets ".exe" (unless it came as lpApplicationName).
fn find_program(prog: &str, app: bool) -> Result<(String, String), u32> {
    let mut name = prog.trim().to_string();
    let file = name.rsplit(['\\', '/']).next().unwrap_or("");
    if !app && !file.contains('.') {
        name += ".exe";
    }
    let bare = !name.contains(['\\', '/']);
    let mut tries = Vec::new();
    if bare {
        let exe = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.clone()).unwrap_or_default();
        if let Some((d, _)) = exe.rsplit_once('\\') {
            tries.push(format!("{d}\\{name}"));
        }
        tries.push(files::full_path(&name));
        tries.push(format!("C:\\windows\\system32\\{name}"));
        tries.push(format!("C:\\windows\\{name}"));
    } else {
        tries.push(files::full_path(&name));
    }
    for w in &tries {
        if let Some(h) = files::resolve(w) {
            if kura::fs::metadata(&h).is_ok_and(|m| m.is_file()) {
                return Ok((files::host_to_win(&h), h));
            }
        }
    }
    let full = files::full_path(&name);
    let dir_there = full.rsplit_once('\\').is_none_or(|(d, _)| d.len() <= 2 || files::resolve(d).is_some());
    Err(if bare || dir_there { 2 } else { 3 })
}

/// What a handle of this process is to another process (None: nothing another can use).
fn kref(o: &Obj) -> Option<KRef> {
    match o {
        Obj::Pipe(p) => Some(KRef::Pipe { end: p.end, read: p.read, write: p.write, ovl: p.ovl }),
        Obj::Process { access, pid } => Some(KRef::Process { pid: *pid, access: *access }),
        Obj::Shared(id) => Some(KRef::Sync { id: *id }),
        _ => None,
    }
}

/// Handle `h` for another process (DuplicateHandle into it): what it is there. An unnamed
/// event, mutex or semaphore becomes the kernel's first (its handles here use it there too).
pub fn share_handle(h: u32) -> Option<KRef> {
    let me = crate::pid();
    let local = obj::with(|t| t.objs.get(&h).map(|(o, _)| match o {
        Obj::Event { manual, set } => Some(kern::Sync::Event { manual: *manual, set: *set }),
        Obj::Mutex { owner, count } if *owner == obj::ABANDONED => Some(kern::Sync::Mutex { owner: 0, pid: 0, count: 0, abandoned: true }).filter(|_| *count == 0),
        Obj::Mutex { owner, count } => Some(kern::Sync::Mutex { owner: *owner, pid: if *owner != 0 { me } else { 0 }, count: *count, abandoned: false }),
        Obj::Semaphore { count, max } => Some(kern::Sync::Semaphore { count: *count, max: *max }),
        _ => None,
    }))?;
    if let Some(sync) = local {
        if !kern::share() {
            return None;
        }
        watch();
        let name = format!("\0anonymous {me:x} {h:x} {}", crate::k32::ticks_ms());
        let id = kern::with(|k| k.create_sync(&name, sync, me)).0.ok()?.0;
        obj::with(|t| {
            if let Some((o, _)) = t.objs.get_mut(&h) {
                *o = Obj::Shared(id);
            }
        });
    }
    obj::with(|t| t.objs.get(&h).and_then(|(o, _)| kref(o)))
}

/// A handle here for what another process duplicated into this one
pub fn adopt_handle(r: KRef) -> u32 {
    let o = match r {
        KRef::Pipe { end, read, write, ovl } => Obj::Pipe(crate::pipe::Handle { end, read, write, ovl }),
        KRef::Process { pid, access } => Obj::Process { access, pid },
        KRef::Sync { id } => {
            kern::with(|k| k.add_sync_ref(id, crate::pid()));
            Obj::Shared(id)
        }
    };
    obj::add(o)
}

/// STARTUPINFO's dwFlags and its three standard handles
fn startup_std(si: u32) -> Option<(u32, u32, u32)> {
    if si == 0 {
        return None;
    }
    let (flags, std) = if mem::x64() { (60, 80) } else { (44, 56) };
    if r32(si + flags) & 0x100 == 0 {
        return None;
    }
    let ps = mem::ps();
    Some((mem::rp(si + std), mem::rp(si + std + ps), mem::rp(si + std + 2 * ps)))
}

/// A new process running `prog` (`app`: named exactly, as lpApplicationName, else ".exe" is
/// added to a bare name) with `args` after its name, `cmd` as its command line (None: the
/// program's name), the current directory `dir`, this process's inheritable handles when
/// `inherit`, CreateProcess `flags` (CREATE_SUSPENDED) and standard handles `std`: its id, or
/// the Windows error. On a host that runs one process the program takes this one's place
/// (crate::k32::exec) and the answer is ERROR_FILE_NOT_FOUND. CreateProcess and the C runtime's
/// _spawn / _exec functions start processes here.
#[allow(clippy::too_many_arguments)]
/// `env`: the child's environment, None: this process's as it is now (Windows passes the
/// parent's current environment when lpEnvironment is NULL).
pub fn spawn(prog: &str, app: bool, args: &str, cmd: Option<String>, dir: Option<String>, inherit: bool, flags: u32, std: Option<(u32, u32, u32)>, env: Option<Vec<(String, String)>>) -> Result<u32, u32> {
    // (SAKE_NO_EXEC: tests/winetest, whose programs start copies of themselves as children)
    if std::env::var_os("SAKE_NO_EXEC").is_some() {
        eprintln!("sake: CreateProcess({prog}, {args}) refused");
        return Err(2);
    }
    let (win, host) = find_program(prog, app)?;
    // a host that runs one process: the program takes this one's place (crate::k32::exec)
    if !kern::share() {
        if !crate::k32::exec(prog, args, dir.as_deref()) {
            eprintln!("sake: CreateProcess({prog}, {args}) refused");
        }
        return Err(2);
    }
    watch();
    let cmdline = cmd.unwrap_or_else(|| format!("\"{win}\""));
    let cwd = match dir {
        Some(d) => files::full_path(&d),
        None => files::cwd(),
    };
    // (an unnamed event, mutex or semaphore becomes the kernel's, to be the child's too)
    let list: Vec<(u32, KRef)> = if inherit {
        let hs: Vec<u32> = obj::with(|t| t.inheritable().map(|(h, _)| h).collect());
        hs.into_iter().filter_map(|h| Some((h, share_handle(h)?))).collect()
    } else {
        Vec::new()
    };
    let env = Some(env.unwrap_or_else(crate::k32::env_vars));
    let p = Proc { parent: crate::pid(), image: win.clone(), cmdline, cwd, suspend: (flags & 4 != 0) as u32, inherit: list, std, env, ..Proc::default() };
    // (DEBUG_PROCESS, DEBUG_ONLY_THIS_PROCESS: the creating thread's debug object, made if
    // it has none; a debuggee's children: crate::dbg)
    let port = if flags & 3 != 0 { crate::dbg::connect() } else { None };
    let pid = kern::with(|k| {
        let pid = k.new_process(p);
        k.dbg.spawned(port, crate::pid(), pid, flags);
        pid
    }).0;
    if crate::env_on!("SAKE_TRACE_EXIT") {
        eprintln!("sake: CreateProcess {win} -> process {pid:#x}");
    }
    if !kern::host::spawn(pid, &host) {
        kern::with(|k| k.exited(pid, 1));
        return Err(2);
    }
    Ok(pid)
}

/// The id a new process handle names (the C runtime's _spawn with _P_NOWAIT).
pub fn process_handle(pid: u32) -> u32 {
    obj::add(Obj::Process { access: PROCESS_ALL_ACCESS, pid })
}

/// The variables of an environment block at `p` ("NAME=value\0" each, then "\0"; a name may
/// start with "=", as the "=C:=C:\dir" entries of the drives' current folders do).
pub fn env_block_vars(p: u32, wide: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut at = p;
    loop {
        let s = arg_str(at, wide);
        if s.is_empty() {
            break;
        }
        let units = if wide { 2 * (s.encode_utf16().count() as u32 + 1) } else { crate::text::s2a(&s).len() as u32 + 1 };
        at += units;
        out.push(split_env_entry(&s));
    }
    out
}

/// "NAME=value" -> (NAME, value); the "=" a name starts with is the name's.
pub fn split_env_entry(s: &str) -> (String, String) {
    match s.char_indices().skip(1).find(|&(_, c)| c == '=') {
        Some((i, _)) => (s[..i].to_string(), s[i + 1..].to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// Waits for process `pid` to end: its exit code.
pub fn wait_exit(pid: u32) -> u32 {
    kern::block(None, &|| false, |k| k.exit_code(pid).ok_or(kern::PENDING)).unwrap_or(1)
}

/// CreateProcessA/W(app, command line, process sa, thread sa, inherit, flags, environment,
/// directory, startup info, process information)
pub fn create_process(c: &mut Ctx, wide: bool) -> u32 {
    let app = (c.a(0) != 0).then(|| arg_str(c.a(0), wide));
    let cmd = (c.a(1) != 0).then(|| arg_str(c.a(1), wide));
    let (inherit, flags, dir, si, pi) = (c.a(4) != 0, c.a(5), c.a(7), c.a(8), c.a(9));
    let (prog, args) = match (&app, &cmd) {
        (Some(a), Some(cl)) => (a.clone(), split_command(cl).1),
        (Some(a), None) => (a.clone(), String::new()),
        (None, Some(cl)) => split_command(cl),
        (None, None) => {
            last_error(87);
            return 0;
        }
    };
    let dir = (dir != 0).then(|| arg_str(dir, wide));
    // (CREATE_UNICODE_ENVIRONMENT: a block of UTF-16 strings)
    let env = (c.a(6) != 0).then(|| env_block_vars(c.a(6), flags & 0x400 != 0));
    let pid = match spawn(&prog, app.is_some(), &args, cmd, dir, inherit, flags, startup_std(si), env) {
        Ok(pid) => pid,
        Err(e) => {
            last_error(e);
            return 0;
        }
    };
    let hp = obj::add(Obj::Process { access: PROCESS_ALL_ACCESS, pid });
    let ht = obj::add(Obj::RemoteThread { pid, tid: first_tid(pid) });
    if pi != 0 {
        let ps = mem::ps();
        mem::wp(pi, hp);
        mem::wp(pi + ps, ht);
        w32(pi + 2 * ps, pid);
        w32(pi + 2 * ps + 4, first_tid(pid));
    }
    last_error(0);
    1
}

/// A process's first thread's id (crate::main numbers threads from the process's id up)
pub fn first_tid(pid: u32) -> u32 {
    pid + 0x10
}

/// SCM starts the registered command as a real process of the shared kernel.
/// Publish its ownership before spawning: ServiceMain may report status as soon
/// as the host starts the instance.
pub(crate) fn spawn_service(command: &str, service: &str) -> Result<u32, u32> {
    let command = command.trim();
    let (win, host, arguments) = if command.starts_with('"') {
        let (program, arguments) = split_command(command);
        let (win, host) = find_program(&program, false)?;
        (win, host, arguments)
    } else {
        // SCM ImagePath may be an unquoted path containing spaces. Resolve the
        // executable progressively, as CreateProcess does, then quote argv[0].
        let mut found = None;
        for end in command.char_indices().filter_map(|(i, c)| c.is_whitespace().then_some(i)).chain([command.len()]) {
            if let Ok((win, host)) = find_program(&command[..end], false) {
                found = Some((win, host, command[end..].trim().to_string())); break;
            }
        }
        found.ok_or(2u32)?
    };
    if !kern::share() { return Err(1054); }
    watch();
    let p = Proc { parent: crate::pid(), image: win.clone(), cmdline: format!("\"{win}\" {arguments}"),
        cwd: "C:\\windows\\system32\\".into(), service: Some(service.into()), ..Proc::default() };
    let pid = kern::with(|k| {
        let pid = k.new_process(p);
        if let Some(s) = k.services.get_mut(service) { s.pid = pid; }
        pid
    }).0;
    if !kern::host::spawn(pid, &host) {
        kern::with(|k| k.exited(pid, 1));
        return Err(2);
    }
    Ok(pid)
}

/// The process another one started: its record (None for the first process).
pub fn record() -> Option<Proc> {
    if crate::pid() == kern::ROOT_PID {
        return None;
    }
    kern::with(|k| k.proc(crate::pid()).cloned()).0
}

/// A child's start (before its program loads): its inherited handles; the watcher.
pub fn start_child(r: &Proc) {
    for &(h, o) in &r.inherit {
        let o = match o {
            KRef::Pipe { end, read, write, ovl } => Obj::Pipe(crate::pipe::Handle { end, read, write, ovl }),
            KRef::Process { pid, access } => Obj::Process { access, pid },
            KRef::Sync { id } => Obj::Shared(id),
        };
        obj::add_at(h, o, true);
    }
    watch();
}

/// A suspended child waits here until its first thread is resumed.
pub fn wait_resumed() {
    let me = crate::pid();
    let _ = kern::block(None, &|| false, |k| match k.proc(me) {
        Some(p) if p.suspend > 0 => Err(PENDING),
        _ => Ok(()),
    });
}

static WATCHING: AtomicBool = AtomicBool::new(false);

/// The watcher thread (once the kernel is shared).
pub fn watch() {
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let me = crate::pid();
    crate::user::publish();
    std::thread::spawn(move || loop {
        let (kill, gen) = kern::with(|k| k.proc(me).and_then(|p| p.kill));
        if let Some(code) = kill {
            if crate::env_on!("SAKE_TRACE_EXIT") {
                eprintln!("sake: terminated by another process ({code})");
            }
            unsafe { libc::exit(code as i32) }
        }
        obj::signal();
        crate::dbg::watch_attach();
        crate::xmem::serve();
        crate::pipe::pump();
        crate::user::deliver_remote();
        crate::comrpc::wake();
        kern::wait_change(gen, None);
    });
}

// ---------------------------------------------------------------------------------------------
// process handles

/// The process a handle is: (id, access); GetCurrentProcess's is this one.
pub fn of_handle(h: u32) -> Option<(u32, u32)> {
    if h == obj::CURRENT_PROCESS {
        return Some((crate::pid(), PROCESS_ALL_ACCESS));
    }
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Process { access, pid }, _)) => Some((*pid, *access)),
        _ => None,
    })
}

/// Whether another process has ended (for waits on its handles).
pub fn ended(pid: u32) -> bool {
    !is_self(pid) && kern::with(|k| k.exit_code(pid).is_some()).0
}

pub fn exit_code(pid: u32) -> u32 {
    if is_self(pid) {
        return STILL_ACTIVE;
    }
    kern::with(|k| k.exit_code(pid)).0.unwrap_or(STILL_ACTIVE)
}

/// TerminateProcess of another process
pub fn terminate(pid: u32, code: u32) -> bool {
    kern::with(|k| k.terminate(pid, code)).0
}

/// OpenProcess of another process: whether there is one with that id.
pub fn exists(pid: u32) -> bool {
    kern::with(|k| k.known(pid)).0
}

pub fn resume(pid: u32) -> u32 {
    kern::with(|k| k.resume(pid)).0
}

/// The ids of every process the kernel knows that has not ended (this one first).
pub fn running() -> Vec<u32> {
    let me = crate::pid();
    let mut v = vec![me];
    v.extend(kern::with(|k| k.procs.iter().filter(|(&p, r)| p != me && r.exit.is_none()).map(|(&p, _)| p).collect::<Vec<_>>()).0);
    v
}

#[cfg(test)]
mod argv_tests {
    use super::{argv, split_env_entry};
    use proptest::prelude::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    /// The cases of tests/winref/probes/argv.c (CommandLineToArgvW on Windows)
    #[test]
    fn windows_rules() {
        assert_eq!(argv("p a b"), v(&["p", "a", "b"]));
        // (no program: msiloadr's " /i x.msi" to msiexec)
        assert_eq!(argv(" /i x.msi"), v(&["", "/i", "x.msi"]));
        assert_eq!(argv("\t/i x.msi"), v(&["", "/i", "x.msi"]));
        assert_eq!(argv("\"C:\\a b\\p.exe\" one \"two three\""), v(&["C:\\a b\\p.exe", "one", "two three"]));
        assert_eq!(argv("\"C:\\a b\\p.exe\"tail x"), v(&["C:\\a b\\p.exe", "tail", "x"]));
        assert_eq!(argv("p a\\\\b \"c\\\\\" d"), v(&["p", "a\\\\b", "c\\", "d"]));
        assert_eq!(argv("p a\\\"b c"), v(&["p", "a\"b", "c"]));
        assert_eq!(argv("p a\\\\\\\"b c"), v(&["p", "a\\\"b", "c"]));
        assert_eq!(argv("p a\\\\\"b c\" d"), v(&["p", "a\\b c", "d"]));
        assert_eq!(argv("p \"\" x"), v(&["p", "", "x"]));
        assert_eq!(argv("p \"a\"\"b\" c"), v(&["p", "a\"b c"]));
        assert_eq!(argv("p \"a\"\"\"b c"), v(&["p", "a\"b c"]));
        assert_eq!(argv("p a\"b\"c"), v(&["p", "abc"]));
        assert_eq!(argv("p    a\t\tb   "), v(&["p", "a", "b"]));
        assert_eq!(argv("p \"unterminated arg"), v(&["p", "unterminated arg"]));
        assert_eq!(argv("p NAME=\"C:\\My Game\" X=1"), v(&["p", "NAME=C:\\My Game", "X=1"]));
        assert_eq!(argv(""), v(&[""]));
    }

    #[test]
    fn env_entries() {
        assert_eq!(split_env_entry("PATH=C:\\w;C:\\x"), ("PATH".into(), "C:\\w;C:\\x".into()));
        assert_eq!(split_env_entry("=C:=C:\\dir"), ("=C:".into(), "C:\\dir".into()));
        assert_eq!(split_env_entry("A=b=c"), ("A".into(), "b=c".into()));
        assert_eq!(split_env_entry("A="), ("A".into(), "".into()));
    }

    /// An argument quoted the way programs build command lines for CommandLineToArgvW (quotes
    /// only when needed; backslashes doubled before a quote and at the end of a quoted one).
    fn quote(a: &str) -> String {
        if !a.is_empty() && !a.contains([' ', '\t', '"']) {
            return a.to_string();
        }
        let mut out = String::from("\"");
        let mut slashes = 0;
        for ch in a.chars() {
            match ch {
                '\\' => slashes += 1,
                '"' => {
                    // (the n already written, n more and one for the quote)
                    out.extend(std::iter::repeat_n('\\', slashes + 1));
                    out.push('"');
                    slashes = 0;
                    continue;
                }
                _ => slashes = 0,
            }
            out.push(ch);
        }
        out.extend(std::iter::repeat_n('\\', slashes));
        out.push('"');
        out
    }

    proptest! {
        /// Arguments quoted as programs quote them come back as they went in.
        #[test]
        fn quoted_arguments_come_back(args in prop::collection::vec("[a-z\\\\\" \t=]{0,8}", 0..6), lead in any::<bool>()) {
            let prog = if lead { String::new() } else { "\"C:\\a b\\p.exe\"".to_string() };
            let line = std::iter::once(prog.clone()).chain(args.iter().map(|a| quote(a))).collect::<Vec<_>>().join(" ");
            let got = argv(&line);
            prop_assert_eq!(&got[0], if lead { "" } else { "C:\\a b\\p.exe" });
            prop_assert_eq!(&got[1..], &args[..]);
        }

        /// Without quotes or backslashes, the arguments are the words between spaces and tabs.
        #[test]
        fn plain_words(line in "[a-z/= \t]{0,40}") {
            let got = argv(&line);
            let mut want: Vec<String> = Vec::new();
            let first_end = line.find([' ', '\t']).unwrap_or(line.len());
            want.push(line[..first_end].to_string());
            want.extend(line[first_end..].split([' ', '\t']).filter(|w| !w.is_empty()).map(String::from));
            prop_assert_eq!(got, want);
        }

        /// Any line splits without panicking, and no argument holds a space or tab that was
        /// outside quotes (each one is nonempty or came from quotes).
        #[test]
        fn any_line(line in "[a-z\\\\\" \t]{0,40}") {
            let got = argv(&line);
            prop_assert!(!got.is_empty());
            if !line.contains('"') {
                prop_assert!(got.iter().all(|a| !a.contains([' ', '\t'])));
            }
        }

        /// "NAME=value" splits back for names without "=" (or with one leading "=").
        #[test]
        fn env_entry_back(lead in any::<bool>(), name in "[A-Za-z_:]{1,8}", value in "[A-Za-z=:;\\\\]{0,12}") {
            let n = if lead { format!("={name}") } else { name };
            prop_assert_eq!(split_env_entry(&format!("{n}={value}")), (n, value));
        }
    }
}
