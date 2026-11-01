//! Structured exception handling on the guest's fs:[0] chain.
//!
//! The dispatch loop runs as guest code (a stub below), so a handler that catches the
//! exception (RtlUnwind, then a jump into its function) leaves no host frame behind; the
//! stub reports each handler's verdict to the host through SEH_TRAP.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use vnx86::{Cpu, Fault, Memory, EAX, EBP, EBX, ECX, EDI, EDX, ESI, ESP};

use crate::api::{Ctx, SEH_TRAP};
use crate::mem::{r32, w32};

pub const CONTEXT_SIZE: u32 = 0x2cc;
const RECORD_SIZE: u32 = 0x50;

/// Guest memory for vnx86's FXSAVE helpers.
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
    let _ = vnx86::fxsave(cpu, &mut Guest, ctx + 0xcc);
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
        let _ = vnx86::fxrstor(cpu, &mut Guest, ctx + 0xcc);
    }
}

/// Raises an exception with `cpu` as the context (EIP = where it happened) and runs the
/// handlers from the next instruction the guest executes.
pub fn raise(cpu: &mut Cpu, code: u32, flags: u32, addr: u32, params: &[u32]) {
    if std::env::var_os("VNWIN_TRACE_SEH").is_some() {
        crate::api::dump_ring("the exception");
        let st: Vec<String> = (0..8).map(|k| format!("{:x}", r32(cpu.regs[ESP] + 4 * k))).collect();
        eprintln!(
            "vnwin: exception {code:08x} at {addr:#x} params {params:x?} (esp {:#x}: {}; ebp {:#x})",
            cpu.regs[ESP],
            st.join(" "),
            cpu.regs[5]
        );
        let mut f = r32(crate::cur().teb);
        let mut chain = String::new();
        for _ in 0..12 {
            if f == 0xffff_ffff || f == 0 {
                break;
            }
            chain += &format!(" {f:x}:{:x}", r32(f + 4));
            f = r32(f);
        }
        eprintln!("vnwin:   frames{chain}");
        if code == 0x0eef_face && params.len() >= 3 {
            let o = params[2];
            let w: Vec<String> = (0..8).map(|k| format!("{:x}", r32(o + 4 * k))).collect();
            eprintln!("vnwin:   object {}", w.join(" "));
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
    cpu.eip = stub();
}

pub fn raise_fault(cpu: &mut Cpu, addr: u32, write: bool) {
    let eip = cpu.eip;
    raise(cpu, 0xc000_0005, 0, eip, &[write as u32, addr]);
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
    if std::env::var_os("VNWIN_TRACE_SEH").is_some() {
        eprintln!("vnwin: handler of frame {frame:#x} says {verdict}");
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
    if std::env::var_os("VNWIN_TRACE_SEH").is_some() {
        eprintln!("vnwin: RtlUnwind to {target:#x} ip {_ip:#x} rec {rec_in:#x} from {:#x}, esp {:#x} ebp {:#x}", r32(c.esp), c.esp, c.cpu.regs[EBP]);
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
    let params: Vec<u32> = (0..n.min(15)).map(|i| r32(args + 4 * i)).collect();
    // the context: as if RaiseException had returned
    let ret = r32(c.esp);
    c.cpu.eip = ret;
    c.cpu.regs[ESP] = c.esp + 4 + 16;
    raise(c.cpu, code, flags & 1, ret, &params);
    c.jumped = true;
    0
}
