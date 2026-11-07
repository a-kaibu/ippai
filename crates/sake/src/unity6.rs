//! Unity 6's low-level allocator made to fit the guest's 2 GB.
//!
//! Unity 6 (6000.x) gives every allocator its own regions of virtual memory, 256 MB each on a
//! 256 MB boundary, and finds the allocator of a pointer by `pointer >> 28`: a two-level table of
//! 24-bit allocator ids, one per 256 MB bucket (an id with 0xff in its top byte marks a bucket
//! that continues the region before it). It wants 14 regions at startup and more once a swap
//! chain exists, but only 7 such buckets are free below 2 GB (on Windows it has 128 TB), so it
//! fails while initializing graphics. Each region commits only 2-32 MB by the title screen.
//!
//! All of that code sits together in UnityPlayer.dll (the region manager's vtable methods, the
//! region and allocator constructors, the large-allocation path), so at load the module gets
//! 64 MB buckets instead: in the code near the function that registers a region's buckets,
//! shifts by 28 become shifts by [`SHIFT`], 0x10000000 (a region's size and alignment)
//! becomes 1 << SHIFT, and the masks and round-ups that go with them change to match.
//! Regions are then 64 MB on 64 MB boundaries and about 28 fit.
//!
//! The registration function is recognized by what it does (one id per bucket, 24 bits of id
//! and a continuation byte). Nothing is changed unless all the kinds of sites a 6000.3 and a
//! 6000.5 build have are found: a partial change would split one allocator's pointers across
//! two bucket sizes.

use koji::iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

/// log2 of the bucket and region size after the change (64 MB)
pub const SHIFT: u32 = 26;
const OLD: u32 = 28;
/// How far from the registration function the allocator's other code can be (bytes)
const WINDOW: u64 = 0x1_0000;

/// One change: the bytes at `at` (an offset in the code given to [`plan`]) become `bytes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub at: usize,
    pub bytes: Vec<u8>,
}

/// What [`plan`] found, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// the registration function's first shift
    pub anchor: u64,
    pub shifts: usize,
    /// 0x10000000 immediates (sizes, alignments, clamps)
    pub sizes: usize,
    /// `and r64, ~0xfffffff`
    pub masks: usize,
    /// `+ 0xfffffff` (round-ups)
    pub rounds: usize,
}

fn gpr64(r: Register) -> bool {
    r.is_gpr64()
}

/// The instructions of `code` (at address `ip`) from `from` up to `to` (offsets), decoded in
/// a linear sweep.
fn decode(code: &[u8], ip: u64, from: usize, to: usize) -> Vec<Instruction> {
    let to = to.min(code.len());
    let mut d = Decoder::with_ip(64, &code[from..to], ip + from as u64, DecoderOptions::NONE);
    let mut v = Vec::new();
    while d.can_decode() {
        v.push(d.decode());
    }
    v
}

fn imm(i: &Instruction) -> Option<u64> {
    (0..i.op_count()).find_map(|k| match i.op_kind(k) {
        OpKind::Immediate8
        | OpKind::Immediate8to64
        | OpKind::Immediate32
        | OpKind::Immediate32to64
        | OpKind::Immediate8to32 => Some(i.immediate(k)),
        _ => None,
    })
}

/// `shr r64, 28`
fn is_shift(i: &Instruction) -> bool {
    matches!(i.mnemonic(), Mnemonic::Shr | Mnemonic::Shl)
        && i.op0_kind() == OpKind::Register
        && gpr64(i.op0_register())
        && imm(i) == Some(OLD as u64)
}

/// `and r32/r64, 0xfffffff`: the low 28 bits of a bit field (Unity packs 4 + 28 bit handles
/// next to the allocator in some builds); its shifts by 28 are not about buckets.
fn is_field_mask(i: &Instruction) -> bool {
    i.mnemonic() == Mnemonic::And
        && i.op0_kind() == OpKind::Register
        && imm(i).is_some_and(|v| v as u32 == 0x0fff_ffff)
}

/// The registration loop: from a `shr r64, 28` on, an `and r32, 0xffffff` (the id) and a
/// `cmp r32, 0xff000000` (the continuation mark) within the function.
fn is_anchor(ins: &[Instruction]) -> bool {
    // (up to the function's end: a ret or padding)
    let body = || {
        ins[1..]
            .iter()
            .take(70)
            .take_while(|i| !matches!(i.mnemonic(), Mnemonic::Ret | Mnemonic::Int3))
    };
    is_shift(&ins[0])
        && ins[0].mnemonic() == Mnemonic::Shr
        && body().any(|i| {
            i.mnemonic() == Mnemonic::And
                && i.op0_kind() == OpKind::Register
                && imm(i) == Some(0xff_ffff)
        })
        && body().any(|i| {
            i.mnemonic() == Mnemonic::Cmp && imm(i).is_some_and(|v| v as u32 == 0xff00_0000)
        })
}

/// Finds the registration function: candidates by their bytes (REX.W C1 /5 1C), each checked
/// by decoding from there.
fn anchor(code: &[u8], ip: u64) -> Option<usize> {
    (0..code.len().saturating_sub(4)).find(|&o| {
        matches!(code[o], 0x48 | 0x49)
            && code[o + 1] == 0xc1
            && (0xe8..=0xef).contains(&code[o + 2])
            && code[o + 3] == OLD as u8
            && is_anchor(&decode(code, ip, o, o + 0x200))
    })
}

/// Where linear decoding can start at or before `o`: after a run of int3 padding.
fn sync_point(code: &[u8], o: usize) -> usize {
    let mut p = o;
    while p >= 2 {
        if code[p - 1] == 0xcc && code[p - 2] == 0xcc {
            return p;
        }
        p -= 1;
    }
    0
}

/// The changes for a module's code section `code` mapped at `ip`. None: not Unity 6's
/// allocator, or not all of it was recognized.
pub fn plan(code: &[u8], ip: u64) -> Option<(Vec<Edit>, Found)> {
    let a = anchor(code, ip)?;
    let lo = sync_point(code, a.saturating_sub(WINDOW as usize));
    let hi = (a + WINDOW as usize).min(code.len());
    let ins = decode(code, ip, lo, hi);
    let mut found = Found {
        anchor: ip + a as u64,
        ..Found::default()
    };
    let mut edits = Vec::new();
    let (mut div_size, mut region_size) = (false, false);
    let new_size = 1u32 << SHIFT;
    for (k, i) in ins.iter().enumerate() {
        if i.is_invalid() {
            continue;
        }
        let at = (i.ip() - ip) as usize;
        let raw = &code[at..at + i.len()];
        let offs = {
            // (the offsets are the decoder's, of the instruction it decoded last)
            let mut d = Decoder::with_ip(64, raw, i.ip(), DecoderOptions::NONE);
            let again = d.decode();
            d.get_constant_offsets(&again)
        };
        let put_imm = |v: u32, n: usize| {
            let o = offs.immediate_offset();
            let mut b = raw.to_vec();
            b[o..o + n].copy_from_slice(&v.to_le_bytes()[..n]);
            Edit { at, bytes: b }
        };
        let near = &ins[k.saturating_sub(3)..(k + 4).min(ins.len())];
        if is_shift(i) && offs.immediate_size() == 1 && !near.iter().any(is_field_mask) {
            edits.push(put_imm(SHIFT, 1));
            found.shifts += 1;
        } else if matches!(i.mnemonic(), Mnemonic::Mov | Mnemonic::Cmp)
            && imm(i) == Some(0x1000_0000)
            && offs.immediate_size() == 4
        {
            edits.push(put_imm(new_size, 4));
            found.sizes += 1;
            div_size |= i.mnemonic() == Mnemonic::Mov
                && ins
                    .get(k + 1)
                    .is_some_and(|n| n.mnemonic() == Mnemonic::Div);
            // (stored by 6000.x's region constructor, handed to it as the third argument by
            // 2022.3's)
            region_size |= i.mnemonic() == Mnemonic::Mov
                && (i.op0_kind() == OpKind::Memory
                    || matches!(i.op0_register(), Register::R8D | Register::R8));
        } else if i.mnemonic() == Mnemonic::And
            && i.op0_kind() == OpKind::Register
            && gpr64(i.op0_register())
            && imm(i) == Some(!0x0fff_ffffu64)
            && offs.immediate_size() == 4
        {
            edits.push(put_imm(new_size.wrapping_neg(), 4));
            found.masks += 1;
        } else if i.mnemonic() == Mnemonic::Add
            && i.op0_kind() == OpKind::Register
            && gpr64(i.op0_register())
            && imm(i) == Some(0x0fff_ffff)
            && offs.immediate_size() == 4
        {
            edits.push(put_imm(new_size - 1, 4));
            found.rounds += 1;
        } else if i.mnemonic() == Mnemonic::Lea
            && i.memory_displacement64() == 0x0fff_ffff
            && offs.displacement_size() == 4
        {
            let o = offs.displacement_offset();
            let mut b = raw.to_vec();
            b[o..o + 4].copy_from_slice(&(new_size - 1).to_le_bytes());
            edits.push(Edit { at, bytes: b });
            found.rounds += 1;
        }
    }
    // registration (2 shifts) and lookup (shift right, shifts left), the reservation's alignment
    // (divided by the page size), a region's size (stored), the block mask, a round-up
    let whole =
        found.shifts >= 4 && div_size && region_size && found.masks >= 2 && found.rounds >= 1;
    if !whole {
        return None;
    }
    if !same_instructions(code, ip, &edits) {
        return None;
    }
    Some((edits, found))
}

/// Whether every edit is inside `code`, apart from the others, and keeps its instruction as it
/// was (length, mnemonic, operands) but for a constant.
pub fn same_instructions(code: &[u8], ip: u64, edits: &[Edit]) -> bool {
    let mut end = 0;
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.at);
    sorted.iter().all(|e| {
        let ok = e.at >= end && e.at + e.bytes.len() <= code.len() && {
            let old = Decoder::with_ip(
                64,
                &code[e.at..e.at + e.bytes.len()],
                ip + e.at as u64,
                DecoderOptions::NONE,
            )
            .decode();
            let new =
                Decoder::with_ip(64, &e.bytes, ip + e.at as u64, DecoderOptions::NONE).decode();
            !new.is_invalid()
                && new.len() == e.bytes.len()
                && old.len() == new.len()
                && new.mnemonic() == old.mnemonic()
                && new.op_count() == old.op_count()
                && (0..new.op_count()).all(|k| {
                    new.op_kind(k) == old.op_kind(k)
                        && (new.op_kind(k) != OpKind::Register
                            || new.op_register(k) == old.op_register(k))
                })
                && new.memory_base() == old.memory_base()
                && new.memory_index() == old.memory_index()
        };
        end = e.at + e.bytes.len();
        ok
    })
}

/// Patches UnityPlayer.dll of a 64-bit Unity 6 program, mapped at `base` (`size` bytes).
pub fn patch(name: &str, base: u32, size: u32) {
    if !wanted(
        name,
        crate::mem::x64(),
        std::env::var_os("SAKE_NO_UNITY6").is_some(),
    ) {
        return;
    }
    let img = unsafe { std::slice::from_raw_parts(base as usize as *const u8, size as usize) };
    let Some((edits, f)) = image_plan(img, crate::mem::widen(base)) else {
        return;
    };
    for e in &edits {
        crate::mem::write(base + e.at as u32, &e.bytes);
    }
    eprintln!(
        "sake: Unity 6 allocator buckets of {} MB ({} shifts, {} sizes, {} masks, {} round-ups near {:#x})",
        1 << (SHIFT - 20),
        f.shifts,
        f.sizes,
        f.masks,
        f.rounds,
        f.anchor
    );
}

/// Whether a module loaded as `name` is patched: UnityPlayer.dll of a 64-bit program, unless
/// SAKE_NO_UNITY6 is set (`off`).
fn wanted(name: &str, x64: bool, off: bool) -> bool {
    x64 && !off && crate::pe::base_name(name).eq_ignore_ascii_case("unityplayer.dll")
}

/// [`plan`] for a mapped image at `ip`: the first code section with the allocator, edits at
/// offsets in the image.
fn image_plan(img: &[u8], ip: u64) -> Option<(Vec<Edit>, Found)> {
    code_sections(img).into_iter().find_map(|(va, end)| {
        let (edits, f) = plan(&img[va..end], ip + va as u64)?;
        Some((
            edits
                .into_iter()
                .map(|e| Edit {
                    at: va + e.at,
                    bytes: e.bytes,
                })
                .collect(),
            f,
        ))
    })
}

/// [start, end) of a mapped image's code sections.
fn code_sections(img: &[u8]) -> Vec<(usize, usize)> {
    let rd32 = |o: usize| {
        img.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
    };
    let rd16 = |o: usize| {
        img.get(o..o + 2)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()) as usize)
    };
    let (Some(pe), mut v) = (rd32(0x3c), Vec::new()) else {
        return Vec::new();
    };
    let (Some(n), Some(opt)) = (rd16(pe + 6), rd16(pe + 20)) else {
        return v;
    };
    for k in 0..n {
        let s = pe + 24 + opt + 40 * k;
        let (Some(vsize), Some(va), Some(ch)) = (rd32(s + 8), rd32(s + 12), rd32(s + 36)) else {
            break;
        };
        if ch & 0x20 != 0 && va < img.len() {
            v.push((va, (va + vsize).min(img.len())));
        }
    }
    v
}

/// Pieces of real UnityPlayer.dll code (hex) for the tests and the fuzz target.
#[doc(hidden)]
pub mod samples {
    // UnityPlayer.dll 6000.3.8f1: the registration function (0x181237d30), the lookup
    // (0x1812382a0), the reservation's alignment (0x181237eb4), a region's size (0x181235a9e), a
    // round-up (0x18123261c)
    pub const REG: &str = "405357415441574883ec28418bd94e8d2402488bfa49c1ec1c81e3ffffff0048c1ef1c4c8bf9413bfc0f83c800000048896c24504c89742420488974246066904863ef448bf748c1ed0c4181e6ff0f00008bc549837cc760007559ba10000000b900400000e8baae9200488bf04885c0740cf048810532f4ee000040000033d241b800400000488bcee832d394008bcd33c0f0490fb174cf6074194885f67414488bcee860ae9200f0488105fcf3ee0000c0ffff8bc5498b44c76042891cb08bc325000000ff3d000000ff74108d8b0000000133d981e3ffffff0033d9ffc7413bfc0f8258ffffff4c8b742420488b742460488b6c24504883c428415f415c5f5bc3";
    pub const LOOKUP: &str = "4881e2000000f04c8bd14c8bc249c1e81c4963c048c1e80c8bc04c8b4cc1604d85c9747d418bc025ff0f0000418b04818bc881e1000000ff81f9000000ff75498bc84181e8ff00000048c1e91848f7d94963c048c1e11c48c1e80c4803d18bc0498b4cc2604885c9741d418bc025ff0f00008b04818bc881e1000000ff81f9000000ff74bbeb0233c0a9ff07000074118bc848c1e91848c1e11c482bd1488bc2c333c0c3";
    pub const RESERVE: &str = "33d2488bf0b80000001049f7f5c3";
    pub const CTOR: &str =
        "b900000010483d0000001077028bc84881f90000001049898ee000000049c786e800000000000010c3";
    pub const ROUND: &str = "498dbeffffff0f440fb746084881e7000000f0c3";
    // 2022.3.22f1: the region's size passed in a register (0x18035771c), a request clamped to it
    // (0x1803615a3)
    pub const CTOR22: &str = "41b8000000100fb6442460b800000010483bd0480f47d0c3";
    // 2022.3.22f1 0x180357b6e: a round-up by add (6000.x rounds with lea)
    pub const ADD22: &str = "4881c1ffffff0f4803f9498bc94881e7000000f0c3";
    // 6000.5.4f1 0x18172170f: a 4 + 28 bit field next to the allocator code (not buckets)
    pub const FIELD: &str =
        "81e2ffffff0f49c1ea1c8910418bd2488b810002000081e2ffffff0f49c1ea1c8910c3";
    // a flag test with the same constant
    pub const FLAG: &str = "f7430400000010c3";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    use super::samples::*;

    const IP: u64 = 0x1_8000_0000;

    /// The pieces laid out as functions (int3 padding between), with `far` bytes of padding
    /// before the last one.
    fn module(parts: &[&str], last: &str, far: usize) -> Vec<u8> {
        let mut v = vec![0xcc; 16];
        for p in parts {
            v.extend(hex(p));
            v.extend([0xcc; 16]);
        }
        v.extend(vec![0xcc; far]);
        v.extend(hex(last));
        v.extend([0xcc; 16]);
        v
    }

    fn apply(code: &[u8], edits: &[Edit]) -> Vec<u8> {
        let mut c = code.to_vec();
        for e in edits {
            c[e.at..e.at + e.bytes.len()].copy_from_slice(&e.bytes);
        }
        c
    }

    /// The instructions as mnemonic and operands, in a form of our own (an iced formatter's text
    /// depends on which of its formatters the build's features bring in)
    fn listing(code: &[u8]) -> Vec<String> {
        decode(code, IP, 0, code.len())
            .iter()
            .filter(|i| i.mnemonic() != Mnemonic::Int3)
            .map(canon)
            .collect()
    }

    fn canon(i: &Instruction) -> String {
        let ops: Vec<String> = (0..i.op_count())
            .map(|k| match i.op_kind(k) {
                OpKind::Register => format!("{:?}", i.op_register(k)).to_lowercase(),
                OpKind::Memory => {
                    let mut m =
                        format!("{:?}[{:?}", i.memory_size(), i.memory_base()).to_lowercase();
                    if i.memory_index() != Register::None {
                        m += &format!("+{:?}*{}", i.memory_index(), i.memory_index_scale())
                            .to_lowercase();
                    }
                    format!("{m}+{:#x}]", i.memory_displacement64())
                }
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    format!("{:#x}", i.near_branch_target())
                }
                OpKind::Immediate8
                | OpKind::Immediate8_2nd
                | OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate64
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
                | OpKind::Immediate8to64
                | OpKind::Immediate32to64 => format!("{:#x}", i.immediate(k)),
                other => format!("{other:?}").to_lowercase(),
            })
            .collect();
        format!("{:?} {}", i.mnemonic(), ops.join(",")).to_lowercase()
    }

    #[test]
    fn patches_the_allocator_and_nothing_else() {
        let code = module(&[REG, LOOKUP, FIELD, RESERVE, FLAG, CTOR], ROUND, 0);
        let (edits, f) = plan(&code, IP).expect("recognized");
        assert_eq!(
            f,
            Found {
                anchor: IP + 16 + 0x15,
                shifts: 5,
                sizes: 5,
                masks: 2,
                rounds: 1
            }
        );
        let before = listing(&code);
        let after = listing(&apply(&code, &edits));
        let changed: Vec<(String, String)> = before
            .iter()
            .zip(&after)
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        let want = [
            ("shr r12,0x1c", "shr r12,0x1a"),
            ("shr rdi,0x1c", "shr rdi,0x1a"),
            ("and rdx,0xfffffffff0000000", "and rdx,0xfffffffffc000000"),
            ("shr r8,0x1c", "shr r8,0x1a"),
            ("shl rcx,0x1c", "shl rcx,0x1a"),
            ("shl rcx,0x1c", "shl rcx,0x1a"),
            ("mov eax,0x10000000", "mov eax,0x4000000"),
            ("mov ecx,0x10000000", "mov ecx,0x4000000"),
            ("cmp rax,0x10000000", "cmp rax,0x4000000"),
            ("cmp rcx,0x10000000", "cmp rcx,0x4000000"),
            (
                "mov uint64[r14+0xe8],0x10000000",
                "mov uint64[r14+0xe8],0x4000000",
            ),
            (
                "lea rdi,unknown[r14+0xfffffff]",
                "lea rdi,unknown[r14+0x3ffffff]",
            ),
            ("and rdi,0xfffffffff0000000", "and rdi,0xfffffffffc000000"),
        ];
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(changed, want);
        // the same instructions, only constants differ
        assert_eq!(before.len(), after.len());
    }

    #[test]
    fn leaves_code_out_of_reach_alone() {
        // the round-up past the window: no round-up found, so nothing at all
        assert!(plan(
            &module(&[REG, LOOKUP, RESERVE, CTOR], ROUND, WINDOW as usize),
            IP
        )
        .is_none());
        assert!(plan(&module(&[REG, LOOKUP, RESERVE, CTOR], ROUND, 0), IP).is_some());
    }

    #[test]
    fn wants_every_kind_of_site() {
        let all = [REG, LOOKUP, RESERVE, CTOR, ROUND];
        for skip in 0..all.len() {
            let parts: Vec<&str> = all
                .iter()
                .enumerate()
                .filter(|&(k, _)| k != skip)
                .map(|(_, p)| *p)
                .collect();
            assert!(
                plan(&module(&parts, FLAG, 0), IP).is_none(),
                "without part {skip}"
            );
        }
    }

    #[test]
    fn region_size_in_a_register() {
        // 2022.3 passes the region's size to the constructor in R8D instead of storing it
        let code = module(&[REG, LOOKUP, RESERVE, CTOR22], ROUND, 0);
        let (edits, f) = plan(&code, IP).expect("recognized");
        let after = listing(&apply(&code, &edits));
        assert!(
            after.contains(&"mov r8d,0x4000000".to_string()),
            "{after:?}"
        );
        assert!(
            after.contains(&"mov eax,0x4000000".to_string()),
            "{after:?}"
        );
        assert_eq!(f.sizes, 3);
        // the clamp alone (no size handed on) is not enough
        let clamp_only = "b800000010483bd0480f47d0c3";
        assert!(plan(&module(&[REG, LOOKUP, RESERVE, clamp_only], ROUND, 0), IP).is_none());
    }

    #[test]
    fn round_up_by_add() {
        let (edits, f) =
            plan(&module(&[REG, LOOKUP, RESERVE, CTOR], ADD22, 0), IP).expect("recognized");
        assert_eq!((f.rounds, f.masks), (1, 2));
        let after = listing(&apply(
            &module(&[REG, LOOKUP, RESERVE, CTOR], ADD22, 0),
            &edits,
        ));
        assert!(
            after.contains(&"add rcx,0x3ffffff".to_string()),
            "{after:?}"
        );
        // a 32-bit add of the constant, or one to memory, is no round-up of a pointer
        for other in ["81c1ffffff0fc3", "4881410cffffff0fc3"] {
            assert!(
                plan(&module(&[REG, LOOKUP, RESERVE, CTOR], other, 0), IP).is_none(),
                "{other}"
            );
        }
    }

    #[test]
    fn only_64_bit_registers() {
        // 32-bit shifts, masks and adds of the same constants are not about pointers: the
        // same edits with them next to the allocator as without
        let (want, _) =
            plan(&module(&[REG, LOOKUP, RESERVE, CTOR], ROUND, 0), IP).expect("recognized");
        // shr eax,0x1c; shl edx,0x1c; add ecx,0xfffffff; and ecx,0xf0000000
        let decoy = "c1e81cc1e21c81c1ffffff0f81e1000000f0c3";
        let code = module(&[REG, LOOKUP, RESERVE, CTOR, decoy], ROUND, 0);
        let (edits, f) = plan(&code, IP).expect("recognized");
        assert_eq!((f.shifts, f.rounds, f.masks), (5, 1, 2));
        assert_eq!(
            edits.iter().map(|e| &e.bytes).collect::<Vec<_>>(),
            want.iter().map(|e| &e.bytes).collect::<Vec<_>>()
        );
        let after = listing(&apply(&code, &edits));
        for d in [
            "shr eax,0x1c",
            "shl edx,0x1c",
            "add ecx,0xfffffff",
            "and ecx,0xf0000000",
        ] {
            assert!(after.contains(&d.to_string()), "{d} {after:?}");
        }
    }

    #[test]
    fn anchor_is_the_registration_loop() {
        // shifts by 28 before it that lack the id mask or the continuation mark (or have them in
        // another form) are not the anchor
        let decoys = [
            FIELD,
            "48c1e81c25ffffff00c3", // shr rax,28; and eax,0xffffff (no cmp)
            "48c1e81c3d000000ffc3", // shr rax,28; cmp eax,0xff000000 (no and)
            "48c1e81c8120ffffff003d000000ffc3", // and dword [rax],0xffffff; cmp
            "48c1e81c25feffff003d000000ffc3", // and eax,0xfffffe; cmp
            "48c1e01c25ffffff003d000000ffc3", // shl, not shr
            "48c1e81cb8ffffff003d000000ffc3", // mov eax,0xffffff; cmp
            "48c1e81c25ffffff0083f801c3", // and; cmp eax,1
            "48c1e81c25ffffff0025000000ffc3", // and; and eax,0xff000000 (no cmp)
        ];
        for d in decoys {
            let code = module(&[d, REG, LOOKUP, RESERVE, CTOR], ROUND, 0);
            let (_, f) = plan(&code, IP).expect(d);
            assert_eq!(f.anchor, IP + 16 + (hex(d).len() + 16) as u64 + 0x15, "{d}");
        }
    }

    #[test]
    fn sync_points_and_addresses() {
        let code = [0x90, 0xcc, 0xcc, 0x90, 0x90, 0x90];
        assert_eq!(sync_point(&code, 5), 3);
        assert_eq!(sync_point(&code, 3), 3);
        assert_eq!(sync_point(&code, 2), 0);
        assert_eq!(sync_point(&[0x90; 8], 7), 0);
        assert_eq!(sync_point(&[0xcc, 0x90, 0xcc, 0x90], 4), 0);
        let ins = decode(&code, IP, 3, 6);
        assert_eq!(
            ins.iter().map(|i| i.ip()).collect::<Vec<_>>(),
            [IP + 3, IP + 4, IP + 5]
        );
        // far into a section: decoding starts after the padding before the window, and the
        // edits land where they belong
        let mut big = vec![0x90; 0x2_0000];
        big[0x100] = 0xcc;
        big[0x101] = 0xcc;
        // (a movabs whose immediate would swallow the padding, were decoding to start at 0)
        big[0xfe] = 0x48;
        big[0xff] = 0xb8;
        let body = module(&[REG, LOOKUP, FIELD, RESERVE, FLAG, CTOR], ROUND, 0);
        big.extend(&body);
        let (edits, f) = plan(&big, IP).expect("recognized");
        let (near, _) = plan(&body, IP + 0x2_0000).unwrap();
        assert_eq!(f.anchor, IP + 0x2_0000 + 16 + 0x15);
        assert_eq!(
            edits
                .iter()
                .map(|e| (e.at - 0x2_0000, e.bytes.clone()))
                .collect::<Vec<_>>(),
            near.iter()
                .map(|e| (e.at, e.bytes.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// A mapped image with a header page and the given sections (virtual address, size,
    /// characteristics, contents)
    fn image(sections: &[(usize, usize, u32, &[u8])]) -> Vec<u8> {
        let end = sections
            .iter()
            .map(|s| s.0 + s.1)
            .max()
            .unwrap_or(0x1000)
            .max(0x1000);
        let mut img = vec![0u8; end];
        let pe = 0x80;
        img[0x3c..0x40].copy_from_slice(&(pe as u32).to_le_bytes());
        img[pe + 6..pe + 8].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        let opt = 0xf0usize;
        img[pe + 20..pe + 22].copy_from_slice(&(opt as u16).to_le_bytes());
        for (k, &(va, size, ch, data)) in sections.iter().enumerate() {
            let s = pe + 24 + opt + 40 * k;
            img[s + 8..s + 12].copy_from_slice(&(size as u32).to_le_bytes());
            img[s + 12..s + 16].copy_from_slice(&(va as u32).to_le_bytes());
            img[s + 36..s + 40].copy_from_slice(&ch.to_le_bytes());
            img[va..va + data.len()].copy_from_slice(data);
        }
        img
    }

    #[test]
    fn whole_images() {
        let body = module(&[REG, LOOKUP, FIELD, RESERVE, FLAG, CTOR], ROUND, 0);
        let (near, nf) = plan(&body, IP + 0x3000).unwrap();
        // data first (not code: the same bytes there are not looked at), then the code section
        let img = image(&[
            (0x1000, 0x2000, 0x4000_0040, &body),
            (0x3000, body.len(), 0x6000_0020, &body),
        ]);
        assert_eq!(code_sections(&img), [(0x3000, 0x3000 + body.len())]);
        let (edits, f) = image_plan(&img, IP).expect("found");
        assert_eq!(f, nf);
        assert_eq!(
            edits
                .iter()
                .map(|e| (e.at, e.bytes.clone()))
                .collect::<Vec<_>>(),
            near.iter()
                .map(|e| (0x3000 + e.at, e.bytes.clone()))
                .collect::<Vec<_>>()
        );
        // a section reaching past the image is cut at its end; one starting at the end is left out
        let img = image(&[(0x1000, 0x1000, 0x20, &body), (0x2000, 0x10, 0x20, &[])]);
        assert_eq!(code_sections(&img), [(0x1000, 0x2000), (0x2000, 0x2010)]);
        assert_eq!(code_sections(&img[..0x2000]), [(0x1000, 0x2000)]);
        assert_eq!(code_sections(&img[..0x1800]), [(0x1000, 0x1800)]);
        // no PE header, no sections; code without the allocator
        assert!(code_sections(&[0u8; 0x20]).is_empty());
        assert!(image_plan(&image(&[(0x1000, 0x100, 0x20, &[0x90; 0x100])]), IP).is_none());
    }

    #[test]
    fn only_unity_player_of_64_bit_programs() {
        assert!(wanted("Z:\\game\\UnityPlayer.dll", true, false));
        assert!(wanted("unityplayer.DLL", true, false));
        assert!(!wanted("UnityPlayer.dll", false, false));
        assert!(!wanted("UnityPlayer.dll", true, true));
        assert!(!wanted("GameAssembly.dll", true, false));
        assert!(!wanted("UnityPlayer.dll.bak", true, false));
    }

    #[test]
    fn edits_keep_their_instructions() {
        // and rdx, 0xfffffffff0000000 at 0, shr r8, 0x1c at 7
        let code = hex("4881e2000000f049c1e81c");
        let e = |at: usize, b: &str| Edit { at, bytes: hex(b) };
        assert!(same_instructions(
            &code,
            IP,
            &[e(0, "4881e2000000fc"), e(7, "49c1e81a")]
        ));
        assert!(same_instructions(
            &code,
            IP,
            &[e(7, "49c1e81a"), e(0, "4881e2000000fc")]
        ));
        assert!(same_instructions(&code, IP, &[]));
        // another register, another mnemonic, another length, an invalid encoding
        assert!(!same_instructions(&code, IP, &[e(7, "49c1e91a")]));
        assert!(!same_instructions(&code, IP, &[e(7, "49c1e01a")]));
        assert!(!same_instructions(&code, IP, &[e(0, "4883e2f0")]));
        assert!(!same_instructions(&code, IP, &[e(7, "0f0b0f0b")]));
        // overlapping, reaching past the end of the code
        assert!(!same_instructions(
            &code,
            IP,
            &[e(0, "4881e2000000fc"), e(6, "fc49c1e81a")]
        ));
        assert!(!same_instructions(&code, IP, &[e(9, "c1e81a00")]));
        // memory operands: the base and index stay
        let m = hex("49c786e800000000000010");
        assert!(same_instructions(&m, IP, &[e(0, "49c786e800000000000004")]));
        assert!(!same_instructions(
            &m,
            IP,
            &[e(0, "49c787e800000000000004")]
        ));
        assert!(!same_instructions(
            &hex("488d3c88"),
            IP,
            &[e(0, "488d3c89")]
        ));
        // a branch: the target moves (its operand stays a near branch)
        assert!(same_instructions(
            &hex("e800000000"),
            IP,
            &[e(0, "e810000000")]
        ));
        // the edit's address: a RIP-relative operand decodes the same anywhere, a second
        // instruction's offset is its own
        assert!(same_instructions(
            &hex("90488b0500000000"),
            IP,
            &[e(1, "488b0510000000")]
        ));
    }

    #[test]
    fn anchor_at_the_start() {
        let reg = hex(REG);
        assert_eq!(anchor(&reg[0x15..], IP), Some(0));
    }

    #[test]
    fn not_unity() {
        assert!(plan(&module(&[FIELD, FLAG, RESERVE, CTOR], ROUND, 0), IP).is_none());
        assert!(plan(&[], IP).is_none());
        assert!(plan(&[0x48, 0xc1, 0xe8, 0x1c], IP).is_none());
    }

    #[test]
    fn sections() {
        let mut img = vec![0u8; 0x400];
        img[0x3c] = 0x80;
        img[0x86] = 2; // sections
        img[0x94] = 0xf0; // optional header size
        let s = 0x80 + 24 + 0xf0;
        let put =
            |img: &mut Vec<u8>, o: usize, v: u32| img[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut img, s + 8, 0x100);
        put(&mut img, s + 12, 0x200);
        put(&mut img, s + 36, 0x6000_0020);
        put(&mut img, s + 40 + 8, 0x100);
        put(&mut img, s + 40 + 12, 0x300);
        put(&mut img, s + 40 + 36, 0x4000_0040);
        assert_eq!(code_sections(&img), vec![(0x200, 0x300)]);
        assert!(code_sections(&[0; 8]).is_empty());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// (snippet, its shifts, sizes, masks, round-ups)
        const PARTS: [(&str, usize, usize, usize, usize); 7] = [
            (REG, 2, 0, 0, 0),
            (LOOKUP, 3, 0, 1, 0),
            (RESERVE, 0, 1, 0, 0),
            (CTOR, 0, 4, 0, 0),
            (ROUND, 0, 0, 1, 1),
            (FIELD, 0, 0, 0, 0),
            (FLAG, 0, 0, 0, 0),
        ];

        proptest! {
            #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

            /// Any order and spacing of the pieces (int3 padding, as between functions) within
            /// WINDOW of the registration: all found, each change only a constant, and the decoys
            /// untouched. (Gaps up to 0x3000 could put a piece past WINDOW, where plan rightly
            /// finds nothing: 1 run in a few thousand failed so. far_piece tests that side.)
            #[test]
            fn every_layout(order in Just((0..PARTS.len()).collect::<Vec<_>>()).prop_shuffle(), gaps in proptest::collection::vec(0usize..0x2000, PARTS.len())) {
                let mut code = vec![0xcc; 16];
                for (&k, &g) in order.iter().zip(&gaps) {
                    code.extend(hex(PARTS[k].0));
                    code.extend(vec![0xcc; 2 + g]);
                }
                let (edits, f) = plan(&code, IP).expect("recognized");
                let want = PARTS.iter().fold((0, 0, 0, 0), |a, p| (a.0 + p.1, a.1 + p.2, a.2 + p.3, a.3 + p.4));
                prop_assert_eq!((f.shifts, f.sizes, f.masks, f.rounds), want);
                prop_assert!(same_instructions(&code, IP, &edits));
                prop_assert_eq!(edits.len(), want.0 + want.1 + want.2 + want.3);
                // (the pieces' own listings: only the allocator's constants differ)
                let after = apply(&code, &edits);
                let mut o = 16;
                for (&k, &g) in order.iter().zip(&gaps) {
                    let n = hex(PARTS[k].0).len();
                    if matches!(PARTS[k].0, FIELD | FLAG) {
                        prop_assert_eq!(&code[o..o + n], &after[o..o + n]);
                    }
                    o += n + 2 + g;
                }
            }

            /// A needed piece farther than WINDOW from the registration: not recognized (the
            /// allocator's code is one compilation unit; farther code is something else)
            #[test]
            fn far_piece(last in 2usize..PARTS.len(), extra in 0usize..0x1000) {
                let mut code = vec![0xcc; 16];
                for k in (0..PARTS.len()).filter(|&k| k != last) {
                    code.extend(hex(PARTS[k].0));
                    code.extend([0xcc; 2]);
                }
                code.extend(vec![0xcc; WINDOW as usize + extra]);
                code.extend(hex(PARTS[last].0));
                code.extend([0xcc; 16]);
                // (FIELD and FLAG are decoys: without them it is still found)
                prop_assert_eq!(plan(&code, IP).is_some(), matches!(PARTS[last].0, FIELD | FLAG));
            }

            /// Random bytes, alone or around the pieces: no panic, and whatever is planned keeps
            /// its instructions.
            #[test]
            fn random_bytes(noise in proptest::collection::vec(any::<u8>(), 0..4096), with in any::<bool>(), at in 0usize..4096) {
                let mut code = noise.clone();
                if with {
                    let at = at.min(code.len());
                    let mut mid = vec![0xcc; 2];
                    for p in PARTS {
                        mid.extend(hex(p.0));
                        mid.extend([0xcc; 2]);
                    }
                    code.splice(at..at, mid);
                }
                if let Some((edits, _)) = plan(&code, IP) {
                    prop_assert!(same_instructions(&code, IP, &edits));
                }
            }
        }
    }

    /// The real thing: SAKE_UNITY_DLLS="a.dll:b.dll" cargo test -p sake --lib unity6 -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_dlls() {
        let Ok(list) = std::env::var("SAKE_UNITY_DLLS") else {
            return;
        };
        for path in list.split(':') {
            let file = std::fs::read(path).unwrap();
            // map the sections as the loader does
            let rd = |o: usize| u32::from_le_bytes(file[o..o + 4].try_into().unwrap()) as usize;
            let pe = rd(0x3c);
            let (n, opt) = (
                u16::from_le_bytes([file[pe + 6], file[pe + 7]]) as usize,
                u16::from_le_bytes([file[pe + 20], file[pe + 21]]) as usize,
            );
            let mut img = vec![0u8; rd(pe + 24 + 56)];
            img[..rd(pe + 24 + 60)].copy_from_slice(&file[..rd(pe + 24 + 60)]);
            for k in 0..n {
                let s = pe + 24 + opt + 40 * k;
                let (va, raw, ptr) = (rd(s + 12), rd(s + 16), rd(s + 20));
                let n = raw.min(img.len() - va);
                img[va..va + n].copy_from_slice(&file[ptr..ptr + n]);
            }
            let found: Vec<_> = code_sections(&img)
                .into_iter()
                .filter_map(|(a, b)| plan(&img[a..b], IP + a as u64).map(|p| p.1))
                .collect();
            eprintln!("{path}: {found:x?}");
            assert_eq!(found.len(), 1);
        }
    }

    /// Unity's own code, run natively: UnityPlayer.dll mapped at its base in this (x86-64) test
    /// process, its registration and lookup functions called as they are and once patched, and
    /// compared with a model of the bucket table at 256 MB and at 64 MB buckets, for random
    /// sequences of registrations and releases (proptest).
    /// SAKE_UNITY_DLL=path cargo test -p sake --lib unity6::tests::native -- --ignored --nocapture
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    mod native {
        use super::super::*;
        use proptest::prelude::*;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};

        /// The mapped image and what was found in it
        struct Img {
            base: u64,
            text: (usize, usize),
            register: u64,
            lookup: u64,
            entry: u64,
        }

        extern "win64" fn alloc(n: usize, _align: usize) -> *mut u8 {
            unsafe { libc::malloc(n) as *mut u8 } // (glibc: 16-aligned, what Unity asks)
        }
        extern "win64" fn memset(p: *mut u8, v: i32, n: usize) -> *mut u8 {
            unsafe { libc::memset(p as *mut libc::c_void, v, n) as *mut u8 }
        }
        extern "win64" fn free(p: *mut u8) {
            unsafe { libc::free(p as *mut libc::c_void) }
        }

        fn rd(f: &[u8], o: usize) -> usize {
            u32::from_le_bytes(f[o..o + 4].try_into().unwrap()) as usize
        }

        /// The first instruction after int3 padding or a ret (2022.3 does not align it) in the
        /// window that `is` accepts.
        fn function_starting(
            code: &[u8],
            ip: u64,
            a: usize,
            is: impl Fn(&Instruction) -> bool,
        ) -> u64 {
            let lo = sync_point(code, a - WINDOW as usize);
            let ins = decode(code, ip, lo, a + WINDOW as usize);
            ins.windows(3)
                .find(|w| {
                    matches!(w[0].mnemonic(), Mnemonic::Int3 | Mnemonic::Ret)
                        && w[1].mnemonic() != Mnemonic::Int3
                        && is(&w[1])
                })
                .unwrap()[1]
                .ip()
        }

        fn image() -> &'static Mutex<Img> {
            static IMG: OnceLock<Mutex<Img>> = OnceLock::new();
            IMG.get_or_init(|| {
                let path = std::env::var("SAKE_UNITY_DLL").expect("SAKE_UNITY_DLL");
                let f = std::fs::read(path).unwrap();
                let pe = rd(&f, 0x3c);
                let base = u64::from_le_bytes(f[pe + 24 + 24..pe + 24 + 32].try_into().unwrap());
                let size = rd(&f, pe + 24 + 56);
                let p = unsafe {
                    libc::mmap(
                        base as *mut libc::c_void,
                        size,
                        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                        -1,
                        0,
                    )
                };
                assert_eq!(p as u64, base, "the image's base is taken");
                let img = unsafe { std::slice::from_raw_parts_mut(base as *mut u8, size) };
                let (n, opt) = (
                    u16::from_le_bytes([f[pe + 6], f[pe + 7]]) as usize,
                    u16::from_le_bytes([f[pe + 20], f[pe + 21]]) as usize,
                );
                img[..rd(&f, pe + 24 + 60)].copy_from_slice(&f[..rd(&f, pe + 24 + 60)]);
                for k in 0..n {
                    let s = pe + 24 + opt + 40 * k;
                    let (va, raw, ptr) = (rd(&f, s + 12), rd(&f, s + 16), rd(&f, s + 20));
                    let n = raw.min(size - va);
                    img[va..va + n].copy_from_slice(&f[ptr..ptr + n]);
                }
                let text = code_sections(img)[0];
                let code = &img[text.0..text.1];
                let ip = base + text.0 as u64;
                let a = anchor(code, ip).unwrap();
                // (the last function start before the anchor: after padding or a ret, as 6000.4
                // has one int3 only before it)
                let from = sync_point(code, a);
                let register = decode(code, ip, from, a)
                    .windows(2)
                    .filter(|w| {
                        matches!(w[0].mnemonic(), Mnemonic::Ret | Mnemonic::Int3)
                            && w[1].mnemonic() != Mnemonic::Int3
                    })
                    .map(|w| w[1].ip())
                    .last()
                    .unwrap_or(from as u64 + ip);
                let lookup = function_starting(code, ip, a, |i| {
                    i.mnemonic() == Mnemonic::And
                        && i.op0_register() == Register::RDX
                        && imm(i) == Some(!0x0fff_ffffu64)
                });
                let entry = function_starting(code, ip, a, |i| {
                    is_shift(i) && i.mnemonic() == Mnemonic::Shr && i.op0_register() == Register::R8
                });
                // the registration's calls: allocate, clear, free (an inner table that lost a race)
                let ro = (register - ip) as usize;
                let calls: Vec<u64> = decode(code, ip, ro, ro + 0x200)
                    .iter()
                    .take_while(|i| i.mnemonic() != Mnemonic::Ret)
                    .filter(|i| {
                        i.mnemonic() == Mnemonic::Call && i.op0_kind() == OpKind::NearBranch64
                    })
                    .map(|i| i.near_branch64())
                    .collect();
                assert_eq!(calls.len(), 3, "{calls:x?}");
                for (t, f) in calls.iter().zip([
                    alloc as *const () as u64,
                    memset as *const () as u64,
                    free as *const () as u64,
                ]) {
                    let o = (t - base) as usize;
                    img[o..o + 2].copy_from_slice(&[0x48, 0xb8]);
                    img[o + 2..o + 10].copy_from_slice(&f.to_le_bytes());
                    img[o + 10..o + 12].copy_from_slice(&[0xff, 0xe0]);
                }
                eprintln!(
                    "register {register:#x} lookup {lookup:#x} entry {entry:#x} calls {calls:x?}"
                );
                Mutex::new(Img {
                    base,
                    text,
                    register,
                    lookup,
                    entry,
                })
            })
        }

        fn patch_in_place(img: &Img) {
            let code = unsafe {
                std::slice::from_raw_parts_mut(
                    (img.base + img.text.0 as u64) as *mut u8,
                    img.text.1 - img.text.0,
                )
            };
            let (edits, _) = plan(code, img.base + img.text.0 as u64).unwrap();
            for e in edits {
                code[e.at..e.at + e.bytes.len()].copy_from_slice(&e.bytes);
            }
        }

        /// The bucket table as the registration function keeps it: id | buckets since the
        /// region's first << 24 (0xff: at least that many).
        #[derive(Default)]
        struct Model {
            shift: u32,
            t: HashMap<u64, u32>,
        }
        impl Model {
            fn register(&mut self, p: u64, n: u64, id: u32) {
                let mut e = id & 0xff_ffff;
                for b in (p >> self.shift) as u32..((p + n) >> self.shift) as u32 {
                    self.t.insert(b as u64, e);
                    if e & 0xff00_0000 != 0xff00_0000 {
                        e += 0x100_0000;
                    }
                }
            }
            fn entry(&self, p: u64) -> u32 {
                self.t
                    .get(&((p >> self.shift) as u32 as u64))
                    .copied()
                    .unwrap_or(0)
            }
            /// The region's start, 0: none
            fn lookup(&self, p: u64) -> u64 {
                let mut q = p & !((1u64 << self.shift) - 1);
                let mut e = self.entry(q);
                while e >> 24 == 0xff {
                    q -= 0xff << self.shift;
                    e = self.entry(q);
                }
                if e & 0x7ff == 0 {
                    0
                } else {
                    q - (((e >> 24) as u64) << self.shift)
                }
            }
        }

        #[derive(Debug, Clone)]
        enum Op {
            Register(u64, u64, u32),
            Release(u64, u64),
        }

        fn ops(shift: u32) -> impl Strategy<Value = Vec<Op>> {
            let b = 1u64 << shift;
            // (regions in the first 256 buckets, most on bucket boundaries as Unity makes them)
            let at = (0u64..256, any::<bool>(), 0u64..b)
                .prop_map(move |(k, aligned, off)| k * b + if aligned { 0 } else { off });
            let len = (1u64..300, any::<bool>(), 0u64..b).prop_map(move |(k, whole, off)| {
                if whole {
                    k * b
                } else {
                    k * b / 3 + off
                }
            });
            let op = prop_oneof![
                3 => (at.clone(), len.clone(), 1u32..0x80_0000).prop_map(|(p, n, id)| Op::Register(p, n, id)),
                1 => (at, len).prop_map(|(p, n)| Op::Release(p, n)),
            ];
            proptest::collection::vec(op, 1..24)
        }

        /// Runs `ops` through Unity's functions and the model; compares every bucket touched
        /// and pointers around them.
        fn check(img: &Img, shift: u32, ops: &[Op]) -> Result<(), TestCaseError> {
            type Reg = extern "win64" fn(*mut u8, u64, u64, u32);
            type Look = extern "win64" fn(*mut u8, u64) -> u64;
            type Entry = extern "win64" fn(*mut u8, *mut u32, u64) -> *mut u32;
            let (reg, look, entry): (Reg, Look, Entry) = unsafe {
                (
                    std::mem::transmute(img.register),
                    std::mem::transmute(img.lookup),
                    std::mem::transmute(img.entry),
                )
            };
            let mut this = vec![0u8; 0x1_0000];
            let mut m = Model {
                shift,
                ..Model::default()
            };
            let mut probes = Vec::new();
            for op in ops {
                let (p, n, id) = match *op {
                    Op::Register(p, n, id) => (p, n, id),
                    Op::Release(p, n) => (p, n, 0),
                };
                reg(this.as_mut_ptr(), p, n, id);
                m.register(p, n, id);
                for q in [
                    p,
                    p + n / 2,
                    (p + n).saturating_sub(1),
                    p + n,
                    p.saturating_sub(1),
                ] {
                    probes.push(q);
                }
            }
            for q in probes {
                prop_assert_eq!(
                    look(this.as_mut_ptr(), q),
                    m.lookup(q),
                    "lookup {:#x} ({}-bit buckets)",
                    q,
                    shift
                );
                let mut out = 0xdead_beef_u32;
                entry(this.as_mut_ptr(), &mut out, q);
                prop_assert_eq!(out, m.entry(q), "entry {:#x}", q);
            }
            // (the inner tables leak: a few KB per case)
            Ok(())
        }

        #[test]
        #[ignore]
        fn unity_code_matches_the_model() {
            if std::env::var_os("SAKE_UNITY_DLL").is_none() {
                return;
            }
            let img = image().lock().unwrap();
            let cfg = ProptestConfig {
                cases: 400,
                ..ProptestConfig::default()
            };
            // as shipped (the model is right about 256 MB buckets) ...
            proptest!(cfg.clone(), |(ops in ops(OLD))| { check(&img, OLD, &ops)?; });
            // ... and patched: the same table at 64 MB
            patch_in_place(&img);
            proptest!(cfg, |(ops in ops(SHIFT))| { check(&img, SHIFT, &ops)?; });
        }
    }
}
