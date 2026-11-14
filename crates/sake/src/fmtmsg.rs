//! kernel32 FormatMessageA/W as Windows does it (winref fmt): the message from a string, a
//! module's message table or the system's (a small table of common errors), inserts %1..%99
//! with printf specs (!d!, !5s!, !*d!...), the escapes %n %r %t %0 %% and %<other>, the width
//! modes of FORMAT_MESSAGE_MAX_WIDTH_MASK, va_list or ARGUMENT_ARRAY arguments,
//! IGNORE_INSERTS, ALLOCATE_BUFFER and the buffer-too-small error.

use crate::api::Ctx;
use crate::mem::{self, r16, r32};
use crate::text::{a2w, w2a};

const ALLOCATE_BUFFER: u32 = 0x100;
const IGNORE_INSERTS: u32 = 0x200;
const FROM_STRING: u32 = 0x400;
const FROM_HMODULE: u32 = 0x800;
const FROM_SYSTEM: u32 = 0x1000;
const ARGUMENT_ARRAY: u32 = 0x2000;

/// Common system messages (English, as the runner's).
fn system_message(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "The operation completed successfully.\r\n",
        1 => "Incorrect function.\r\n",
        2 => "The system cannot find the file specified.\r\n",
        3 => "The system cannot find the path specified.\r\n",
        4 => "The system cannot open the file.\r\n",
        5 => "Access is denied.\r\n",
        6 => "The handle is invalid.\r\n",
        8 => "Not enough memory resources are available to process this command.\r\n",
        13 => "The data is invalid.\r\n",
        14 => "Not enough memory resources are available to complete this operation.\r\n",
        15 => "The system cannot find the drive specified.\r\n",
        18 => "There are no more files.\r\n",
        21 => "The device is not ready.\r\n",
        32 => "The process cannot access the file because it is being used by another process.\r\n",
        38 => "Reached the end of the file.\r\n",
        50 => "The request is not supported.\r\n",
        80 => "The file exists.\r\n",
        87 => "The parameter is incorrect.\r\n",
        109 => "The pipe has been ended.\r\n",
        111 => "The file name is too long.\r\n",
        112 => "There is not enough space on the disk.\r\n",
        120 => "This function is not supported on this system.\r\n",
        122 => "The data area passed to a system call is too small.\r\n",
        123 => "The filename, directory name, or volume label syntax is incorrect.\r\n",
        126 => "The specified module could not be found.\r\n",
        127 => "The specified procedure could not be found.\r\n",
        145 => "The directory is not empty.\r\n",
        183 => "Cannot create a file when that file already exists.\r\n",
        193 => "%1 is not a valid Win32 application.\r\n",
        203 => "The system could not find the environment option that was entered.\r\n",
        234 => "More data is available.\r\n",
        258 => "The wait operation timed out.\r\n",
        259 => "No more data is available.\r\n",
        267 => "The directory name is invalid.\r\n",
        317 => "The system cannot find message text for message number 0x%1 in the message file for %2.\r\n",
        487 => "Attempt to access invalid address.\r\n",
        998 => "Invalid access to memory location.\r\n",
        1114 => "A dynamic link library (DLL) initialization routine failed.\r\n",
        1400 => "Invalid window handle.\r\n",
        1460 => "This operation returned because the timeout period expired.\r\n",
        1813 => "The specified resource type cannot be found in the image file.\r\n",
        1814 => "The specified resource name cannot be found in the image file.\r\n",
        1815 => "The specified resource language ID cannot be found in the image file.\r\n",
        // (STG_E_FILENOTFOUND: RPG Maker VX's Game.exe names a missing RGSS dll with it, winref fmt)
        0x8003_0002 => "%1 could not be found.\r\n",
        0x8000_4001 => "Not implemented\r\n",
        0x8000_4002 => "No such interface supported\r\n",
        0x8000_4005 => "Unspecified error\r\n",
        0x8007_000e => "Not enough memory resources are available to complete this operation.\r\n",
        0x8007_0057 => "The parameter is incorrect.\r\n",
        0x8876_086c => "Invalid call\r\n",
        _ => return None,
    })
}

/// A module's RT_MESSAGETABLE entry for `id`: MESSAGE_RESOURCE_DATA {blocks, {low, high,
/// offset}...}, entries {length, flags (1: UTF-16), text}.
fn table_message(module: u32, id: u32) -> Option<Vec<u16>> {
    let m = if module == 0 { crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map_or(0, |m| m.base) } else { module };
    let e = crate::pe::find_resource(m, &crate::pe::ResId::Int(11), &crate::pe::ResId::Int(1))?;
    let (p, size) = crate::pe::resource_data(m, e);
    let blocks = r32(p);
    for b in 0..blocks.min(4096) {
        let blk = p + 4 + 12 * b;
        let (lo, hi, off) = (r32(blk), r32(blk + 4), r32(blk + 8));
        if !(lo..=hi).contains(&id) {
            continue;
        }
        // (entries of a table that is not well formed: one of length 0, or past the resource, ends it)
        let end = p as u64 + size as u64;
        let mut q = p as u64 + off as u64;
        for _ in lo..id {
            if q + 4 > end {
                return None;
            }
            match r16(q as u32) {
                0 => return None,
                n => q += n as u64,
            }
        }
        if q + 4 > end {
            return None;
        }
        let q = q as u32;
        let (len, flags) = ((r16(q) as u32).min((end - q as u64) as u32), r16(q + 2));
        let body = q + 4;
        let text: Vec<u16> = if flags & 1 != 0 {
            (0..(len.saturating_sub(4)) / 2).map(|i| r16(body + 2 * i)).take_while(|&c| c != 0).collect()
        } else {
            a2w(&mem::slice(body, len.saturating_sub(4)).iter().copied().take_while(|&c| c != 0).collect::<Vec<u8>>())
        };
        return Some(text);
    }
    None
}

/// The insert arguments: a va_list (pointer-sized slots) or an array of DWORD_PTRs.
struct Args {
    base: u32,
}
impl Args {
    fn get(&self, i: u32) -> u64 {
        if self.base == 0 {
            return 0;
        }
        let a = self.base + i * mem::ps();
        if mem::x64() { mem::r64(a) } else { r32(a) as u64 }
    }
}

/// The widest field and the longest precision an insert takes (FormatMessage's whole output is
/// limited to 64K): a garbage `*` argument or digits are not gigabytes of padding.
pub const MAX_FIELD: usize = 0x1_0000;

/// One printf conversion of an insert: `spec` is what is between the '!'s (default "s").

fn printf_insert(spec: &[u16], args: &Args, slot: &mut u32, wide: bool) -> Vec<u16> {
    let s = String::from_utf16_lossy(spec);
    let mut it = s.chars().peekable();
    let (mut left, mut plus, mut space, mut zero, mut alt) = (false, false, false, false, false);
    while let Some(&c) = it.peek() {
        match c {
            '-' => left = true,
            '+' => plus = true,
            ' ' => space = true,
            '0' => zero = true,
            '#' => alt = true,
            _ => break,
        }
        it.next();
    }
    let num = |it: &mut std::iter::Peekable<std::str::Chars>, slot: &mut u32| -> Option<usize> {
        if it.peek() == Some(&'*') {
            it.next();
            let v = args.get(*slot) as i32;
            *slot += 1;
            return Some((v.max(0) as usize).min(MAX_FIELD));
        }
        let mut n = None;
        while let Some(d) = it.peek().and_then(|c| c.to_digit(10)) {
            n = Some((n.unwrap_or(0usize).saturating_mul(10).saturating_add(d as usize)).min(MAX_FIELD));
            it.next();
        }
        n
    };
    // zeros in front up to precision `p`, in one step
    // (and a precision of 0 prints no digits for 0, as printf)
    let zeros = |t: &mut String, p: Option<usize>| {
        if p == Some(0) && t == "0" {
            t.clear();
        }
        if let Some(p) = p.filter(|&p| p > t.len()) {
            t.insert_str(0, &"0".repeat(p - t.len()));
        }
    };
    let width = num(&mut it, slot).unwrap_or(0);
    let prec = if it.peek() == Some(&'.') {
        it.next();
        Some(num(&mut it, slot).unwrap_or(0))
    } else {
        None
    };
    let mut long64 = false;
    let mut force_wide = None;
    loop {
        match it.peek() {
            Some('h') => force_wide = Some(false),
            Some('l') | Some('w') => force_wide = Some(true),
            Some('I') => {
                it.next();
                let a: String = it.clone().take(2).collect();
                if a == "64" {
                    long64 = true;
                    it.next();
                    it.next();
                } else if a == "32" {
                    it.next();
                    it.next();
                }
                continue;
            }
            Some('L') => {}
            _ => break,
        }
        it.next();
    }
    let conv = it.next().unwrap_or('s');
    let v = args.get(*slot);
    *slot += 1;
    let v = if long64 { v } else { v & 0xffff_ffff };
    let (body, numeric): (Vec<u16>, bool) = match conv {
        'd' | 'i' => {
            let n = if long64 { v as i64 } else { v as u32 as i32 as i64 };
            let mut t = n.unsigned_abs().to_string();
            zeros(&mut t, prec);
            let sign = if n < 0 { "-" } else if plus { "+" } else if space { " " } else { "" };
            ((sign.to_string() + &t).encode_utf16().collect(), true)
        }
        'u' => {
            // (a precision pads with zeros, as printf: the C library check found it ignored)
            let mut t = v.to_string();
            zeros(&mut t, prec);
            (t.encode_utf16().collect(), true)
        }
        'x' | 'X' | 'o' | 'p' => {
            let mut t = match conv {
                'x' => format!("{v:x}"),
                'X' => format!("{v:X}"),
                'o' => format!("{v:o}"),
                _ => format!("{:0w$X}", v, w = 2 * mem::ps() as usize),
            };
            zeros(&mut t, prec);
            // ('#': octal's first digit is a 0, the precision's zeros count; hex gets 0x unless 0)
            if alt && conv == 'o' && !t.starts_with('0') {
                t.insert(0, '0');
            } else if alt && v != 0 && (conv == 'x' || conv == 'X') {
                t.insert_str(0, if conv == 'x' { "0x" } else { "0X" });
            }
            (t.encode_utf16().collect(), true)
        }
        'c' | 'C' => {
            let w = force_wide.unwrap_or((conv == 'c') == wide);
            (if w { vec![v as u16] } else { a2w(&[v as u8]) }, false)
        }
        's' | 'S' => {
            let w = force_wide.unwrap_or((conv == 's') == wide);
            let p = v as u32;
            let mut t = if p == 0 { "(null)".encode_utf16().collect() } else if w { mem::wstr(p) } else { a2w(&mem::cstr(p)) };
            if let Some(p) = prec {
                t.truncate(p);
            }
            (t, false)
        }
        _ => (Vec::new(), false),
    };
    if body.len() >= width {
        return body;
    }
    let pad = width - body.len();
    if left {
        let mut b = body;
        b.extend(std::iter::repeat(b' ' as u16).take(pad));
        b
    } else if zero && numeric && prec.is_none() {
        // zeros after the sign
        // (zeros after the sign, or after a 0x prefix)
        let sign = if body.first().is_some_and(|&c| c == b'-' as u16 || c == b'+' as u16 || c == b' ' as u16) {
            1
        } else if body.len() >= 2 && body[0] == b'0' as u16 && (body[1] == b'x' as u16 || body[1] == b'X' as u16) {
            2
        } else {
            0
        };
        let mut b = body[..sign].to_vec();
        b.extend(std::iter::repeat(b'0' as u16).take(pad));
        b.extend(&body[sign..]);
        b
    } else {
        let mut b: Vec<u16> = std::iter::repeat(b' ' as u16).take(pad).collect();
        b.extend(body);
        b
    }
}

/// The formatted message, or the error code.
fn format(src: &[u16], flags: u32, args: &Args, wide: bool) -> Result<Vec<u16>, u32> {
    let width = flags & 0xff;
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0;
    let ch = |c: char| c as u16;
    while i < src.len() {
        let c = src[i];
        if c == ch('%') {
            let Some(&n) = src.get(i + 1) else { return Err(87) };
            if (ch('1')..=ch('9')).contains(&n) {
                let start = i;
                let mut idx = (n - ch('0')) as u32;
                i += 2;
                if let Some(&d) = src.get(i) {
                    if (ch('0')..=ch('9')).contains(&d) {
                        idx = idx * 10 + (d - ch('0')) as u32;
                        i += 1;
                    }
                }
                let mut spec: Vec<u16> = "s".encode_utf16().collect();
                if src.get(i) == Some(&ch('!')) {
                    let Some(end) = src[i + 1..].iter().position(|&x| x == ch('!')) else { return Err(87) };
                    spec = src[i + 1..i + 1 + end].to_vec();
                    i += end + 2;
                }
                if flags & IGNORE_INSERTS != 0 {
                    out.extend(&src[start..i]);
                } else {
                    let mut slot = idx - 1;
                    out.extend(printf_insert(&spec, args, &mut slot, wide));
                }
                continue;
            }
            i += 2;
            match n {
                x if x == ch('0') => break,
                x if x == ch('n') => out.extend([13, 10]),
                x if x == ch('r') => out.push(13),
                x if x == ch('t') => out.push(9),
                x => out.push(x),
            }
            continue;
        }
        if width != 0 && (c == 13 || c == 10) {
            // a line break of the text: a space in the width modes (CR LF together)
            if c == 13 && src.get(i + 1) == Some(&10) {
                i += 1;
            }
            out.push(b' ' as u16);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    if width == 0 || width == 0xff {
        return Ok(out);
    }
    // break lines longer than `width` at their last space, or hard where there is none
    let w = width as usize;
    let mut res = Vec::new();
    let mut line: Vec<u16> = Vec::new();
    let mut k = 0;
    while k < out.len() {
        let c = out[k];
        if c == 13 && out.get(k + 1) == Some(&10) {
            res.extend(&line);
            res.extend([13, 10]);
            line.clear();
            k += 2;
            continue;
        }
        line.push(c);
        if line.len() > w {
            match line.iter().rposition(|&x| x == b' ' as u16) {
                Some(p) => {
                    res.extend(&line[..p]);
                    line.drain(..=p);
                }
                None => {
                    res.extend(&line[..w]);
                    line.drain(..w);
                }
            }
            res.extend([13, 10]);
        }
        k += 1;
    }
    res.extend(&line);
    Ok(res)
}

/// FormatMessage(flags, source, id, language, buffer, size, va_list *args)
pub fn format_message(c: &mut Ctx, wide: bool) -> u32 {
    let (flags, source, id, lang, buf, size, va) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let fail = |e: u32| {
        crate::k32::last_error(e);
        0
    };
    let src: Vec<u16> = if flags & FROM_STRING != 0 {
        if source == 0 {
            return fail(87);
        }
        if wide { mem::wstr(source) } else { a2w(&mem::cstr(source)) }
    } else if flags & (FROM_HMODULE | FROM_SYSTEM) != 0 {
        // a language other than neutral, English or Japanese: no such resource
        let primary = lang & 0x3ff;
        if lang != 0 && !matches!(primary, 0 | 0x09 | 0x11) {
            return fail(15100);
        }
        let from_module = if flags & FROM_HMODULE != 0 { table_message(source, id) } else { None };
        match from_module.or_else(|| if flags & FROM_SYSTEM != 0 { system_message(id).map(|s| s.encode_utf16().collect()) } else { None }) {
            Some(t) => t,
            None => return fail(if flags & FROM_SYSTEM != 0 { 317 } else { 1813 }),
        }
    } else {
        return fail(87);
    };
    let args = Args { base: if va == 0 { 0 } else if flags & ARGUMENT_ARRAY != 0 { va } else { mem::rp(va) } };
    let text = match format(&src, flags, &args, wide) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let bytes: Vec<u8> = if wide { Vec::new() } else { w2a(&text) };
    let n = if wide { text.len() } else { bytes.len() } as u32;
    let unit = if wide { 2 } else { 1 };
    let dst = if flags & ALLOCATE_BUFFER != 0 {
        let p = mem::heap_alloc((n + 1).max(size) * unit, true);
        mem::wp(buf, p);
        p
    } else {
        if n + 1 > size {
            return fail(122);
        }
        buf
    };
    if wide {
        for (k, &u) in text.iter().enumerate() {
            mem::w16(dst + 2 * k as u32, u);
        }
        mem::w16(dst + 2 * n, 0);
    } else {
        mem::write(dst, &bytes);
        mem::w8(dst + n, 0);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str, flags: u32) -> Result<String, u32> {
        let w: Vec<u16> = s.encode_utf16().collect();
        format(&w, flags, &Args { base: 0 }, true).map(|v| String::from_utf16_lossy(&v))
    }

    /// The answers of Windows (tests/winref/golden/fmt-32) for messages without inserts.
    #[test]
    fn escapes_and_widths_follow_windows() {
        assert_eq!(f("line1%nline2%r%t|%0 after zero", 0), Ok("line1\r\nline2\r\t|".into()));
        assert_eq!(f("percent %% dot %. bang %!", 0), Ok("percent % dot . bang !".into()));
        assert_eq!(f("end\r\n", 0), Ok("end\r\n".into()));
        assert_eq!(f("trailing %", 0), Err(87));
        assert_eq!(f("%1", IGNORE_INSERTS), Ok("%1".into()));
        assert_eq!(f("%1 and %2!d!", IGNORE_INSERTS), Ok("%1 and %2!d!".into()));
        assert_eq!(
            f("the quick brown fox jumps over the lazy dog again and again", 20),
            Ok("the quick brown fox\r\njumps over the lazy\r\ndog again and again".into())
        );
        assert_eq!(f("the quick brown fox\r\njumps", 0xff), Ok("the quick brown fox jumps".into()));
        assert_eq!(f("  spaced   words  ", 0xff), Ok("  spaced   words  ".into()));
        assert_eq!(f("verylongwordwithoutanyspaces here", 10), Ok("verylongwo\r\nrdwithouta\r\nnyspaces\r\nhere".into()));
    }

    /// RPG Maker VX's Game.exe (RGSS2 Player) shows FormatMessageW(FROM_SYSTEM | ARGUMENT_ARRAY,
    /// STG_E_FILENOTFOUND, [dll]) when RGSS202J.dll is missing; without the message the box was
    /// empty. Windows (tests/winref/probes/fmt.c "stg filenotfound") has it with the name in it.
    #[test]
    fn stg_file_not_found_names_the_file() {
        let t = system_message(0x8003_0002).expect("STG_E_FILENOTFOUND");
        assert!(t.contains("%1") && t.ends_with("\r\n"), "{t:?}");
        assert_eq!(f(t, IGNORE_INSERTS), Ok(t.into()));
    }

    proptest::proptest! {
        /// Every system message (Win32 codes and the HRESULTs sake knows) formats as Windows'
        /// do: one line ending in CRLF, unchanged with IGNORE_INSERTS, and (without inserts) unchanged
        /// with them too.
        #[test]
        fn system_messages_format(id in proptest::prop_oneof![0u32..20000, 0x8000_0000u32..0x8000_ffff, 0x8003_0000u32..0x8003_0100, 0x8007_0000u32..0x8007_ffff, 0x8876_0000u32..0x8876_ffff]) {
            if let Some(t) = system_message(id) {
                proptest::prop_assert!(t.ends_with("\r\n") && !t[..t.len() - 2].contains('\n'), "{id:#x}: {t:?}");
                proptest::prop_assert_eq!(f(t, IGNORE_INSERTS), Ok(t.to_string()));
                if !t.contains('%') {
                    proptest::prop_assert_eq!(f(t, 0), Ok(t.to_string()));
                }
            }
        }
    }

    /// Whatever the text and width, no line of a width-limited message is longer than the width
    /// and only spaces and line breaks go missing.
    #[test]
    fn width_limits_every_line() {
        let mut seed = 12345u32;
        for _ in 0..2000 {
            let mut s = String::new();
            for _ in 0..(seed % 60) {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                s.push(match (seed >> 16) % 5 {
                    0 => ' ',
                    1 => 'a',
                    2 => 'b',
                    3 => 'c',
                    _ => 'd',
                });
            }
            let w = 1 + (seed >> 8) % 30;
            let out = f(&s, w).unwrap();
            for line in out.split("\r\n") {
                assert!(line.chars().count() <= w as usize, "{s:?} width {w}: {out:?}");
            }
            let strip = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
            assert_eq!(strip(&out), strip(&s));
        }
    }

    /// Inserts with widths and precisions from `*` arguments or digits: a garbage argument
    /// (2^31-1, as a wrong va_list gives) is clamped to MAX_FIELD instead of 4 GiB of padding
    /// or an O(p^2) loop, and long digit strings do not overflow.
    #[test]
    fn huge_widths_are_clamped() {
        let _g = crate::test_guest::enter();
        let a = mem::heap_alloc(16, true);
        mem::w32(a, 0x7fff_ffff);
        mem::w32(a + 4, 7);
        let args = Args { base: a };
        for spec in ["*d", ".*d", "-*x", "099999999999999999999999d", ".99999999999999999999999u", "*.*d"] {
            let w: Vec<u16> = spec.encode_utf16().collect();
            let t = std::time::Instant::now();
            let out = printf_insert(&w, &args, &mut 0, true);
            assert!(out.len() <= MAX_FIELD + 2, "{spec}: {}", out.len());
            assert!(out.len() >= MAX_FIELD.min(64), "{spec}");
            assert!(t.elapsed() < std::time::Duration::from_secs(2), "{spec}");
        }
        mem::heap_free(a);
    }

    proptest::proptest! {
        /// Differential: numeric inserts (d i u x X o, flags - + space 0 #, width, precision)
        /// against the C library's printf (FormatMessage formats inserts with the CRT's
        /// printf); within MAX_FIELD the two agree character for character.
        #[test]
        fn numeric_inserts_are_printfs(flags in proptest::collection::vec(proptest::sample::select(vec!['-', '+', ' ', '0', '#']), 0..4),
                                       width in proptest::option::of(0usize..40), prec in proptest::option::of(0usize..40),
                                       conv in proptest::sample::select(vec!['d', 'i', 'u', 'x', 'X', 'o']),
                                       v in proptest::prop_oneof![proptest::prelude::any::<u32>(), 0u32..20, proptest::prelude::Just(0x8000_0000u32)],
                                       star in proptest::prelude::any::<bool>()) {
            let _g = crate::test_guest::enter();
            let a = mem::heap_alloc(16, true);
            let mut spec: String = flags.iter().collect();
            let mut slots = vec![];
            if let Some(w) = width {
                if star { spec.push('*'); slots.push(w as u32); } else { spec += &w.to_string(); }
            }
            if let Some(p) = prec {
                if star { spec += ".*"; slots.push(p as u32); } else { spec += &format!(".{p}"); }
            }
            spec.push(conv);
            slots.push(v);
            for (i, x) in slots.iter().enumerate() {
                mem::w32(a + 4 * i as u32, *x);
            }
            let w: Vec<u16> = spec.encode_utf16().collect();
            let got = String::from_utf16_lossy(&printf_insert(&w, &Args { base: a }, &mut 0, true));
            mem::heap_free(a);
            let mut buf = [0u8; 256];
            let fmt = std::ffi::CString::new(format!("%{spec}")).unwrap();
            let n = unsafe {
                match (width.is_some() && star, prec.is_some() && star) {
                    (true, true) => libc::snprintf(buf.as_mut_ptr() as *mut _, 256, fmt.as_ptr(), slots[0] as i32, slots[1] as i32, v),
                    (true, false) | (false, true) => libc::snprintf(buf.as_mut_ptr() as *mut _, 256, fmt.as_ptr(), slots[0] as i32, v),
                    _ => libc::snprintf(buf.as_mut_ptr() as *mut _, 256, fmt.as_ptr(), v),
                }
            };
            let want = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
            proptest::prop_assert_eq!(got, want, "%{}", spec);
        }
    }
}
