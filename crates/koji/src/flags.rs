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

/// Kani: the flag helpers against the flags as the Intel SDM defines them (Vol. 1 3.4.3,
/// Vol. 2 ADD/ADC/SUB/SBB/CMP), worked out on wide signed and unsigned integers, for every
/// operand pair, carry and width.
#[cfg(kani)]
mod proofs {
    use super::*;

    fn even_parity(r: u64) -> bool {
        (r as u8).count_ones() % 2 == 0
    }
    fn signed(v: u64, bits: u32) -> i128 {
        let v = v & mask64(bits);
        if v & sign64(bits) != 0 { v as i128 - (1i128 << bits) } else { v as i128 }
    }
    /// ZF SF PF of a result, from the definitions
    fn result_flags(r: u64, bits: u32) -> u32 {
        let r = r & mask64(bits);
        (if r == 0 { ZF } else { 0 }) | (if (r >> (bits - 1)) & 1 == 1 { SF } else { 0 }) | (if even_parity(r) { PF } else { 0 })
    }
    /// a + b + c (c = 0, 1): result and flags
    fn spec_add(a: u64, b: u64, c: u32, bits: u32) -> (u64, u32) {
        let m = mask64(bits) as u128;
        let (ua, ub) = (a as u128 & m, b as u128 & m);
        let sum = ua + ub + c as u128;
        let r = (sum & m) as u64;
        let ssum = signed(a, bits) + signed(b, bits) + c as i128;
        let of = ssum < -(1i128 << (bits - 1)) || ssum >= 1i128 << (bits - 1);
        let af = (ua & 0xf) + (ub & 0xf) + c as u128 > 0xf;
        (r, result_flags(r, bits) | if sum > m { CF } else { 0 } | if of { OF } else { 0 } | if af { AF } else { 0 })
    }
    /// a - b - c (c = 0, 1): result and flags
    fn spec_sub(a: u64, b: u64, c: u32, bits: u32) -> (u64, u32) {
        let m = mask64(bits) as u128;
        let (ua, ub) = (a as u128 & m, b as u128 & m);
        let r = (ua.wrapping_sub(ub).wrapping_sub(c as u128) & m) as u64;
        let sd = signed(a, bits) - signed(b, bits) - c as i128;
        let of = sd < -(1i128 << (bits - 1)) || sd >= 1i128 << (bits - 1);
        let af = (ua & 0xf) < (ub & 0xf) + c as u128;
        (r, result_flags(r, bits) | if ua < ub + c as u128 { CF } else { 0 } | if of { OF } else { 0 } | if af { AF } else { 0 })
    }
    fn any_bits32() -> u32 {
        let k: u8 = kani::any_where(|&k| k < 3);
        8 << k
    }
    fn any_bits64() -> u32 {
        let k: u8 = kani::any_where(|&k| k < 4);
        8 << k
    }

    #[kani::proof]
    fn add_is_the_sdm_add() {
        let (a, b, bits): (u32, u32, u32) = (kani::any(), kani::any(), any_bits32());
        let c: u32 = kani::any_where(|&c| c < 2);
        let (r, f) = add(a, b, c, bits);
        assert_eq!((r as u64, f), spec_add(a as u64, b as u64, c, bits));
    }
    #[kani::proof]
    fn sub_is_the_sdm_sub() {
        let (a, b, bits): (u32, u32, u32) = (kani::any(), kani::any(), any_bits32());
        let c: u32 = kani::any_where(|&c| c < 2);
        let (r, f) = sub(a, b, c, bits);
        assert_eq!((r as u64, f), spec_sub(a as u64, b as u64, c, bits));
    }
    #[kani::proof]
    fn add64_is_the_sdm_add() {
        let (a, b, bits): (u64, u64, u32) = (kani::any(), kani::any(), any_bits64());
        let c: u32 = kani::any_where(|&c| c < 2);
        assert_eq!(add64(a, b, c, bits), spec_add(a, b, c, bits));
    }
    #[kani::proof]
    fn sub64_is_the_sdm_sub() {
        let (a, b, bits): (u64, u64, u32) = (kani::any(), kani::any(), any_bits64());
        let c: u32 = kani::any_where(|&c| c < 2);
        assert_eq!(sub64(a, b, c, bits), spec_sub(a, b, c, bits));
    }
    #[kani::proof]
    fn logic_flags_are_the_result_flags() {
        let (r, bits): (u64, u32) = (kani::any(), any_bits64());
        assert_eq!(szp64(r, bits), result_flags(r, bits));
        if bits <= 32 {
            assert_eq!(logic(r as u32, bits), result_flags(r, bits));
        }
    }

    /// Jcc's conditions (SDM Vol. 2 Jcc, Appendix B "condition codes")
    #[kani::proof]
    fn conditions_are_the_sdm_ones() {
        let (f, cc): (u32, u32) = (kani::any(), kani::any_where(|&c| c < 16));
        let (cf, zf, sf, of, pf) = (f & CF != 0, f & ZF != 0, f & SF != 0, f & OF != 0, f & PF != 0);
        let want = [
            of, !of, cf, !cf, zf, !zf, cf || zf, !cf && !zf, sf, !sf, pf, !pf, sf != of, sf == of, zf || sf != of, !zf && sf == of,
        ][cc as usize];
        assert_eq!(cond(f, cc), want);
    }
}
