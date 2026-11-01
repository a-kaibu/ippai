//! EFLAGS bits and the arithmetic helpers that produce them.

pub const CF: u32 = 1 << 0;
pub const PF: u32 = 1 << 2;
pub const AF: u32 = 1 << 4;
pub const ZF: u32 = 1 << 6;
pub const SF: u32 = 1 << 7;
pub const TF: u32 = 1 << 8;
pub const IF: u32 = 1 << 9;
pub const DF: u32 = 1 << 10;
pub const OF: u32 = 1 << 11;
pub const AC: u32 = 1 << 18;
pub const ID: u32 = 1 << 21;
/// The six arithmetic status flags.
pub const STATUS: u32 = CF | PF | AF | ZF | SF | OF;
/// Flags POPFD may change in user mode.
pub const USER_WRITABLE: u32 = STATUS | TF | DF | OF | AC | ID;

/// Operand width in bits: 8, 16 or 32.
#[inline]
pub fn mask(bits: u32) -> u32 {
    if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 }
}

#[inline]
pub fn sign(bits: u32) -> u32 {
    1u32 << (bits - 1)
}

#[inline]
fn parity(v: u32) -> u32 {
    if (v as u8).count_ones() & 1 == 0 { PF } else { 0 }
}

/// SF, ZF and PF of a result.
#[inline]
pub fn szp(r: u32, bits: u32) -> u32 {
    let r = r & mask(bits);
    (if r == 0 { ZF } else { 0 }) | (if r & sign(bits) != 0 { SF } else { 0 }) | parity(r)
}

/// a + b + carry: (result, status flags)
#[inline]
pub fn add(a: u32, b: u32, carry: u32, bits: u32) -> (u32, u32) {
    let m = mask(bits) as u64;
    let (a, b) = (a as u64 & m, b as u64 & m);
    let full = a + b + carry as u64;
    let r = (full & m) as u32;
    let mut f = szp(r, bits);
    if full > m {
        f |= CF;
    }
    if ((a ^ full) & (b ^ full)) as u32 & sign(bits) != 0 {
        f |= OF;
    }
    if (a ^ b ^ full) & 0x10 != 0 {
        f |= AF;
    }
    (r, f)
}

/// a - b - borrow: (result, status flags)
#[inline]
pub fn sub(a: u32, b: u32, borrow: u32, bits: u32) -> (u32, u32) {
    let m = mask(bits) as u64;
    let (a, b) = (a as u64 & m, b as u64 & m);
    let full = a.wrapping_sub(b).wrapping_sub(borrow as u64);
    let r = (full & m) as u32;
    let mut f = szp(r, bits);
    if a < b + borrow as u64 {
        f |= CF;
    }
    if ((a ^ b) & (a ^ full)) as u32 & sign(bits) != 0 {
        f |= OF;
    }
    if (a ^ b ^ full) & 0x10 != 0 {
        f |= AF;
    }
    (r, f)
}

/// AND / OR / XOR / TEST: CF = OF = 0, AF cleared.
#[inline]
pub fn logic(r: u32, bits: u32) -> u32 {
    szp(r, bits)
}

/// Evaluate a condition code (the low nibble of Jcc/SETcc/CMOVcc).
#[inline]
pub fn cond(eflags: u32, cc: u32) -> bool {
    let cf = eflags & CF != 0;
    let zf = eflags & ZF != 0;
    let sf = eflags & SF != 0;
    let of = eflags & OF != 0;
    let pf = eflags & PF != 0;
    let r = match cc >> 1 {
        0 => of,
        1 => cf,
        2 => zf,
        3 => cf || zf,
        4 => sf,
        5 => pf,
        6 => sf != of,
        _ => zf || (sf != of),
    };
    r ^ (cc & 1 != 0)
}

/// Widths up to 64 bits (x86-64 operands): the same helpers on u64 values.
#[inline]
pub fn mask64(bits: u32) -> u64 {
    if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 }
}

#[inline]
pub fn sign64(bits: u32) -> u64 {
    1u64 << (bits - 1)
}

#[inline]
pub fn szp64(r: u64, bits: u32) -> u32 {
    let r = r & mask64(bits);
    (if r == 0 { ZF } else { 0 }) | (if r & sign64(bits) != 0 { SF } else { 0 }) | parity(r as u32)
}

#[inline]
pub fn add64(a: u64, b: u64, carry: u32, bits: u32) -> (u64, u32) {
    let m = mask64(bits) as u128;
    let (a, b) = (a as u128 & m, b as u128 & m);
    let full = a + b + carry as u128;
    let r = (full & m) as u64;
    let mut f = szp64(r, bits);
    if full > m {
        f |= CF;
    }
    if ((a ^ full) & (b ^ full)) as u64 & sign64(bits) != 0 {
        f |= OF;
    }
    if (a ^ b ^ full) & 0x10 != 0 {
        f |= AF;
    }
    (r, f)
}

#[inline]
pub fn sub64(a: u64, b: u64, borrow: u32, bits: u32) -> (u64, u32) {
    let m = mask64(bits) as u128;
    let (a, b) = (a as u128 & m, b as u128 & m);
    let full = a.wrapping_sub(b).wrapping_sub(borrow as u128);
    let r = (full & m) as u64;
    let mut f = szp64(r, bits);
    if a < b + borrow as u128 {
        f |= CF;
    }
    if ((a ^ b) & (a ^ full)) as u64 & sign64(bits) != 0 {
        f |= OF;
    }
    if (a ^ b ^ full) & 0x10 != 0 {
        f |= AF;
    }
    (r, f)
}
