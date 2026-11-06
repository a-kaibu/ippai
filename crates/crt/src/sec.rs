//! The bounds-checked string and memory functions of the MS CRT ("Security features in the
//! CRT"): strcpy_s, strcat_s, strncpy_s, strncat_s (and the wcs* forms, the same code over
//! u16), memcpy_s, memmove_s. Each returns 0 or an errno; on an error with a usable destination
//! the destination becomes an empty string (memcpy_s zeroes it all instead).
//!
//! `dest` is the destination buffer with its declared size (None: a NULL pointer; the caller
//! makes the slice from the size argument). `src` is the source string without its NUL (None:
//! NULL), read by the caller up to the first NUL or as much as could matter (count, or the
//! destination's size + 1). Kani proves each function equal to a declarative model of its
//! documented behaviour for every input up to a small size (`proofs` below).

use crate::{Unit, EINVAL, ERANGE, STRUNCATE};

/// strncpy_s / strncat_s's count meaning "as much as fits, then truncate".
pub const TRUNCATE: usize = usize::MAX;

/// strcpy_s(dest, size, src)
pub fn strcpy_s<T: Unit>(dest: Option<&mut [T]>, src: Option<&[T]>) -> i32 {
    let Some(d) = dest.filter(|d| !d.is_empty()) else { return EINVAL };
    let Some(s) = src else {
        d[0] = T::default();
        return EINVAL;
    };
    if s.len() >= d.len() {
        d[0] = T::default();
        return ERANGE;
    }
    d[..s.len()].copy_from_slice(s);
    d[s.len()] = T::default();
    0
}

/// The length of the string in `d`, if it ends (has a NUL) inside the buffer.
fn terminated<T: Unit>(d: &[T]) -> Option<usize> {
    d.iter().position(|&c| c == T::default())
}

/// strcat_s(dest, size, src): EINVAL also when dest holds no NUL within its size.
pub fn strcat_s<T: Unit>(dest: Option<&mut [T]>, src: Option<&[T]>) -> i32 {
    let Some(d) = dest.filter(|d| !d.is_empty()) else { return EINVAL };
    let Some(s) = src else {
        d[0] = T::default();
        return EINVAL;
    };
    let Some(at) = terminated(d) else {
        d[0] = T::default();
        return EINVAL;
    };
    if at + s.len() >= d.len() {
        d[0] = T::default();
        return ERANGE;
    }
    d[at..at + s.len()].copy_from_slice(s);
    d[at + s.len()] = T::default();
    0
}

/// strncpy_s(dest, size, src, count): copies min(count, strlen(src)) units and a NUL. With
/// count TRUNCATE, as many as fit (STRUNCATE when that cut the string). A NULL destination
/// with count 0 is allowed and does nothing (the CRT's (NULL, 0, any, 0); the caller rejects a
/// NULL destination with a non-zero size first).
pub fn strncpy_s<T: Unit>(dest: Option<&mut [T]>, src: Option<&[T]>, count: usize) -> i32 {
    if count == 0 && dest.is_none() {
        return 0;
    }
    let Some(d) = dest.filter(|d| !d.is_empty()) else { return EINVAL };
    if count == 0 {
        d[0] = T::default();
        return 0;
    }
    let Some(s) = src else {
        d[0] = T::default();
        return EINVAL;
    };
    let n = s.len().min(count);
    if n >= d.len() {
        if count == TRUNCATE {
            let k = d.len() - 1;
            d[..k].copy_from_slice(&s[..k]);
            d[k] = T::default();
            return STRUNCATE;
        }
        d[0] = T::default();
        return ERANGE;
    }
    d[..n].copy_from_slice(&s[..n]);
    d[n] = T::default();
    0
}

/// strncat_s(dest, size, src, count): appends min(count, strlen(src)) units and a NUL.
pub fn strncat_s<T: Unit>(dest: Option<&mut [T]>, src: Option<&[T]>, count: usize) -> i32 {
    if count == 0 && dest.is_none() {
        return 0;
    }
    let Some(d) = dest.filter(|d| !d.is_empty()) else { return EINVAL };
    let s: &[T] = match src {
        Some(s) => s,
        None if count == 0 => &[],
        None => {
            d[0] = T::default();
            return EINVAL;
        }
    };
    let Some(at) = terminated(d) else {
        d[0] = T::default();
        return EINVAL;
    };
    let n = s.len().min(count);
    if at + n >= d.len() {
        if count == TRUNCATE {
            let k = d.len() - 1 - at;
            d[at..at + k].copy_from_slice(&s[..k]);
            d[at + k] = T::default();
            return STRUNCATE;
        }
        d[0] = T::default();
        return ERANGE;
    }
    d[at..at + n].copy_from_slice(&s[..n]);
    d[at + n] = T::default();
    0
}

/// memcpy_s(dest, size, src, count): `src` holds the `count` bytes (or None for NULL). On
/// EINVAL (NULL source) and ERANGE (size < count) the whole destination is zeroed.
pub fn memcpy_s(dest: Option<&mut [u8]>, src: Option<&[u8]>, count: usize) -> i32 {
    if count == 0 {
        return 0;
    }
    let Some(d) = dest else { return EINVAL };
    let Some(s) = src else {
        d.fill(0);
        return EINVAL;
    };
    if d.len() < count {
        d.fill(0);
        return ERANGE;
    }
    d[..count].copy_from_slice(&s[..count]);
    0
}

/// memmove_s: memcpy_s's checks, nothing written on an error. (The caller has read `src`
/// before writing, so overlapping is fine.)
pub fn memmove_s(dest: Option<&mut [u8]>, src: Option<&[u8]>, count: usize) -> i32 {
    if count == 0 {
        return 0;
    }
    let Some(d) = dest else { return EINVAL };
    let Some(s) = src else { return EINVAL };
    if d.len() < count {
        return ERANGE;
    }
    d[..count].copy_from_slice(&s[..count]);
    0
}

/// Declarative models of the documented behaviour, for the proofs and the property tests:
/// (errno, the destination afterwards).
pub mod model {
    use super::*;

    fn cleared(d: &[u8]) -> Vec<u8> {
        let mut v = d.to_vec();
        v[0] = 0;
        v
    }
    fn with(d: &[u8], at: usize, s: &[u8]) -> Vec<u8> {
        let mut v = d.to_vec();
        for (k, &c) in s.iter().chain([0u8].iter()).enumerate() {
            v[at + k] = c;
        }
        v
    }
    fn strlen(d: &[u8]) -> Option<usize> {
        (0..d.len()).find(|&i| d[i] == 0)
    }

    pub fn strcpy_s(d: Option<&[u8]>, s: Option<&[u8]>) -> (i32, Option<Vec<u8>>) {
        match (d, s) {
            (None, _) => (EINVAL, None),
            (Some(d), _) if d.is_empty() => (EINVAL, Some(d.to_vec())),
            (Some(d), None) => (EINVAL, Some(cleared(d))),
            (Some(d), Some(s)) if s.len() + 1 > d.len() => (ERANGE, Some(cleared(d))),
            (Some(d), Some(s)) => (0, Some(with(d, 0, s))),
        }
    }

    pub fn strcat_s(d: Option<&[u8]>, s: Option<&[u8]>) -> (i32, Option<Vec<u8>>) {
        match (d, s) {
            (None, _) => (EINVAL, None),
            (Some(d), _) if d.is_empty() => (EINVAL, Some(d.to_vec())),
            (Some(d), None) => (EINVAL, Some(cleared(d))),
            (Some(d), Some(s)) => match strlen(d) {
                None => (EINVAL, Some(cleared(d))),
                Some(l) if l + s.len() + 1 > d.len() => (ERANGE, Some(cleared(d))),
                Some(l) => (0, Some(with(d, l, s))),
            },
        }
    }

    pub fn strncpy_s(d: Option<&[u8]>, s: Option<&[u8]>, count: usize) -> (i32, Option<Vec<u8>>) {
        match (d, s) {
            (None, _) if count == 0 => (0, None),
            (None, _) => (EINVAL, None),
            (Some(d), _) if d.is_empty() => (EINVAL, Some(d.to_vec())),
            (Some(d), _) if count == 0 => (0, Some(cleared(d))),
            (Some(d), None) => (EINVAL, Some(cleared(d))),
            (Some(d), Some(s)) => {
                let want = &s[..s.len().min(count)];
                if want.len() < d.len() {
                    (0, Some(with(d, 0, want)))
                } else if count == TRUNCATE {
                    (STRUNCATE, Some(with(d, 0, &s[..d.len() - 1])))
                } else {
                    (ERANGE, Some(cleared(d)))
                }
            }
        }
    }

    pub fn strncat_s(d: Option<&[u8]>, s: Option<&[u8]>, count: usize) -> (i32, Option<Vec<u8>>) {
        match (d, s) {
            (None, _) if count == 0 => (0, None),
            (None, _) => (EINVAL, None),
            (Some(d), _) if d.is_empty() => (EINVAL, Some(d.to_vec())),
            (Some(d), None) if count != 0 => (EINVAL, Some(cleared(d))),
            (Some(d), s) => {
                let s = s.unwrap_or(&[]);
                match strlen(d) {
                    None => (EINVAL, Some(cleared(d))),
                    Some(l) => {
                        let want = &s[..s.len().min(count)];
                        if l + want.len() < d.len() {
                            (0, Some(with(d, l, want)))
                        } else if count == TRUNCATE {
                            (STRUNCATE, Some(with(d, l, &s[..d.len() - 1 - l])))
                        } else {
                            (ERANGE, Some(cleared(d)))
                        }
                    }
                }
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const N: usize = 4;

    fn buf() -> ([u8; N], usize) {
        let b: [u8; N] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= N);
        (b, n)
    }
    /// A source string: no NULs inside, any length up to N + 1.
    fn string() -> ([u8; N + 1], usize) {
        let s: [u8; N + 1] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= N + 1);
        for i in 0..N + 1 {
            if i < n {
                kani::assume(s[i] != 0);
            }
        }
        (s, n)
    }
    fn count() -> usize {
        let c: usize = kani::any();
        kani::assume(c <= N + 2 || c == TRUNCATE);
        c
    }

    macro_rules! check {
        ($f:ident $(, $count:ident)?) => {{
            let (mut d, dn) = buf();
            let (s, sn) = string();
            let dnull: bool = kani::any();
            let snull: bool = kani::any();
            $(let $count = count();)?
            let orig = d;
            let want = model::$f(if dnull { None } else { Some(&orig[..dn]) }, if snull { None } else { Some(&s[..sn]) } $(, $count)?);
            let got = $f(if dnull { None } else { Some(&mut d[..dn]) }, if snull { None } else { Some(&s[..sn]) } $(, $count)?);
            assert_eq!(got, want.0);
            if let Some(w) = want.1 {
                assert!(d[..dn] == w[..]);
            }
            // nothing beyond the declared size changes
            assert!(d[dn..] == orig[dn..]);
        }};
    }

    #[kani::proof]
    #[kani::unwind(7)]
    fn strcpy_s_matches_model() {
        check!(strcpy_s);
    }
    #[kani::proof]
    #[kani::unwind(7)]
    fn strcat_s_matches_model() {
        check!(strcat_s);
    }
    #[kani::proof]
    #[kani::unwind(7)]
    fn strncpy_s_matches_model() {
        check!(strncpy_s, count);
    }
    #[kani::proof]
    #[kani::unwind(7)]
    fn strncat_s_matches_model() {
        check!(strncat_s, count);
    }

    /// memcpy_s copies exactly count bytes on success and zeroes the buffer on a size error.
    #[kani::proof]
    #[kani::unwind(7)]
    fn memcpy_s_contract() {
        let (mut d, dn) = buf();
        let s: [u8; N] = kani::any();
        let c: usize = kani::any();
        kani::assume(c <= N);
        let orig = d;
        let r = memcpy_s(Some(&mut d[..dn]), Some(&s[..c]), c);
        if c == 0 {
            assert!(r == 0 && d == orig);
        } else if dn < c {
            assert!(r == ERANGE && d[..dn].iter().all(|&b| b == 0));
        } else {
            assert!(r == 0 && d[..c] == s[..c] && d[c..] == orig[c..]);
        }
    }

    /// memmove_s: memcpy_s's result, but an error leaves the buffer alone; a NULL source or
    /// destination is EINVAL (memcpy_s zeroes the destination then).
    #[kani::proof]
    #[kani::unwind(7)]
    fn memmove_s_and_null_contract() {
        let (mut d, dn) = buf();
        let s: [u8; N] = kani::any();
        let c: usize = kani::any();
        kani::assume(c <= N);
        let snull: bool = kani::any();
        let orig = d;
        let src = if snull { None } else { Some(&s[..c]) };
        let r = memmove_s(Some(&mut d[..dn]), src, c);
        if c == 0 {
            assert!(r == 0 && d == orig);
        } else if snull {
            assert!(r == EINVAL && d == orig);
        } else if dn < c {
            assert!(r == ERANGE && d == orig);
        } else {
            assert!(r == 0 && d[..c] == s[..c] && d[c..] == orig[c..]);
        }
        let mut z = orig;
        let r = memcpy_s(Some(&mut z[..dn]), None, c);
        assert!(if c == 0 { r == 0 && z == orig } else { r == EINVAL && z[..dn].iter().all(|&b| b == 0) });
        assert!(memcpy_s(None, Some(&s[..c]), c) == if c == 0 { 0 } else { EINVAL });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_cases() {
        let mut d = [b'x'; 4];
        assert_eq!(strcpy_s(Some(&mut d), Some(b"abc")), 0);
        assert_eq!(&d, b"abc\0");
        assert_eq!(strcpy_s(Some(&mut d), Some(b"abcd")), ERANGE);
        assert_eq!(d[0], 0);
        let mut d = *b"ab\0x";
        assert_eq!(strcat_s(Some(&mut d), Some(b"c")), 0);
        assert_eq!(&d, b"abc\0");
        let mut d = [b'x'; 4];
        assert_eq!(strncpy_s(Some(&mut d), Some(b"abcdef"), TRUNCATE), STRUNCATE);
        assert_eq!(&d, b"abc\0");
        assert_eq!(strncpy_s::<u8>(None, None, 0), 0);
        let mut d = *b"a\0xx";
        assert_eq!(strncat_s(Some(&mut d), Some(b"bcdef"), 2), 0);
        assert_eq!(&d, b"abc\0");
        let mut d = [7u8; 3];
        assert_eq!(memcpy_s(Some(&mut d), Some(b"abcd"), 4), ERANGE);
        assert_eq!(d, [0, 0, 0]);
    }

    /// The implementations agree with the models on every small input (what Kani proves,
    /// checked by enumeration here so plain `cargo test` covers it too).
    #[test]
    fn exhaustive_small() {
        let alphabet = [0u8, 1, 2];
        let mut bufs: Vec<Vec<u8>> = vec![vec![]];
        for len in 1..=3 {
            let mut next = Vec::new();
            for b in bufs.iter().filter(|b| b.len() == len - 1) {
                for &c in &alphabet {
                    let mut v = b.clone();
                    v.push(c);
                    next.push(v);
                }
            }
            bufs.extend(next);
        }
        let strs: Vec<Vec<u8>> = (0..=4).map(|n| (1..=n as u8).collect()).collect();
        let counts = [0, 1, 2, 3, 5, TRUNCATE];
        for d in &bufs {
            for dnull in [false, true] {
                for s in &strs {
                    for snull in [false, true] {
                        let dm = if dnull { None } else { Some(&d[..]) };
                        let sm = if snull { None } else { Some(&s[..]) };
                        let run = |f: &dyn Fn(Option<&mut [u8]>) -> i32, want: (i32, Option<Vec<u8>>)| {
                            let mut x = d.clone();
                            let r = f(if dnull { None } else { Some(&mut x[..]) });
                            assert_eq!(r, want.0, "{d:?} {s:?} {dnull} {snull}");
                            if let Some(w) = want.1 {
                                assert_eq!(x, w, "{d:?} {s:?}");
                            }
                        };
                        run(&|x| strcpy_s(x, sm), model::strcpy_s(dm, sm));
                        run(&|x| strcat_s(x, sm), model::strcat_s(dm, sm));
                        for &c in &counts {
                            run(&|x| strncpy_s(x, sm, c), model::strncpy_s(dm, sm, c));
                            run(&|x| strncat_s(x, sm, c), model::strncat_s(dm, sm, c));
                        }
                    }
                }
            }
        }
    }
}
