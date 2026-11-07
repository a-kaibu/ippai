//! Structured exception handling on the guest's fs:[0] chain.
//!
//! The dispatch loop runs as guest code (a stub below), so a handler that catches the
//! exception (RtlUnwind, then a jump into its function) leaves no host frame behind; the
//! stub reports each handler's verdict to the host through SEH_TRAP.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use koji::{Cpu, Fault, Memory, EAX, EBP, EBX, ECX, EDI, EDX, ESI, ESP};

use crate::api::{Ctx, SEH_TRAP};
use crate::mem::{r32, w32};

pub const CONTEXT_SIZE: u32 = 0x2cc;
const RECORD_SIZE: u32 = 0x50;

/// Guest memory for koji's FXSAVE helpers.
pub struct Guest;

impl Memory for Guest {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        Ok(crate::mem::r8(a))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        crate::mem::w8(a, v);
        Ok(())
    }
}

/// The guest's handler caller, like Windows' ExecuteHandler2: stdcall (rec, frame, ctx,
/// dispatcher, handler). Handlers see EBP pointing at this frame (Borland's read their
/// arguments through it) and a nested-exception registration on top of the chain.
fn exec() -> u32 {
    static S: OnceLock<u32> = OnceLock::new();
    *S.get_or_init(|| {
        let nested = crate::api::stub(&[0xb8, 1, 0, 0, 0, 0xc2, 0x10, 0x00]).to_le_bytes();
        crate::api::stub(&[
            0x55, //                                push ebp
            0x8b, 0xec, //                          mov ebp, esp
            0xff, 0x75, 0x0c, //                    push [ebp+0xc]
            0x68, nested[0], nested[1], nested[2], nested[3], // push nested
            0x64, 0xff, 0x35, 0, 0, 0, 0, //        push fs:[0]
            0x64, 0x89, 0x25, 0, 0, 0, 0, //        mov fs:[0], esp
            0xff, 0x75, 0x14, //                    push [ebp+0x14]
            0xff, 0x75, 0x10, //                    push [ebp+0x10]
            0xff, 0x75, 0x0c, //                    push [ebp+0xc]
            0xff, 0x75, 0x08, //                    push [ebp+8]
            0x8b, 0x4d, 0x18, //                    mov ecx, [ebp+0x18]
            0xff, 0xd1, //                          call ecx
            0x64, 0x8b, 0x25, 0, 0, 0, 0, //        mov esp, fs:[0]
            0x64, 0x8f, 0x05, 0, 0, 0, 0, //        pop fs:[0]
            0x8b, 0xe5, //                          mov esp, ebp
            0x5d, //                                pop ebp
            0xc2, 0x14, 0x00, //                    ret 0x14
        ])
    })
}

fn stub() -> u32 {
    static S: OnceLock<u32> = OnceLock::new();
    *S.get_or_init(|| {
        let t = SEH_TRAP.to_le_bytes();
        let x = exec().to_le_bytes();
        crate::api::stub(&[
            0x8b, 0x43, 0x08, // mov eax, [ebx+8]      ; the frame
            0x83, 0xf8, 0xff, // cmp eax, -1
            0x74, 0x14, //       je report             ; (eax = -1: none left)
            0xff, 0x70, 0x04, // push [eax+4]          ; handler
            0x8d, 0x4b, 0x0c, // lea ecx, [ebx+12]
            0x51, //             push ecx              ; dispatcher context
            0xff, 0x73, 0x04, // push [ebx+4]          ; CONTEXT
            0x50, //             push eax              ; frame
            0xff, 0x33, //       push [ebx]            ; EXCEPTION_RECORD
            0xb9, x[0], x[1], x[2], x[3], // mov ecx, exec
            0xff, 0xd1, //       call ecx
            0x8d, 0x63, 0xf0, // report: lea esp, [ebx-16]
            0x50, //             push eax              ; verdict
            0x53, //             push ebx              ; block
            0xb8, t[0], t[1], t[2], t[3], // mov eax, SEH_TRAP
            0xff, 0xd0, //       call eax
            0xeb, 0xd6, //       jmp (start)
        ])
    })
}

pub fn save_context(cpu: &mut Cpu, ctx: u32) {
    crate::mem::fill(ctx, CONTEXT_SIZE, 0);
    w32(ctx, 0x1003f);
    let r = &cpu.regs;
    for (off, v) in [(0x9c, r[EDI]), (0xa0, r[ESI]), (0xa4, r[EBX]), (0xa8, r[EDX]), (0xac, r[ECX]), (0xb0, r[EAX]), (0xb4, r[EBP])] {
        w32(ctx + off, v);
    }
    w32(ctx + 0xb8, cpu.eip);
    w32(ctx + 0xbc, 0x23);
    w32(ctx + 0xc0, cpu.flags());
    w32(ctx + 0xc4, cpu.regs[ESP]);
    w32(ctx + 0xc8, 0x2b);
    w32(ctx + 0x90, 0x53);
    for o in [0x8c, 0x94, 0x98] {
        w32(ctx + o, 0x2b);
    }
    // FloatSave: control/status/tag words; ExtendedRegisters: FXSAVE
    w32(ctx + 0x1c, cpu.fpu.cw as u32 | 0xffff_0000);
    w32(ctx + 0x20, cpu.fpu.status() as u32 | 0xffff_0000);
    w32(ctx + 0x24, cpu.fpu.tag_word() as u32 | 0xffff_0000);
    let _ = koji::fxsave(cpu, &mut Guest, ctx + 0xcc);
}

pub fn load_context(cpu: &mut Cpu, ctx: u32) {
    let flags = r32(ctx);
    if flags & 2 != 0 || flags == 0 {
        for (off, i) in [(0x9c, EDI), (0xa0, ESI), (0xa4, EBX), (0xa8, EDX), (0xac, ECX), (0xb0, EAX)] {
            cpu.regs[i] = r32(ctx + off);
        }
    }
    cpu.regs[EBP] = r32(ctx + 0xb4);
    cpu.eip = r32(ctx + 0xb8);
    cpu.eflags = r32(ctx + 0xc0) | 2;
    cpu.lazy.op = 0;
    cpu.regs[ESP] = r32(ctx + 0xc4);
    if flags & 0x20 != 0 {
        let _ = koji::fxrstor(cpu, &mut Guest, ctx + 0xcc);
    }
}

/// Raises an exception with `cpu` as the context (EIP = where it happened) and runs the
/// handlers from the next instruction the guest executes.
pub fn raise(cpu: &mut Cpu, code: u32, flags: u32, addr: u32, params: &[u32]) {
    if crate::env_on!("SAKE_TRACE_SEH") {
        crate::api::dump_ring("the exception");
        let st: Vec<String> = (0..8).map(|k| format!("{:x}", r32(cpu.regs[ESP] + 4 * k))).collect();
        eprintln!(
            "sake: exception {code:08x} at {addr:#x} params {params:x?} (esp {:#x}: {}; ebp {:#x})",
            cpu.regs[ESP],
            st.join(" "),
            cpu.regs[5]
        );
        // (the callers, by the ebp chain: where a C++ throw came from)
        let (mut bp, mut rets) = (cpu.regs[5], String::new());
        for _ in 0..8 {
            if bp < 0x1000 || bp & 3 != 0 || crate::mem::find(bp).is_none() {
                break;
            }
            rets += &format!(" {:x}", r32(bp + 4));
            bp = r32(bp);
        }
        eprintln!("sake:   returns{rets}");
        let mut f = r32(crate::cur().teb);
        let mut chain = String::new();
        for _ in 0..12 {
            if f == 0xffff_ffff || f == 0 {
                break;
            }
            chain += &format!(" {f:x}:{:x}", r32(f + 4));
            f = r32(f);
        }
        eprintln!("sake:   frames{chain}");
        if code == 0x0eef_face && params.len() >= 3 {
            let o = params[2];
            let w: Vec<String> = (0..8).map(|k| format!("{:x}", r32(o + 4 * k))).collect();
            eprintln!("sake:   object {}", w.join(" "));
        }
    }
    let mut sp = (cpu.regs[ESP] - 0x100) & !15;
    sp -= CONTEXT_SIZE;
    let ctx = sp & !15;
    let rec = ctx - RECORD_SIZE;
    let block = rec - 16;
    save_context(cpu, ctx);
    crate::mem::fill(rec, RECORD_SIZE, 0);
    w32(rec, code);
    w32(rec + 4, flags);
    w32(rec + 12, addr);
    w32(rec + 16, params.len().min(15) as u32);
    for (i, p) in params.iter().take(15).enumerate() {
        w32(rec + 20 + 4 * i as u32, *p);
    }
    w32(block, rec);
    w32(block + 4, ctx);
    w32(block + 8, r32(crate::cur().teb));
    w32(block + 12, 0);
    cpu.regs[EBX] = block;
    cpu.regs[ESP] = block - 16;
    // vectored handlers first (AddVectoredExceptionHandler): -1 continues at the context
    let veh: Vec<u32> = VEH.lock().unwrap().iter().map(|&(_, f)| f).collect();
    if !veh.is_empty() {
        let ptrs = block - 8;
        w32(ptrs, rec);
        w32(ptrs + 4, ctx);
        for f in veh {
            if crate::call(cpu, f, &[ptrs]) == 0xffff_ffff {
                continue_handlers(cpu, ptrs);
                load_context(cpu, ctx);
                return;
            }
        }
    }
    cpu.eip = stub();
}

/// Vectored exception handlers: (handle, function), called in order.
static VEH: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());

/// AddVectoredExceptionHandler(first, handler) -> handle
pub fn add_veh(c: &mut Ctx) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0x7e00);
    let h = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut v = VEH.lock().unwrap();
    if c.a(0) != 0 { v.insert(0, (h, c.a(1))) } else { v.push((h, c.a(1))) }
    h
}

/// The vectored handlers' functions, in calling order.
pub fn veh_list() -> Vec<u32> {
    VEH.lock().unwrap().iter().map(|&(_, f)| f).collect()
}

/// RemoveVectoredExceptionHandler(handle)
pub fn remove_veh(c: &mut Ctx) -> u32 {
    let mut v = VEH.lock().unwrap();
    let n = v.len();
    v.retain(|&(h, _)| h != c.a(0));
    (v.len() != n) as u32
}

/// Vectored continue handlers: called, in order, once a handler continues execution (until
/// one of them returns EXCEPTION_CONTINUE_EXECUTION too).
static VCH: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());
pub fn continue_handlers(cpu: &mut Cpu, ptrs: u32) {
    let list: Vec<u32> = VCH.lock().unwrap().iter().map(|&(_, f)| f).collect();
    for f in list {
        if crate::call(cpu, f, &[ptrs]) == 0xffff_ffff {
            break;
        }
    }
}
fn remove_vch(c: &mut Ctx) -> u32 {
    let mut v = VCH.lock().unwrap();
    let n = v.len();
    v.retain(|&(h, _)| h != c.a(0));
    (v.len() != n) as u32
}

pub fn raise_fault(cpu: &mut Cpu, code: u32, addr: u32, write: bool) {
    let eip = cpu.eip;
    raise(cpu, code, 0, eip, &[write as u32, addr]);
}

/// UnhandledExceptionFilter set by the program.
pub static TOP_FILTER: AtomicU32 = AtomicU32::new(0);

/// SEH_TRAP: the stub reports `verdict` for the block's frame.
pub fn handler_done(cpu: &mut Cpu) {
    let esp = cpu.regs[ESP];
    let block = r32(esp + 4);
    let verdict = r32(esp + 8);
    let (rec, ctx, frame) = (r32(block), r32(block + 4), r32(block + 8));
    let ret = |cpu: &mut Cpu| {
        cpu.eip = r32(esp);
        cpu.regs[ESP] = esp + 12;
    };
    if frame == 0xffff_ffff || frame == 0 {
        // no handler took it
        let filter = TOP_FILTER.load(Ordering::Relaxed);
        if filter != 0 {
            let ptrs = block - 8;
            w32(ptrs, rec);
            w32(ptrs + 4, ctx);
            cpu.regs[ESP] = ptrs - 16;
            let v = crate::call(cpu, filter, &[ptrs]);
            if v == 0xffff_ffff {
                load_context(cpu, ctx);
                return;
            }
        }
        let code = r32(rec);
        let msg = format!(
            "unhandled exception {code:08x} at {:#x} (params {:x} {:x}), eip {:#x} esp {:#x}",
            r32(rec + 12),
            r32(rec + 20),
            r32(rec + 24),
            r32(ctx + 0xb8),
            r32(ctx + 0xc4)
        );
        load_context(cpu, ctx);
        crate::fatal(cpu, &msg);
    }
    if crate::env_on!("SAKE_TRACE_SEH") {
        eprintln!("sake: handler of frame {frame:#x} says {verdict}");
    }
    match verdict {
        0 => load_context(cpu, ctx),
        _ => {
            w32(block + 8, r32(frame));
            ret(cpu);
        }
    }
}

/// RtlUnwind(TargetFrame, TargetIp, ExceptionRecord, ReturnValue)
pub fn rtl_unwind(c: &mut Ctx) -> u32 {
    let (target, _ip, rec_in, retval) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if crate::env_on!("SAKE_TRACE_SEH") {
        eprintln!("sake: RtlUnwind to {target:#x} ip {_ip:#x} rec {rec_in:#x} from {:#x}, esp {:#x} ebp {:#x}", r32(c.esp), c.esp, c.cpu.regs[EBP]);
    }
    let teb = crate::cur().teb;
    // scratch below the caller's stack: a record and a context for the handlers
    let save_esp = c.cpu.regs[ESP];
    let scratch = ((save_esp - 0x100) & !15) - CONTEXT_SIZE - RECORD_SIZE;
    let ctx = scratch + RECORD_SIZE;
    let rec = if rec_in != 0 {
        rec_in
    } else {
        crate::mem::fill(scratch, RECORD_SIZE, 0);
        w32(scratch, 0xc000_0027);
        w32(scratch + 12, r32(c.esp));
        scratch
    };
    w32(rec + 4, r32(rec + 4) | if target == 0 { 6 } else { 2 });
    save_context(c.cpu, ctx);
    c.cpu.regs[ESP] = scratch - 16;
    let mut frame = r32(teb);
    while frame != 0xffff_ffff && frame != 0 && frame != target {
        let handler = r32(frame + 4);
        let disp = scratch - 4;
        w32(disp, 0);
        crate::call(c.cpu, exec(), &[rec, frame, ctx, disp, handler]);
        frame = r32(frame);
        w32(teb, frame);
    }
    c.cpu.regs[ESP] = save_esp;
    retval
}

/// RaiseException(code, flags, nargs, args)
pub fn raise_exception(c: &mut Ctx) -> u32 {
    let (code, flags, n, args) = (c.a(0), c.a(1), c.a(2), c.a(3));
    // (no array: no parameters)
    let n = if args == 0 { 0 } else { n };
    let params: Vec<u32> = (0..n.min(15)).map(|i| r32(args + 4 * i)).collect();
    // the context: as if RaiseException had returned
    let ret = r32(c.esp);
    c.cpu.eip = ret;
    c.cpu.regs[ESP] = c.esp + 4 + 16;
    raise(c.cpu, code, flags & 1, ret, &params);
    c.jumped = true;
    0
}

fn continue_handler(c: &mut Ctx) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0x7f00);
    let h = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut v = VCH.lock().unwrap();
    if c.a(0) != 0 { v.insert(0, (h, c.a(1))) } else { v.push((h, c.a(1))) }
    h
}

pub fn register() {
    use crate::api::implement;
    for d in ["kernel32", "ntdll"] {
        let p = if d == "ntdll" { "Rtl" } else { "" };
        implement(d, &format!("{p}AddVectoredExceptionHandler"), add_veh);
        implement(d, &format!("{p}RemoveVectoredExceptionHandler"), remove_veh);
        implement(d, &format!("{p}AddVectoredContinueHandler"), continue_handler);
        implement(d, &format!("{p}RemoveVectoredContinueHandler"), remove_vch);
    }
    for d in ["msvcrt", "ucrtbase"] {
        implement(d, "_except_handler3", except_handler3);
        implement(d, "_except_handler4_common", except_handler4);
    }
}

// ---- the MSVC CRT's frame handlers (_except_handler3 / _except_handler4_common) ----
//
// A frame: [next, handler, scope table (EH4: xor the cookie), try level, the function's EBP];
// frame - 4 gets the EXCEPTION_POINTERS for GetExceptionInformation. Scope table entries:
// (enclosing level, filter or 0 for __finally, handler / __finally body); EH4's start after a
// 16-byte cookie header. Filters and __finally bodies run with the function's EBP.

/// stdcall (fn, ebp): calls fn with EBP = ebp, keeping the callee-saved registers
fn call_ebp() -> u32 {
    static S: OnceLock<u32> = OnceLock::new();
    *S.get_or_init(|| {
        crate::api::stub(&[
            0x55, 0x53, 0x56, 0x57, //         push ebp, ebx, esi, edi
            0x8b, 0x44, 0x24, 0x14, //         mov eax, [esp+20]
            0x8b, 0x6c, 0x24, 0x18, //         mov ebp, [esp+24]
            0xff, 0xd0, //                     call eax
            0x5f, 0x5e, 0x5b, 0x5d, //         pop edi, esi, ebx, ebp
            0xc2, 0x08, 0x00, //               ret 8
        ])
    })
}

/// runs the __finally bodies from the frame's try level down to `to`
fn local_unwind(c: &mut Ctx, frame: u32, table: u32, first: u32, none: u32, to: u32) {
    loop {
        let level = r32(frame + 12);
        if level == none || level == to {
            break;
        }
        let e = table + first + 12 * level;
        w32(frame + 12, r32(e));
        if r32(e + 4) == 0 {
            let body = r32(e + 8);
            c.call(call_ebp(), &[body, frame + 16]);
        }
    }
}

fn except_handler(c: &mut Ctx, v4: bool) -> u32 {
    let k = if v4 { 2 } else { 0 };
    let (rec, frame, ctx) = (c.a(k), c.a(k + 1), c.a(k + 2));
    let (table, first, none) = if v4 { (r32(frame + 8) ^ r32(c.a(0)), 16, 0xffff_fffe) } else { (r32(frame + 8), 0, 0xffff_ffff) };
    if r32(rec + 4) & 6 != 0 {
        // unwinding: this frame's __finally bodies
        local_unwind(c, frame, table, first, none, none);
        return 1;
    }
    let ptrs = c.esp - 0x40;
    w32(ptrs, rec);
    w32(ptrs + 4, ctx);
    w32(frame - 4, ptrs);
    let mut level = r32(frame + 12);
    while level != none {
        let e = table + first + 12 * level;
        let filter = r32(e + 4);
        if filter != 0 {
            let v = c.call(call_ebp(), &[filter, frame + 16]) as i32;
            if v < 0 {
                return 0; // continue execution
            }
            if v > 0 {
                // global unwind of the frames above, ours down to this level, then the handler
                let unwind = crate::api::export("kernel32", "RtlUnwind");
                c.call(unwind, &[frame, 0, rec, 0]);
                local_unwind(c, frame, table, first, none, level);
                w32(frame + 12, r32(e));
                c.cpu.regs[EBP] = frame + 16;
                c.cpu.eip = r32(e + 8);
                c.jumped = true;
                return 0;
            }
        }
        level = r32(e);
    }
    1
}
pub fn except_handler3(c: &mut Ctx) -> u32 {
    except_handler(c, false)
}
pub fn except_handler4(c: &mut Ctx) -> u32 {
    except_handler(c, true)
}
