//! Another process's memory (and its threads' suspend counts): VirtualAllocEx, VirtualFreeEx, VirtualProtectEx, VirtualQueryEx,
//! Read/WriteProcessMemory and NtAllocate/NtFreeVirtualMemory on a handle to another process.
//! The request goes through the shared kernel (crate::kern's `put_vm`) to that process, whose
//! watcher thread (crate::process::watch) does it there, with the same code the process uses
//! on itself, and answers; the caller waits for the answer (or for the process to end:
//! ERROR_ACCESS_DENIED, as for a process whose memory is gone).
//!
//! The operations run without a guest thread (no last error): each gives Ok or the error.

use crate::kern::{self, VmReply, VmReq, VM_GONE};
use crate::mem;

pub const ALLOC: u32 = 1;
pub const FREE: u32 = 2;
pub const PROTECT: u32 = 3;
pub const QUERY: u32 = 4;
pub const READ: u32 = 5;
pub const WRITE: u32 = 6;
pub const NT_FREE: u32 = 7;
/// Suspend/ResumeThread of one of its threads (a thread id): the count before
pub const SUSPEND: u32 = 8;
pub const RESUME: u32 = 9;
/// CreateRemoteThread: (start, param, stack size, flags) -> the thread's id
pub const CREATE_THREAD: u32 = 10;
/// DuplicateHandle into it: the encoded crate::kern::KRef -> its handle there
pub const ADD_HANDLE: u32 = 11;
/// a debugger attached / detached (crate::dbg), DebugBreakProcess
pub const DEBUG_ATTACH: u32 = 12;
pub const DEBUG_DETACH: u32 = 13;
pub const DEBUG_BREAK: u32 = 14;
/// Get/SetThreadContext of one of its threads: (tid, ContextFlags) [, the CONTEXT's bytes]
pub const GET_CONTEXT: u32 = 15;
pub const SET_CONTEXT: u32 = 16;
/// NtSuspendProcess / NtResumeProcess: each of its threads
pub const SUSPEND_PROCESS: u32 = 17;
pub const RESUME_PROCESS: u32 = 18;
/// GetMappedFileName: (address) -> the Windows path of the file mapped there
pub const MAPPED_FILE: u32 = 19;

/// Which process a handle names, for these calls
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Own,
    Other(u32),
    /// no process handle: ERROR_INVALID_HANDLE
    Bad,
}

pub fn target(h: u32) -> Target {
    match crate::process::of_handle(h) {
        Some((pid, _)) if crate::process::is_self(pid) => Target::Own,
        Some((pid, _)) => Target::Other(pid),
        None => Target::Bad,
    }
}

/// Does operation `op` here: what the process that asked gets back
pub fn exec(op: u32, a: &[u32], data: &[u8]) -> VmReply {
    let arg = |i: usize| a.get(i).copied().unwrap_or(0);
    let ok = |vals: Vec<u32>| VmReply { status: 0, vals, data: vec![] };
    let err = |e: u32| VmReply { status: e, ..Default::default() };
    match op {
        ALLOC => match crate::k32::alloc_r(arg(0), arg(1), arg(2), arg(3)) {
            Ok(b) => ok(vec![b]),
            Err(e) => err(e),
        },
        FREE => match crate::k32::free_r(arg(0), arg(1), arg(2)) {
            Ok(()) => ok(vec![]),
            Err(e) => err(e),
        },
        NT_FREE => match crate::k32::nt_free_r(arg(0), arg(1), arg(2)) {
            Ok((b, n)) => ok(vec![b, n]),
            Err(e) => err(e),
        },
        PROTECT => match mem::protect(arg(0), arg(1), arg(2)) {
            Some(old) => ok(vec![old]),
            None => err(487),
        },
        QUERY => ok(crate::k32::query_r(arg(0)).to_vec()),
        READ => {
            // (a source that is not all readable copies nothing: ERROR_PARTIAL_COPY)
            let (p, n) = (arg(0), arg(1));
            if mem::accessible(p, n, false, false) < n {
                return err(299);
            }
            VmReply { status: 0, vals: vec![], data: mem::slice(p, n).to_vec() }
        }
        WRITE => {
            let (p, n) = (arg(0), data.len() as u32);
            let k = mem::accessible(p, n, true, true);
            if k == 0 && n != 0 {
                return err(998);
            }
            mem::write(p, &data[..k as usize]);
            kura::jit::code_changed(p as usize, k.max(1) as usize);
            ok(vec![k])
        }
        SUSPEND | RESUME => match crate::k32::suspend_for_other(arg(0), op == SUSPEND) {
            Ok(n) => ok(vec![n]),
            Err(e) => err(e), // (ERROR_SIGNAL_REFCOUNT_EXCEEDED)
        },
        CREATE_THREAD => ok(vec![crate::k32::thread_for_other(arg(0), arg(1), arg(2), arg(3))]),
        ADD_HANDLE => {
            use crate::kern::enc::Enc;
            let mut r = data;
            match crate::kern::KRef::get(&mut r) {
                Some(k) => ok(vec![crate::process::adopt_handle(k)]),
                None => err(87),
            }
        }
        DEBUG_ATTACH => {
            crate::dbg::attached();
            ok(vec![])
        }
        DEBUG_DETACH => {
            crate::dbg::detached();
            ok(vec![])
        }
        GET_CONTEXT => match crate::tctx::context_of(arg(0)) {
            Some(bytes) => VmReply { status: 0, vals: vec![], data: bytes },
            None => err(6),
        },
        SET_CONTEXT => {
            crate::tctx::set_context_of(arg(0), arg(1), data);
            ok(vec![])
        }
        MAPPED_FILE => match crate::k32::mapped_file(arg(0)) {
            Ok(p) => VmReply { status: 0, vals: vec![], data: p.into_bytes() },
            Err(e) => err(e),
        },
        SUSPEND_PROCESS | RESUME_PROCESS => {
            crate::k32::suspend_process(op == SUSPEND_PROCESS, 0);
            ok(vec![])
        }
        DEBUG_BREAK => {
            crate::dbg::break_in();
            ok(vec![])
        }
        _ => err(87),
    }
}

/// Asks process `pid` to do `op`; None if it ended first.
pub fn call(pid: u32, op: u32, args: Vec<u32>, data: Vec<u8>) -> Option<VmReply> {
    let id = kern::with(|k| k.put_vm(VmReq { id: 0, to: pid, op, args, data })).0;
    let r = kern::block(None, &|| false, |k| k.take_vm_reply(id)).ok()?;
    if r.status == VM_GONE && crate::env_on!("SAKE_TRACE_DBG") {
        let st = kern::with(|k| (k.exit_code(pid), k.procs.contains_key(&pid))).0;
        eprintln!("sake: xmem op {op} to {pid:#x}: gone {st:?}");
    }
    (r.status != VM_GONE).then_some(r)
}

/// The watcher's part: does what other processes asked of this one.
pub fn serve() {
    let reqs = kern::with(|k| k.take_vm(crate::pid())).0;
    for r in reqs {
        let reply = exec(r.op, &r.args, &r.data);
        kern::with(|k| k.vm_reply(r.id, reply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_operation_is_invalid_parameter() {
        assert_eq!(exec(99, &[], &[]).status, 87);
    }
}
