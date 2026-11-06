//! The C runtime's pure parts, as the C standard (C11 7.21-7.27) and Microsoft's CRT
//! documentation specify them, with the MS extensions games use (%I64d, %S, _s functions,
//! 1.#INF of the old msvcrt...). Nothing here touches guest memory: sake reads strings and
//! variable arguments through the [`fmt::Args`] / [`scan::Sink`] traits and writes results back.
//!
//! Two dialects: the Universal CRT (ucrtbase, VC++ 2015 and later, every 64-bit program) and
//! the legacy one (msvcrt.dll and msvcr70..120), where they differ (`legacy` fields).
//!
//! Tests: differential property tests against glibc where the C standard pins the result
//! (tests/), cargo-fuzz targets (fuzz/), Kani proofs of the bounded string functions and the
//! calendar (`#[cfg(kani)]`), and the whole suite under Miri (formal/miri.sh).

pub mod fmt;
pub mod num;
pub mod scan;
pub mod sec;
pub mod time;
mod dec;

/// errno values (errno.h)
pub const EINVAL: i32 = 22;
pub const ERANGE: i32 = 34;
pub const EILSEQ: i32 = 42;
/// strncpy_s and friends with _TRUNCATE: the copy was cut short
pub const STRUNCATE: i32 = 80;

/// A character unit: a byte of an ANSI string or a UTF-16 code unit.
pub trait Unit: Copy + Eq + Default + core::fmt::Debug + 'static {
    const WIDE: bool;
    fn from_ascii(b: u8) -> Self;
    /// The unit with value `c` (truncated to the unit's width).
    fn from_code(c: u32) -> Self;
    fn code(self) -> u32;
}

impl Unit for u8 {
    const WIDE: bool = false;
    #[inline]
    fn from_ascii(b: u8) -> Self {
        b
    }
    #[inline]
    fn from_code(c: u32) -> Self {
        c as u8
    }
    #[inline]
    fn code(self) -> u32 {
        self as u32
    }
}

impl Unit for u16 {
    const WIDE: bool = true;
    #[inline]
    fn from_ascii(b: u8) -> Self {
        b as u16
    }
    #[inline]
    fn from_code(c: u32) -> Self {
        c as u16
    }
    #[inline]
    fn code(self) -> u32 {
        self as u32
    }
}

/// isspace in the "C" locale: space, \t \n \v \f \r.
#[inline]
pub fn is_space(c: u32) -> bool {
    c == 0x20 || (9..=13).contains(&c)
}

/// A unit's value as a digit of `base` (2..=36), if it is one. Wide strings also take the
/// Unicode decimal digits the UCRT's `_wchartodigit` knows (fullwidth ０-９ and the others).
pub fn digit<T: Unit>(c: T, base: u32) -> Option<u32> {
    let c = c.code();
    let d = match c {
        0x30..=0x39 => c - 0x30,
        0x41..=0x5a => c - 0x41 + 10,
        0x61..=0x7a => c - 0x61 + 10,
        _ if T::WIDE => wide_digit(c)?,
        _ => return None,
    };
    (d < base).then_some(d)
}

/// _wchartodigit's ranges: the zero of each script's decimal digits.
fn wide_digit(c: u32) -> Option<u32> {
    const ZEROS: [u32; 17] = [
        0x0660, 0x06f0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0c66, 0x0ce6, 0x0d66, 0x0e50, 0x0ed0, 0x0f20, 0x1040,
        0x17e0, 0x1810, 0xff10,
    ];
    ZEROS.iter().find(|&&z| c.wrapping_sub(z) < 10).map(|&z| c - z)
}
