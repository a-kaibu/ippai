//! x86 fast paths for the hottest imports. A trap leaves the JIT's code for the Rust
//! dispatcher and back (KiriKiri enters and leaves critical sections ~70k times a second):
//! these stubs do the common case in guest code and jump to the trap only for the rest
//! (contention, a waiter to wake, a TLS slot past the TEB's 64). They keep k32's layouts:
//! a critical section's OwningThread is its lock word (CAS), +16 counts waiters; the last
//! error at TEB+0x34, TLS slots at TEB+0xE10.
//!
//! VNWIN_NO_FAST=1 binds the traps instead.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::api::Ctx;
use crate::mem;

/// A tiny assembler: bytes, labels, rel8/rel32 fixups.
#[derive(Default)]
struct Asm {
    b: Vec<u8>,
    labels: HashMap<&'static str, usize>,
    rel8: Vec<(usize, &'static str)>,
    rel32: Vec<(usize, u32)>,
}

impl Asm {
    fn x(&mut self, bytes: &[u8]) -> &mut Self {
        self.b.extend_from_slice(bytes);
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// a short jump opcode (0xEB, 0x7x) to `label`
    fn j8(&mut self, op: u8, label: &'static str) -> &mut Self {
        self.b.push(op);
        self.rel8.push((self.b.len(), label));
        self.b.push(0);
        self
    }
    fn label(&mut self, l: &'static str) -> &mut Self {
        self.labels.insert(l, self.b.len());
        self
    }
    /// jmp to an absolute address
    fn jmp(&mut self, to: u32) -> &mut Self {
        self.b.push(0xe9);
        self.rel32.push((self.b.len(), to));
        self.u32(0)
    }
    /// Places the code in guest memory.
    fn place(&mut self) -> u32 {
        let at = mem::heap_alloc(self.b.len() as u32, true);
        for &(p, l) in &self.rel8 {
            let d = self.labels[l] as i64 - (p as i64 + 1);
            assert!((-128..128).contains(&d), "fast: jump too far");
            self.b[p] = d as i8 as u8;
        }
        for &(p, to) in &self.rel32 {
            let next = at.wrapping_add(p as u32 + 4);
            self.b[p..p + 4].copy_from_slice(&to.wrapping_sub(next).to_le_bytes());
        }
        for (i, &v) in self.b.iter().enumerate() {
            mem::w8(at + i as u32, v);
        }
        at
    }
}

fn enter_cs(trap: u32) -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x4c, 0x24, 0x04]) // mov ecx,[esp+4]
        .x(&[0x64, 0x8b, 0x15, 0x24, 0, 0, 0]) // mov edx,fs:[0x24] (thread id)
        .x(&[0x39, 0x51, 0x0c]) // cmp [ecx+0xc],edx
        .j8(0x74, "again") // je
        .x(&[0x31, 0xc0]) // xor eax,eax
        .x(&[0xf0, 0x0f, 0xb1, 0x51, 0x0c]) // lock cmpxchg [ecx+0xc],edx
        .j8(0x75, "slow") // jne
        .x(&[0xc7, 0x41, 0x08, 1, 0, 0, 0]) // mov dword [ecx+8],1
        .x(&[0xc7, 0x41, 0x04, 0, 0, 0, 0]) // mov dword [ecx+4],0
        .x(&[0xc2, 0x04, 0x00]) // ret 4
        .label("again")
        .x(&[0xff, 0x41, 0x08]) // inc dword [ecx+8]
        .x(&[0xc2, 0x04, 0x00])
        .label("slow")
        .jmp(trap);
    a.place()
}

fn leave_cs(wake: u32) -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x4c, 0x24, 0x04]) // mov ecx,[esp+4]
        .x(&[0x8b, 0x41, 0x08]) // mov eax,[ecx+8]
        .x(&[0x83, 0xf8, 0x01]) // cmp eax,1
        .j8(0x7f, "nested") // jg
        .x(&[0xc7, 0x41, 0x08, 0, 0, 0, 0]) // mov dword [ecx+8],0
        .x(&[0xc7, 0x41, 0x04, 0xff, 0xff, 0xff, 0xff]) // mov dword [ecx+4],-1
        .x(&[0x31, 0xc0]) // xor eax,eax
        .x(&[0xf0, 0x87, 0x41, 0x0c]) // lock xchg [ecx+0xc],eax
        .x(&[0x83, 0x79, 0x10, 0x00]) // cmp dword [ecx+0x10],0
        .j8(0x75, "wake") // jne
        .x(&[0xc2, 0x04, 0x00]) // ret 4
        .label("nested")
        .x(&[0x48]) // dec eax
        .x(&[0x89, 0x41, 0x08]) // mov [ecx+8],eax
        .x(&[0xc2, 0x04, 0x00])
        .label("wake")
        .jmp(wake);
    a.place()
}

fn is_dbcs_lead_byte() -> u32 {
    let table = mem::heap_alloc(256, true);
    for b in 0..=255u8 {
        mem::w8(table + b as u32, crate::text::is_lead(b) as u8);
    }
    let mut a = Asm::default();
    a.x(&[0x0f, 0xb6, 0x44, 0x24, 0x04]) // movzx eax,byte [esp+4]
        .x(&[0x0f, 0xb6, 0x80]) // movzx eax,byte [eax+table]
        .u32(table)
        .x(&[0xc2, 0x04, 0x00]);
    a.place()
}

fn get_last_error() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x64, 0xa1, 0x34, 0, 0, 0]).x(&[0xc3]); // mov eax,fs:[0x34]; ret
    a.place()
}

fn set_last_error() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, 0x04]) // mov eax,[esp+4]
        .x(&[0x64, 0xa3, 0x34, 0, 0, 0]) // mov fs:[0x34],eax
        .x(&[0xc2, 0x04, 0x00]);
    a.place()
}

fn tls_get_value(trap: u32) -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, 0x04]) // mov eax,[esp+4]
        .x(&[0x83, 0xf8, 0x40]) // cmp eax,64
        .j8(0x73, "slow") // jae
        .x(&[0x64, 0xc7, 0x05, 0x34, 0, 0, 0, 0, 0, 0, 0]) // mov dword fs:[0x34],0
        .x(&[0x64, 0x8b, 0x04, 0x85, 0x10, 0x0e, 0, 0]) // mov eax,fs:[eax*4+0xe10]
        .x(&[0xc2, 0x04, 0x00])
        .label("slow")
        .jmp(trap);
    a.place()
}

/// msvcrt's _errno / __doserrno: per-thread words in the TEB's free page (crt::ERRNO_TEB)
fn errno_ptr(off: u32) -> u32 {
    let mut a = Asm::default();
    a.x(&[0x64, 0xa1, 0x18, 0, 0, 0]) // mov eax,fs:[0x18]
        .x(&[0x05])
        .u32(crate::crt::ERRNO_TEB + off) // add eax,imm32
        .x(&[0xc3]);
    a.place()
}

fn cs_wake(c: &mut Ctx) -> u32 {
    crate::k32::cs_wake(c.a(0));
    0
}

static STUBS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// The address to bind for `dll!name`, whose trap is `trap`: a stub, or the trap.
pub fn bind(dll: &str, name: &str, trap: u32) -> u32 {
    let key = crate::api::dll_key(dll);
    if !matches!(key.as_str(), "kernel32" | "kernelbase" | "ntdll" | "msvcrt") {
        return trap;
    }
    if key == "msvcrt" && !matches!(name, "_errno" | "__doserrno") {
        return trap;
    }
    if std::env::var_os("VNWIN_NO_FAST").is_some() {
        return trap;
    }
    let mut g = STUBS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(&a) = m.get(name) {
        return a;
    }
    let a = match name {
        "EnterCriticalSection" | "RtlEnterCriticalSection" => enter_cs(trap),
        "LeaveCriticalSection" | "RtlLeaveCriticalSection" => leave_cs(crate::api::host_fn("cs_wake", cs_wake, 4)),
        "IsDBCSLeadByte" => is_dbcs_lead_byte(),
        "GetLastError" | "RtlGetLastWin32Error" => get_last_error(),
        "SetLastError" | "RtlSetLastWin32Error" => set_last_error(),
        "TlsGetValue" => tls_get_value(trap),
        "_errno" => errno_ptr(0),
        "__doserrno" => errno_ptr(4),
        _ => return trap,
    };
    m.insert(name.to_string(), a);
    a
}
