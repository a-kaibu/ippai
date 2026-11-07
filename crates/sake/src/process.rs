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

/// Splits a command line into the program and the rest.
pub fn split_command(cmd: &str) -> (String, String) {
    let cmd = cmd.trim_start();
    if let Some(q) = cmd.strip_prefix('"') {
        let end = q.find('"').unwrap_or(q.len());
        (
            q[..end].to_string(),
            q.get(end + 1..).unwrap_or("").trim().to_string(),
        )
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
        let exe = crate::pe::MODULES
            .read()
            .unwrap()
            .iter()
            .find(|m| !m.dll)
            .map(|m| m.path.clone())
            .unwrap_or_default();
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
            if std::path::Path::new(&h).is_file() {
                return Ok((files::host_to_win(&h), h));
            }
        }
    }
    let full = files::full_path(&name);
    let dir_there = full
        .rsplit_once('\\')
        .is_none_or(|(d, _)| d.len() <= 2 || files::resolve(d).is_some());
    Err(if bare || dir_there { 2 } else { 3 })
}

/// What a handle of this process is to another process (None: nothing another can use).
fn kref(o: &Obj) -> Option<KRef> {
    match o {
        Obj::Pipe(p) => Some(KRef::Pipe {
            end: p.end,
            read: p.read,
            write: p.write,
            ovl: p.ovl,
        }),
        Obj::Process { access, pid } => Some(KRef::Process {
            pid: *pid,
            access: *access,
        }),
        Obj::Shared(id) => Some(KRef::Sync { id: *id }),
        _ => None,
    }
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
    Some((
        mem::rp(si + std),
        mem::rp(si + std + ps),
        mem::rp(si + std + 2 * ps),
    ))
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
    // (SAKE_NO_EXEC: tests/winetest, whose programs start copies of themselves as children)
    if std::env::var_os("SAKE_NO_EXEC").is_some() {
        eprintln!("sake: CreateProcess({prog}, {args}) refused");
        last_error(2);
        return 0;
    }
    let (win, host) = match find_program(&prog, app.is_some()) {
        Ok(p) => p,
        Err(e) => {
            last_error(e);
            return 0;
        }
    };
    // a host that runs one process: the program takes this one's place (crate::k32::exec)
    if !kern::share() {
        if !crate::k32::exec(&prog, &args, dir.as_deref()) {
            eprintln!("sake: CreateProcess({prog}, {args}) refused");
        }
        last_error(2);
        return 0;
    }
    watch();
    let cmdline = cmd.unwrap_or_else(|| format!("\"{win}\""));
    let cwd = match dir {
        Some(d) => files::full_path(&d),
        None => files::cwd(),
    };
    let list: Vec<(u32, KRef)> = if inherit {
        obj::with(|t| {
            t.inheritable()
                .filter_map(|(h, o)| Some((h, kref(o)?)))
                .collect()
        })
    } else {
        Vec::new()
    };
    let p = Proc {
        parent: crate::pid(),
        image: win.clone(),
        cmdline,
        cwd,
        suspend: (flags & 4 != 0) as u32,
        inherit: list,
        std: startup_std(si),
        ..Proc::default()
    };
    let pid = kern::with(|k| k.new_process(p)).0;
    if crate::env_on!("SAKE_TRACE_EXIT") {
        eprintln!("sake: CreateProcess {win} -> process {pid:#x}");
    }
    if !kern::host::spawn(pid, &host) {
        kern::with(|k| k.exited(pid, 1));
        last_error(2);
        return 0;
    }
    let hp = obj::add(Obj::Process {
        access: PROCESS_ALL_ACCESS,
        pid,
    });
    let ht = obj::add(Obj::RemoteThread {
        pid,
        tid: first_tid(pid),
    });
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
            KRef::Pipe {
                end,
                read,
                write,
                ovl,
            } => Obj::Pipe(crate::pipe::Handle {
                end,
                read,
                write,
                ovl,
            }),
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
        crate::pipe::pump();
        crate::user::deliver_remote();
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
    v.extend(
        kern::with(|k| {
            k.procs
                .iter()
                .filter(|(&p, r)| p != me && r.exit.is_none())
                .map(|(&p, _)| p)
                .collect::<Vec<_>>()
        })
        .0,
    );
    v
}
