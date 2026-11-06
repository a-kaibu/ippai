//! Differential property tests: the conversions where the C standard pins the result, against
//! the host's glibc (UCRT mode; the MS extensions and legacy msvcrt behaviour have unit tests).
//! PROPTEST_CASES raises the count (CI runs the default 2048 per property).

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use crt::{fmt, num, scan, time};
use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(2048)
}

// ---------------------------------------------------------------------------------------------
// printf

enum Arg {
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
}

struct One(Option<Arg>);

impl fmt::Args for One {
    fn int(&mut self, bytes: u32) -> u64 {
        match self.0.take() {
            Some(Arg::Int(v)) => v as u64 & if bytes >= 8 { u64::MAX } else { (1 << (8 * bytes)) - 1 },
            Some(Arg::Str(_)) => 1,
            _ => 0,
        }
    }
    fn float(&mut self) -> f64 {
        match self.0.take() {
            Some(Arg::Float(v)) => v,
            _ => 0.0,
        }
    }
    fn narrow(&mut self, _: u64, max: Option<usize>) -> Vec<u8> {
        STR.with(|s| s.borrow().iter().copied().take(max.unwrap_or(usize::MAX)).collect())
    }
    fn wide(&mut self, _: u64, _: Option<usize>) -> Vec<u16> {
        vec![]
    }
    fn counted(&mut self, _: u64) -> (usize, u64) {
        (0, 0)
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        s.iter().map(|&b| b as u16).collect()
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        s.iter().map(|&u| u as u8).collect()
    }
    fn store(&mut self, _: u64, _: u32, _: u64) {}
}

thread_local! {
    static STR: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn glibc_printf(f: &str, arg: &Arg) -> String {
    let cf = CString::new(f).unwrap();
    let mut buf = vec![0u8; 1 << 16];
    let n = unsafe {
        match arg {
            Arg::Int(v) => libc::snprintf(buf.as_mut_ptr() as *mut c_char, buf.len(), cf.as_ptr(), *v as libc::c_longlong),
            Arg::Float(v) => libc::snprintf(buf.as_mut_ptr() as *mut c_char, buf.len(), cf.as_ptr(), *v),
            Arg::Str(s) => {
                let cs = CString::new(s.clone()).unwrap();
                libc::snprintf(buf.as_mut_ptr() as *mut c_char, buf.len(), cf.as_ptr(), cs.as_ptr())
            }
        }
    };
    String::from_utf8_lossy(&buf[..n as usize]).into_owned()
}

fn spec() -> impl Strategy<Value = (String, Option<usize>, Option<usize>)> {
    (
        proptest::collection::vec(prop_oneof![Just('-'), Just('+'), Just(' '), Just('#'), Just('0')], 0..4),
        prop_oneof![3 => Just(None), 1 => (0usize..40).prop_map(Some)],
        prop_oneof![2 => Just(None), 1 => (0usize..30).prop_map(Some)],
    )
        .prop_map(|(f, w, p)| (f.into_iter().collect(), w, p))
}

fn build(flags: &str, w: Option<usize>, p: Option<usize>, size: &str, ty: char) -> String {
    let mut s = format!("%{flags}");
    if let Some(w) = w {
        s += &w.to_string();
    }
    if let Some(p) = p {
        s += &format!(".{p}");
    }
    format!("<{s}{size}{ty}>")
}

fn ours(f: &str, arg: Arg) -> String {
    let mut a = One(Some(arg));
    String::from_utf8(fmt::format(f.as_bytes(), fmt::Mode::ucrt(false, 0), &mut a).unwrap()).unwrap()
}

/// Doubles of every kind: exact ties, huge and tiny magnitudes, subnormals, integers.
fn double() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<u64>().prop_map(f64::from_bits).prop_filter("finite", |v| v.is_finite()),
        (-1_000_000i64..1_000_000, 0u32..12).prop_map(|(m, k)| m as f64 / 10f64.powi(k as i32)),
        (-4096i64..4096, -20i32..20).prop_map(|(m, e)| m as f64 * 2f64.powi(e)),
        (0u64..(1 << 52)).prop_map(f64::from_bits),
        Just(0.5), Just(1.5), Just(2.5), Just(-0.0), Just(0.125), Just(9.5), Just(99.5), Just(1e300), Just(5e-324),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn printf_integers((flags, w, p) in spec(), v in any::<i64>(), ty in prop_oneof![Just('d'), Just('i'), Just('u'), Just('o'), Just('x'), Just('X')], size in prop_oneof![Just(""), Just("hh"), Just("h"), Just("ll")]) {
        let f = build(&flags, w, p, size, ty);
        // (the C reference takes the value as the size says: cast it as the callee would)
        let g = match size { "hh" => v as i8 as i64, "h" => v as i16 as i64, "" => v as i32 as i64, _ => v };
        let want = glibc_printf(&f, &Arg::Int(g));
        prop_assert_eq!(ours(&f, Arg::Int(v)), want, "{}", f);
    }

    #[test]
    fn printf_floats((flags, w, p) in spec(), v in double(), ty in prop_oneof![Just('f'), Just('F'), Just('e'), Just('E'), Just('g'), Just('G')]) {
        let f = build(&flags, w, p, "", ty);
        let (o, g) = (ours(&f, Arg::Float(v)), glibc_printf(&f, &Arg::Float(v)));
        // (glibc bug: %#g that rounds up into the next power of ten loses a digit, 99.5 with
        // %#.2g prints 1.e+02; C11 7.21.6.1p8 keeps P-1 = 1 digit after the point: 1.0e+02)
        let glibc_carry_bug = flags.contains('#') && "gG".contains(ty) && {
            let bare = build(&flags, None, p, "", ty);
            ours(&bare, Arg::Float(v)) == glibc_printf(&bare, &Arg::Float(v)).replacen('.', ".0", 1)
        };
        prop_assert!(o == g || glibc_carry_bug, "{} {:e}: {} vs glibc {}", f, v, o, g);
    }

    #[test]
    fn printf_hex_floats((flags, w, _) in spec(), p in 0usize..16, v in double().prop_map(|v| if v != 0.0 && v.abs() < f64::MIN_POSITIVE { v * 2f64.powi(60) } else { v }), ty in prop_oneof![Just('a'), Just('A')]) {
        // (an explicit precision: without one the MS CRT prints all 13 digits, glibc the fewest;
        // subnormals have their own unit test)
        let f = build(&flags, w, Some(p), "", ty);
        prop_assert_eq!(ours(&f, Arg::Float(v)), glibc_printf(&f, &Arg::Float(v)), "{} {:e}", f, v);
    }

    #[test]
    fn printf_strings((flags, w, p) in spec(), s in "[ -~]{0,40}", ty in prop_oneof![Just('s'), Just('c')]) {
        // ('0' pads text with zeros in the MS CRT: not a C behaviour)
        let flags = flags.replace('0', "");
        let f = build(&flags, w, if ty == 'c' { None } else { p }, "", ty);
        STR.with(|x| *x.borrow_mut() = s.as_bytes().to_vec());
        let (arg, ga) = if ty == 'c' {
            let c = s.bytes().next().unwrap_or(b'x') as i64;
            (Arg::Int(c), Arg::Int(c))
        } else {
            (Arg::Str(s.as_bytes().to_vec()), Arg::Str(s.as_bytes().to_vec()))
        };
        prop_assert_eq!(ours(&f, arg), glibc_printf(&f, &ga), "{}", f);
    }
}

// ---------------------------------------------------------------------------------------------
// strto*

fn numberish() -> impl Strategy<Value = String> {
    prop_oneof![
        "[ \t]{0,2}[+-]?(0[xX])?[0-9a-fA-F]{0,24}[.]?[0-9a-fA-F]{0,8}([eEpP][+-]?[0-9]{0,4})?[a-z]?",
        "[ \t+\\-0-9.eExXpPa-fA-FinftyINFTYnaNA()_]{0,30}",
        "[+-]?(inf|INF|infinity|Infinity|nan|NaN|nan\\([a-z0-9_]{0,4}\\)?)[a-z]?",
        "[+-]?[0-9]{1,30}(e[+-]?[0-9]{1,6})?",
        "[+-]?0[xX][0-9a-f]{1,20}(p[+-]?[0-9]{1,5})?",
    ]
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}
fn clear_errno() {
    unsafe { *libc::__errno_location() = 0 }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn strtod_matches(s in numberish()) {
        let cs = CString::new(s.clone()).unwrap();
        let mut end: *mut c_char = std::ptr::null_mut();
        clear_errno();
        let g = unsafe { libc::strtod(cs.as_ptr(), &mut end) };
        let (ge, gn) = (errno(), end as usize - cs.as_ptr() as usize);
        let (v, n, e) = num::strtod(s.as_bytes(), num::Float::F64, false);
        prop_assert_eq!(n, gn, "{:?}", s);
        // (NAN(IND) is the UCRT's negative indefinite NaN: an MS extension)
        let ind = s.to_ascii_lowercase().contains("nan(ind)");
        prop_assert!(v.to_bits() == g.to_bits() || (v.is_nan() && g.is_nan() && (ind || v.is_sign_negative() == g.is_sign_negative())), "{:?}: {} vs {}", s, v, g);
        // (glibc follows C11: ERANGE for an inexact subnormal too; Windows only for zero)
        let sub = |v: f64, min: f64| v != 0.0 && v.abs() < min;
        // (and glibc leaks the ERANGE of reading a long NAN(digits) payload)
    let ge = if sub(v, f64::MIN_POSITIVE) || v.is_nan() { 0 } else { ge };
        prop_assert_eq!(e, ge, "{:?}", s);
        clear_errno();
        let gf = unsafe { libc::strtof(cs.as_ptr(), &mut end) };
        let (ge, gn) = (errno(), end as usize - cs.as_ptr() as usize);
        let (v, n, e) = num::strtod(s.as_bytes(), num::Float::F32, false);
        prop_assert_eq!(n, gn, "{:?}", s);
        prop_assert!((v as f32).to_bits() == gf.to_bits() || (v.is_nan() && gf.is_nan()), "{:?}: {} vs {}", s, v, gf);
        let ge = if sub(v, f32::MIN_POSITIVE as f64) || v.is_nan() { 0 } else { ge };
        prop_assert_eq!(e, ge, "strtof {:?}", s);
    }

    #[test]
    fn strtol_matches(s in numberish(), base in prop_oneof![Just(0u32), Just(10), Just(16), Just(8), 2u32..37]) {
        let cs = CString::new(s.clone()).unwrap();
        let mut end: *mut c_char = std::ptr::null_mut();
        clear_errno();
        let g = unsafe { libc::strtoll(cs.as_ptr(), &mut end, base as c_int) };
        let (ge, gn) = (errno(), end as usize - cs.as_ptr() as usize);
        prop_assert_eq!(num::strtol(s.as_bytes(), base, 64), (g, gn, ge), "{:?} base {}", s, base);
        // 32-bit long: the 64-bit result clamped, as the standard defines it
        let want32 = if ge == 0 && (i32::MIN as i64..=i32::MAX as i64).contains(&g) { (g, gn, 0) } else { (if g < 0 { i32::MIN as i64 } else { i32::MAX as i64 }, gn, libc::ERANGE) };
        prop_assert_eq!(num::strtol(s.as_bytes(), base, 32), want32, "{:?} base {}", s, base);
        clear_errno();
        let g = unsafe { libc::strtoull(cs.as_ptr(), &mut end, base as c_int) };
        let (ge, gn) = (errno(), end as usize - cs.as_ptr() as usize);
        prop_assert_eq!(num::strtoul(s.as_bytes(), base, 64), (g, gn, ge), "{:?} base {}", s, base);
    }
}

// ---------------------------------------------------------------------------------------------
// sscanf

struct Bufs {
    next: usize,
    mem: [[u8; 64]; 3],
}

impl Default for Bufs {
    fn default() -> Bufs {
        Bufs { next: 0, mem: [[0; 64]; 3] }
    }
}

impl scan::Sink for Bufs {
    fn ptr(&mut self) -> u64 {
        self.next += 1;
        self.next as u64 - 1
    }
    fn size(&mut self) -> u64 {
        64
    }
    fn int(&mut self, p: u64, bytes: u32, v: u64) {
        self.mem[p as usize][..bytes as usize].copy_from_slice(&v.to_le_bytes()[..bytes as usize]);
    }
    fn float(&mut self, p: u64, bytes: u32, v: f64) {
        if bytes == 4 {
            self.mem[p as usize][..4].copy_from_slice(&(v as f32).to_le_bytes());
        } else {
            self.mem[p as usize][..8].copy_from_slice(&v.to_le_bytes());
        }
    }
    fn narrow(&mut self, p: u64, s: &[u8]) {
        self.mem[p as usize][..s.len()].copy_from_slice(s);
    }
    fn wide(&mut self, _: u64, _: &[u16]) {}
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        s.iter().map(|&b| b as u16).collect()
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        s.iter().map(|&u| u as u8).collect()
    }
}

fn conversion() -> impl Strategy<Value = String> {
    (
        prop_oneof![4 => Just(""), 1 => Just("*")],
        prop_oneof![3 => Just(None), 1 => (1usize..12).prop_map(Some)],
        prop_oneof![
            Just("d"), Just("i"), Just("u"), Just("x"), Just("o"), Just("hd"), Just("hhu"), Just("lld"), Just("llx"),
            Just("f"), Just("lf"), Just("e"), Just("g"), Just("s"), Just("c"), Just("[a-f0-9]"), Just("[^ ,]"), Just("n")
        ],
    )
        .prop_map(|(star, w, ty)| {
            // (a width of c/s/[ beyond the 64-byte buffers must not overflow them)
            format!("%{star}{}{ty}", w.map_or(String::new(), |w| w.to_string()))
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn sscanf_matches(input in "[ \t0-9a-fx.+\\-eE,]{0,40}", convs in proptest::collection::vec(conversion(), 1..4), seps in proptest::collection::vec(prop_oneof![Just(""), Just(" "), Just(","), Just(" ,")], 3)) {
        // (%s/%[ without a width could overflow a 64-byte buffer only past 63 units: inputs are shorter)
        let fmt: String = convs.iter().zip(&seps).map(|(c, s)| format!("{c}{s}")).collect();
        // (a hexadecimal float with no hex digit, "0x." or "0xx": glibc reads some of them as 0,
        // C11 7.21.6.2p9 makes all a matching failure, which we do)
        let floats = convs.iter().any(|c| c.ends_with(['f', 'e', 'g']));
        if floats && input.contains("0x") {
            return Ok(());
        }
        let ci = CString::new(input.clone()).unwrap();
        let cf = CString::new(fmt.clone()).unwrap();
        let mut g = [[0u8; 64]; 3];
        let [a, b, c] = &mut g;
        let n = unsafe { libc::sscanf(ci.as_ptr(), cf.as_ptr(), a.as_mut_ptr() as *mut c_void, b.as_mut_ptr() as *mut c_void, c.as_mut_ptr() as *mut c_void) };
        let mut o = Bufs::default();
        let m = scan::scan(input.as_bytes(), fmt.as_bytes(), scan::Mode::default(), &mut o);
        // (glibc answers EOF when nothing was assigned yet; C11 7.21.6.2p16 says EOF only when
        // the input ended before the first conversion completed, and a %*d that completed counts:
        // 0, as the MS CRT also returns once it has read characters)
        let suppressed_first = fmt.contains("%*");
        prop_assert!(m == n || (n == -1 && m == 0 && suppressed_first), "{:?} {:?}: {} vs glibc {}", input, fmt, m, n);
        prop_assert_eq!(o.mem, g, "{:?} {:?}", input, fmt);
    }
}

// ---------------------------------------------------------------------------------------------
// strftime

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn strftime_matches(t in 0i64..time::MAX_TIME64, convs in proptest::collection::vec(prop_oneof![
        Just('a'), Just('A'), Just('b'), Just('B'), Just('C'), Just('d'), Just('D'), Just('e'), Just('F'), Just('g'), Just('G'),
        Just('h'), Just('H'), Just('I'), Just('j'), Just('m'), Just('M'), Just('n'), Just('p'), Just('r'), Just('R'), Just('S'),
        Just('t'), Just('T'), Just('u'), Just('U'), Just('V'), Just('w'), Just('W'), Just('x'), Just('X'), Just('y'), Just('Y'), Just('%')
    ], 1..8)) {
        let fmt: String = convs.iter().map(|c| format!("%{c}.")).collect();
        let tm = time::gmtime(t, 64).unwrap();
        let mut ctm: libc::tm = unsafe { std::mem::zeroed() };
        ctm.tm_sec = tm[0]; ctm.tm_min = tm[1]; ctm.tm_hour = tm[2]; ctm.tm_mday = tm[3]; ctm.tm_mon = tm[4];
        ctm.tm_year = tm[5]; ctm.tm_wday = tm[6]; ctm.tm_yday = tm[7]; ctm.tm_isdst = tm[8];
        // glibc's own gmtime agrees on the fields
        let tt = t as libc::time_t;
        let mut gtm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::gmtime_r(&tt, &mut gtm) };
        prop_assert_eq!((gtm.tm_sec, gtm.tm_min, gtm.tm_hour, gtm.tm_mday, gtm.tm_mon, gtm.tm_year, gtm.tm_wday, gtm.tm_yday), (tm[0], tm[1], tm[2], tm[3], tm[4], tm[5], tm[6], tm[7]));
        let cf = CString::new(fmt.clone()).unwrap();
        let mut buf = [0u8; 512];
        let n = unsafe { libc::strftime(buf.as_mut_ptr() as *mut c_char, buf.len(), cf.as_ptr(), &ctm) };
        let want = String::from_utf8_lossy(&buf[..n]).into_owned();
        let w: Vec<u16> = fmt.encode_utf16().collect();
        let got = String::from_utf16(&time::strftime(&w, &tm, (0, "UTC")).unwrap()).unwrap();
        prop_assert_eq!(got, want, "{}", fmt);
        // mktime(gmtime(t)) == t with no zone offset
        let mut back = tm;
        prop_assert_eq!(time::mktime(&mut back, 64, 0), Some(t));
        prop_assert_eq!(back, tm);
    }
}

#[test]
fn c_locale() {
    // glibc's strftime above runs in the "C" locale (no setlocale call): make sure
    let s = unsafe { CStr::from_ptr(libc::setlocale(libc::LC_ALL, std::ptr::null())) };
    assert_eq!(s.to_bytes(), b"C");
}
