//! x86 fast paths for the hottest imports. A trap leaves the JIT's code for the Rust
//! dispatcher and back (KiriKiri enters and leaves critical sections ~70k times a second):
//! these stubs do the common case in guest code and jump to the trap only for the rest
//! (contention, a waiter to wake, a TLS slot past the TEB's 64). They keep k32's layouts:
//! a critical section's OwningThread is its lock word (CAS), +16 counts waiters; the last
//! error at TEB+0x34, TLS slots at TEB+0xE10.
//!
//! SAKE_NO_FAST=1 binds the traps instead.

use crate::HashMap;
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

// user32's RECT helpers (CatSystem2 calls IntersectRect / IsRectEmpty / SetRectEmpty ~150k
// times a second)

fn is_rect_empty() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x4c, 0x24, 0x04]) // mov ecx,[esp+4]
        .x(&[0x8b, 0x41, 0x08, 0x3b, 0x01]) // mov eax,[ecx+8]; cmp eax,[ecx]
        .j8(0x7e, "empty")
        .x(&[0x8b, 0x41, 0x0c, 0x3b, 0x41, 0x04]) // mov eax,[ecx+12]; cmp eax,[ecx+4]
        .j8(0x7e, "empty")
        .x(&[0x31, 0xc0, 0xc2, 0x04, 0x00]) // xor eax,eax; ret 4
        .label("empty")
        .x(&[0xb8, 1, 0, 0, 0, 0xc2, 0x04, 0x00]);
    a.place()
}

fn set_rect_empty() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, 0x04, 0x31, 0xc9]) // mov eax,[esp+4]; xor ecx,ecx
        .x(&[0x89, 0x08, 0x89, 0x48, 0x04, 0x89, 0x48, 0x08, 0x89, 0x48, 0x0c])
        .x(&[0xb8, 1, 0, 0, 0, 0xc2, 0x04, 0x00]);
    a.place()
}

fn set_rect() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, 0x04]); // mov eax,[esp+4]
    for k in 0..4u8 {
        a.x(&[0x8b, 0x4c, 0x24, 8 + 4 * k, 0x89, 0x48, 4 * k]); // mov ecx,[esp+8+4k]; mov [eax+4k],ecx
    }
    a.x(&[0xb8, 1, 0, 0, 0, 0xc2, 0x14, 0x00]);
    a.place()
}

fn copy_rect() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, 0x04, 0x8b, 0x54, 0x24, 0x08]); // mov eax,[esp+4]; mov edx,[esp+8]
    for k in 0..4u8 {
        a.x(&[0x8b, 0x4a, 4 * k, 0x89, 0x48, 4 * k]); // mov ecx,[edx+4k]; mov [eax+4k],ecx
    }
    a.x(&[0xb8, 1, 0, 0, 0, 0xc2, 0x08, 0x00]);
    a.place()
}

fn offset_rect() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, 0x04, 0x8b, 0x4c, 0x24, 0x08, 0x8b, 0x54, 0x24, 0x0c]) // eax=rc ecx=dx edx=dy
        .x(&[0x01, 0x08, 0x01, 0x50, 0x04, 0x01, 0x48, 0x08, 0x01, 0x50, 0x0c]) // add [eax],ecx; [eax+4],edx; ...
        .x(&[0xb8, 1, 0, 0, 0, 0xc2, 0x0c, 0x00]);
    a.place()
}

fn intersect_rect() -> u32 {
    let mut a = Asm::default();
    a.x(&[0x56, 0x57]) // push esi; push edi
        .x(&[0x8b, 0x7c, 0x24, 0x0c, 0x8b, 0x54, 0x24, 0x10, 0x8b, 0x4c, 0x24, 0x14]); // edi=dst edx=a ecx=b
    for (off, max, l) in [(0u8, true, "s0"), (4, true, "s1"), (8, false, "s2"), (12, false, "s3")] {
        a.x(&[0x8b, 0x42, off, 0x8b, 0x71, off, 0x39, 0xf0]) // mov eax,[edx+off]; mov esi,[ecx+off]; cmp eax,esi
            .j8(if max { 0x7d } else { 0x7e }, l)
            .x(&[0x89, 0xf0]) // mov eax,esi
            .label(l)
            .x(&[0x89, 0x47, off]); // mov [edi+off],eax
    }
    a.x(&[0x8b, 0x47, 0x08, 0x3b, 0x07]) // mov eax,[edi+8]; cmp eax,[edi]
        .j8(0x7e, "empty")
        .x(&[0x8b, 0x47, 0x0c, 0x3b, 0x47, 0x04])
        .j8(0x7e, "empty")
        .x(&[0xb8, 1, 0, 0, 0, 0x5f, 0x5e, 0xc2, 0x0c, 0x00])
        .label("empty")
        .x(&[0x31, 0xc0, 0x89, 0x07, 0x89, 0x47, 0x04, 0x89, 0x47, 0x08, 0x89, 0x47, 0x0c, 0x5f, 0x5e, 0xc2, 0x0c, 0x00]);
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

// the heap's small blocks (mem::heap_alloc_small): pop/push the per-class free lists. YU-RIS
// calls HeapAlloc/HeapFree ~140k times a second.

/// Takes the lock word at `t`, using eax (`edx` false) or edx.
fn spin_lock(a: &mut Asm, t: u32, edx: bool) {
    let (mov, xchg, test) = if edx { (0xba, 0x15, 0xd2) } else { (0xb8, 0x05, 0xc0) };
    a.label("spin")
        .x(&[mov, 1, 0, 0, 0]) // mov reg,1
        .x(&[0x87, xchg])
        .u32(t) // xchg [t],reg
        .x(&[0x85, test]) // test reg,reg
        .j8(0x74, "locked")
        .x(&[0xf3, 0x90]) // pause
        .j8(0xeb, "spin")
        .label("locked");
}

fn unlock(a: &mut Asm, t: u32) {
    a.x(&[0xc7, 0x05]).u32(t).u32(0); // mov dword [t],0
}

/// HeapAlloc(heap, flags, size) (size at esp+12, flags at esp+8, ret 12) or malloc(size)
/// (esp+4, ret 0)
fn slab_alloc(trap: u32, size_at: u8, flags_at: Option<u8>, ret: u16) -> u32 {
    let t = mem::slab_table();
    let mut a = Asm::default();
    a.x(&[0x8b, 0x54, 0x24, size_at]) // mov edx,[esp+size]
        .x(&[0x81, 0xfa])
        .u32(mem::SLAB_MAX) // cmp edx,MAX
        .j8(0x77, "slow") // ja
        .x(&[0x8d, 0x4a, 0x0f, 0xc1, 0xe9, 0x04]) // lea ecx,[edx+15]; shr ecx,4
        .j8(0x75, "class")
        .x(&[0x41]) // inc ecx
        .label("class");
    spin_lock(&mut a, t, false);
    a.x(&[0x8b, 0x04, 0x8d])
        .u32(t) // mov eax,[ecx*4+t]
        .x(&[0x85, 0xc0])
        .j8(0x74, "miss")
        .x(&[0x8b, 0x10]) // mov edx,[eax]
        .x(&[0x89, 0x14, 0x8d])
        .u32(t); // mov [ecx*4+t],edx
    unlock(&mut a, t);
    a.x(&[0x8b, 0x54, 0x24, size_at]) // mov edx,[esp+size]
        .x(&[0x89, 0x50, 0xf8]) // mov [eax-8],edx
        .x(&[0xc7, 0x40, 0xfc])
        .u32(mem::SLAB); // mov dword [eax-4],SLAB
    if let Some(f) = flags_at {
        a.x(&[0xf6, 0x44, 0x24, f, 0x08]) // test byte [esp+flags],8
            .j8(0x75, "zero");
    }
    a.x(&[0xc2]).x(&ret.to_le_bytes());
    if flags_at.is_some() {
        a.label("zero")
            .x(&[0x57, 0x50]) // push edi; push eax
            .x(&[0x89, 0xc7, 0x89, 0xd1, 0x31, 0xc0]) // mov edi,eax; mov ecx,edx; xor eax,eax
            .x(&[0xf3, 0xaa]) // rep stosb
            .x(&[0x58, 0x5f]) // pop eax; pop edi
            .x(&[0xc2])
            .x(&ret.to_le_bytes());
    }
    a.label("miss");
    unlock(&mut a, t);
    a.label("slow").jmp(trap);
    a.place()
}

/// HeapFree(heap, flags, p) (p at esp+12, returns TRUE, ret 12) or free(p) (esp+4, ret 0)
fn slab_free(trap: u32, p_at: u8, ret: u16) -> u32 {
    let t = mem::slab_table();
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, p_at]) // mov eax,[esp+p]
        .x(&[0x85, 0xc0])
        .j8(0x74, "done")
        .x(&[0x81, 0x78, 0xfc])
        .u32(mem::SLAB) // cmp dword [eax-4],SLAB
        .j8(0x75, "slow")
        .x(&[0x8b, 0x48, 0xf8]) // mov ecx,[eax-8]
        .x(&[0x83, 0xc1, 0x0f, 0xc1, 0xe9, 0x04]) // add ecx,15; shr ecx,4
        .j8(0x75, "class")
        .x(&[0x41])
        .label("class");
    spin_lock(&mut a, t, true);
    a.x(&[0xc7, 0x40, 0xfc])
        .u32(mem::SLAB_FREE) // mov dword [eax-4],SLAB_FREE
        .x(&[0x8b, 0x14, 0x8d])
        .u32(t) // mov edx,[ecx*4+t]
        .x(&[0x89, 0x10]) // mov [eax],edx
        .x(&[0x89, 0x04, 0x8d])
        .u32(t); // mov [ecx*4+t],eax
    unlock(&mut a, t);
    a.label("done").x(&[0xb8, 1, 0, 0, 0, 0xc2]).x(&ret.to_le_bytes());
    a.label("slow").jmp(trap);
    a.place()
}

/// HeapReAlloc(heap, flags, p, size) of a small block within its class: in place
fn slab_realloc(trap: u32) -> u32 {
    let mut a = Asm::default();
    a.x(&[0x8b, 0x44, 0x24, 0x0c]) // mov eax,[esp+12]
        .x(&[0x85, 0xc0])
        .j8(0x74, "slow")
        .x(&[0x81, 0x78, 0xfc])
        .u32(mem::SLAB) // cmp dword [eax-4],SLAB
        .j8(0x75, "slow")
        .x(&[0x8b, 0x54, 0x24, 0x10]) // mov edx,[esp+16]
        .x(&[0x81, 0xfa])
        .u32(mem::SLAB_MAX)
        .j8(0x77, "slow")
        .x(&[0x8d, 0x4a, 0x0f, 0xc1, 0xe9, 0x04]) // lea ecx,[edx+15]; shr ecx,4
        .x(&[0x8b, 0x50, 0xf8, 0x83, 0xc2, 0x0f, 0xc1, 0xea, 0x04]) // mov edx,[eax-8]; add edx,15; shr edx,4
        .j8(0x75, "k")
        .x(&[0x42]) // inc edx
        .label("k")
        .x(&[0x39, 0xd1]) // cmp ecx,edx
        .j8(0x77, "slow") // ja
        .x(&[0x8b, 0x54, 0x24, 0x10]) // mov edx,[esp+16]
        .x(&[0x3b, 0x50, 0xf8]) // cmp edx,[eax-8]
        .j8(0x76, "store") // jbe
        .x(&[0xf6, 0x44, 0x24, 0x08, 0x08]) // test byte [esp+8],8
        .j8(0x75, "slow")
        .label("store")
        .x(&[0x89, 0x50, 0xf8]) // mov [eax-8],edx
        .x(&[0xc2, 0x10, 0x00])
        .label("slow")
        .jmp(trap);
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
    if let Some(a) = crate::guest::lookup(&key, name) {
        return a;
    }
    if !matches!(key.as_str(), "kernel32" | "kernelbase" | "ntdll" | "msvcrt" | "user32") {
        return trap;
    }
    if key == "user32" && !matches!(name, "IsRectEmpty" | "SetRectEmpty" | "IntersectRect" | "SetRect" | "CopyRect" | "OffsetRect") {
        return trap;
    }
    if key == "msvcrt" && !matches!(name, "_errno" | "__doserrno" | "malloc" | "free") {
        return trap;
    }
    if std::env::var_os("SAKE_NO_FAST").is_some() {
        return trap;
    }
    let mut g = STUBS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
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
        "IsRectEmpty" => is_rect_empty(),
        "SetRectEmpty" => set_rect_empty(),
        "IntersectRect" => intersect_rect(),
        "SetRect" => set_rect(),
        "CopyRect" => copy_rect(),
        "OffsetRect" => offset_rect(),
        "__doserrno" => errno_ptr(4),
        "HeapAlloc" | "RtlAllocateHeap" => slab_alloc(trap, 12, Some(8), 12),
        "HeapFree" | "RtlFreeHeap" => slab_free(trap, 12, 12),
        "HeapReAlloc" | "RtlReAllocateHeap" => slab_realloc(trap),
        "malloc" => slab_alloc(trap, 4, None, 0),
        "free" => slab_free(trap, 4, 0),
        _ => return trap,
    };
    m.insert(name.to_string(), a);
    a
}
