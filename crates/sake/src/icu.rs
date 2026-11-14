#![allow(non_snake_case)]
//! icu.dll, the ICU that Windows 10 1903 and later carry: the break iterators (ubrk_*) over
//! UTF-16 text and the UText of utext_openUChars, which AdvHD (x64) uses to find where its
//! lines may wrap (#51). The boundaries are ICU4X's (icu_segmenter: the same CLDR / UAX #14 and
//! #29 rules as ICU4C). Line breaking is strict as ICU's root `line` rules (small kana do not
//! start a line); a Japanese locale takes ICU's `ja` rules, CSS's normal, where they may. Word
//! breaking is the rule-based one: ICU4C splits runs of kanji and kana by its dictionary, sake by
//! script (ICU4X's dictionary would match all but a word or two of icubrk.c, for 4 MB more wasm,
//! and games wrap lines by the line kind). Windows' answers: tests/winref/probes/icubrk.c.
//!
//! The exports are cdecl and unversioned (ubrk_open, not ubrk_open_64).

use crate::api::{implement, Ctx};
use crate::mem::{self, ps, r16, r32, w32, w64, wp};
use crate::HashMap;
use icu_segmenter::options::{LineBreakOptions, LineBreakStrictness, SentenceBreakInvariantOptions, WordBreakInvariantOptions};
use icu_segmenter::{GraphemeClusterSegmenter, LineSegmenter, SentenceSegmenter, WordSegmenter};
use std::sync::Mutex;

/// UErrorCode: U_ILLEGAL_ARGUMENT_ERROR (failures are > 0)
const ILLEGAL_ARGUMENT: u32 = 1;
const DONE: i32 = -1;
/// UBreakIteratorType
pub const CHARACTER: u32 = 0;
pub const WORD: u32 = 1;
pub const LINE: u32 = 2;
pub const SENTENCE: u32 = 3;

/// UErrorCode warnings ubrk_open leaves: the locale's rules were the root's
/// (U_USING_DEFAULT_WARNING), or a parent's (U_USING_FALLBACK_WARNING: ja_JP's line rules are
/// ja's)
const USING_DEFAULT: i32 = -127;
const USING_FALLBACK: i32 = -128;

/// What ubrk_open's `locale` gives `kind`: whether its rules are Japanese, and the warning (0:
/// none) Windows' icu.dll leaves; only `ja` has rules of its own there, for line breaking.
pub fn locale_rules(kind: u32, locale: &str) -> (bool, i32) {
    let lang = locale.split(['_', '-', '@']).next().unwrap_or("");
    match (kind, lang) {
        (_, "" | "root") => (false, 0),
        (LINE, "ja") => (true, if locale == "ja" { 0 } else { USING_FALLBACK }),
        _ => (false, USING_DEFAULT),
    }
}

/// The boundaries of `text` (UTF-16 offsets, 0 and its length among them) by `kind`, by the
/// Japanese rules if `ja`; None: not a kind ICU has.
pub fn boundaries(kind: u32, text: &[u16], ja: bool) -> Option<Vec<i32>> {
    let mut b: Vec<i32> = match kind {
        CHARACTER => GraphemeClusterSegmenter::new().segment_utf16(text).map(|i| i as i32).collect(),
        WORD => WordSegmenter::new_for_non_complex_scripts(WordBreakInvariantOptions::default()).segment_utf16(text).map(|i| i as i32).collect(),
        LINE => {
            let mut o = LineBreakOptions::default();
            o.strictness = Some(if ja { LineBreakStrictness::Normal } else { LineBreakStrictness::Strict });
            LineSegmenter::new_for_non_complex_scripts(o).segment_utf16(text).map(|i| i as i32).collect()
        }
        SENTENCE => SentenceSegmenter::new(SentenceBreakInvariantOptions::default()).segment_utf16(text).map(|i| i as i32).collect(),
        _ => return None,
    };
    // (an empty text has the one boundary 0)
    if b.first() != Some(&0) {
        b.insert(0, 0);
    }
    b.dedup();
    Some(b)
}

/// A break iterator: its kind, the boundaries of its text, which one it is at.
#[derive(Clone, Debug, PartialEq)]
pub struct Brk {
    pub kind: u32,
    /// the locale's rules are Japanese
    pub ja: bool,
    pub bounds: Vec<i32>,
    pub at: usize,
}

impl Brk {
    pub fn new(kind: u32, ja: bool) -> Brk {
        Brk { kind, ja, bounds: vec![0], at: 0 }
    }
    pub fn set_text(&mut self, text: &[u16]) {
        self.bounds = boundaries(self.kind, text, self.ja).unwrap_or_else(|| vec![0]);
        self.at = 0;
    }
    fn end(&self) -> usize {
        self.bounds.len() - 1
    }
    pub fn current(&self) -> i32 {
        self.bounds[self.at]
    }
    pub fn first(&mut self) -> i32 {
        self.at = 0;
        self.current()
    }
    pub fn last(&mut self) -> i32 {
        self.at = self.end();
        self.current()
    }
    /// The next boundary; DONE at the end (where it stays).
    pub fn next(&mut self) -> i32 {
        if self.at >= self.end() {
            return DONE;
        }
        self.at += 1;
        self.current()
    }
    pub fn previous(&mut self) -> i32 {
        if self.at == 0 {
            return DONE;
        }
        self.at -= 1;
        self.current()
    }
    /// The first boundary past `off`; DONE (at the last one) when there is none.
    pub fn following(&mut self, off: i32) -> i32 {
        // (bounds rise: a binary search)
        let i = self.bounds.partition_point(|&b| b <= off);
        match (i < self.bounds.len()).then_some(i) {
            Some(i) => {
                self.at = i;
                self.current()
            }
            None => {
                self.at = self.end();
                DONE
            }
        }
    }
    /// The last boundary before `off`; DONE (at the first one) when there is none.
    pub fn preceding(&mut self, off: i32) -> i32 {
        match self.bounds.partition_point(|&b| b < off).checked_sub(1) {
            Some(i) => {
                self.at = i;
                self.current()
            }
            None => {
                self.at = 0;
                DONE
            }
        }
    }
    /// Whether `off` is a boundary: there if so, else at the following one.
    pub fn is_boundary(&mut self, off: i32) -> bool {
        if let Ok(i) = self.bounds.binary_search(&off) {
            self.at = i;
            return true;
        }
        self.following(off);
        false
    }
}

static BRKS: Mutex<Option<HashMap<u32, Brk>>> = Mutex::new(None);

fn with<R>(b: u32, f: impl FnOnce(&mut Brk) -> R) -> Option<R> {
    BRKS.lock().unwrap().get_or_insert_with(HashMap::default).get_mut(&b).map(f)
}

fn failed(status: u32) -> bool {
    status != 0 && r32(status) as i32 > 0
}
fn fail(status: u32, code: u32) {
    if status != 0 {
        w32(status, code);
    }
}

/// The longest text sake segments (UTF-16 units): a longer length, or a text with no NUL in
/// as many units, is a garbage argument (U_ILLEGAL_ARGUMENT_ERROR), not gigabytes read
pub const MAX_TEXT: i64 = 1 << 24;

/// UTF-16 text at `p`, `len` units (-1: up to its NUL); None past MAX_TEXT or for a length
/// below -1
fn uchars(p: u32, len: i64) -> Option<Vec<u16>> {
    if p == 0 {
        return Some(Vec::new());
    }
    if len > MAX_TEXT || len < -1 {
        return None;
    }
    if len >= 0 {
        return Some((0..len as u32).map(|i| r16(p.wrapping_add(2 * i))).collect());
    }
    let mut v = Vec::new();
    loop {
        let u = r16(p.wrapping_add(2 * v.len() as u32));
        if u == 0 {
            return Some(v);
        }
        if v.len() as i64 >= MAX_TEXT {
            return None;
        }
        v.push(u);
    }
}

// ---------------------------------------------------------------------------------------------
// UText (utext.h: public, a caller may hold it on its stack)

const UTEXT_MAGIC: u32 = 0x345a_d82c;
const UTEXT_HEAP_ALLOCATED: u32 = 1;
/// (magic, flags, sizeOfStruct, chunkNativeLimit, nativeIndexingLimit, chunkLength,
/// chunkContents, context, a) offsets; sizeof(UText)
fn utext_layout() -> ([u32; 9], u32) {
    if ps() == 8 { ([0, 4, 12, 16, 28, 44, 48, 72, 112], 160) } else { ([0, 4, 12, 16, 28, 44, 48, 60, 80], 128) }
}

/// The text a UText of utext_openUChars holds.
fn utext_text(ut: u32) -> Option<Vec<u16>> {
    let ([magic, _, _, _, _, _, _, context, a], _) = utext_layout();
    if ut == 0 || r32(ut + magic) != UTEXT_MAGIC {
        return None;
    }
    uchars(mem::rp(ut + context), mem::r64(ut + a) as i64)
}

/// utext_openUChars(UText *ut, const UChar *s, int64_t length, UErrorCode *status)
fn utext_openUChars(c: &mut Ctx) -> u32 {
    let (ut, s) = (c.a(0), c.a(1));
    let (len, status) = if mem::x64() { (c.a64(2) as i64, c.a(3)) } else { ((c.a(2) as u64 | (c.a(3) as u64) << 32) as i64, c.a(4)) };
    if failed(status) {
        return ut;
    }
    if (s == 0 && len != 0) || len < -1 {
        fail(status, ILLEGAL_ARGUMENT);
        return ut;
    }
    let Some(text) = uchars(s, len) else {
        fail(status, ILLEGAL_ARGUMENT);
        return ut;
    };
    let n = text.len() as u64;
    let ([magic, flags, size, limit, indexing, chunk_len, contents, context, a], sz) = utext_layout();
    let (ut, heap) = if ut == 0 { (mem::heap_alloc(sz, true), UTEXT_HEAP_ALLOCATED) } else { (ut, r32(ut + flags) & UTEXT_HEAP_ALLOCATED) };
    if heap == 0 && r32(ut + magic) != UTEXT_MAGIC {
        fail(status, ILLEGAL_ARGUMENT);
        return ut;
    }
    w32(ut + magic, UTEXT_MAGIC);
    w32(ut + flags, heap);
    w32(ut + size, sz);
    w64(ut + limit, n);
    w32(ut + indexing, n as u32);
    w32(ut + chunk_len, n as u32);
    wp(ut + contents, s);
    wp(ut + context, s);
    w64(ut + a, n);
    ut
}

/// utext_close(UText *): NULL when it was allocated by the open, else `ut` itself
fn utext_close(c: &mut Ctx) -> u32 {
    let ut = c.a(0);
    let ([magic, flags, ..], _) = utext_layout();
    if ut == 0 || r32(ut + magic) != UTEXT_MAGIC {
        return ut;
    }
    if r32(ut + flags) & UTEXT_HEAP_ALLOCATED != 0 {
        w32(ut + magic, 0);
        mem::heap_free(ut);
        return 0;
    }
    ut
}

// ---------------------------------------------------------------------------------------------
// ubrk_*

/// ubrk_open(UBreakIteratorType, const char *locale, const UChar *text, int32_t length, UErrorCode *)
fn ubrk_open(c: &mut Ctx) -> u32 {
    let (kind, text, len, status) = (c.a(0), c.a(2), c.a(3) as i32, c.a(4));
    if failed(status) {
        return 0;
    }
    if kind > SENTENCE {
        fail(status, ILLEGAL_ARGUMENT);
        return 0;
    }
    let locale = if c.a(1) == 0 { String::new() } else { String::from_utf8_lossy(&mem::cstr(c.a(1))).into_owned() };
    let (ja, warn) = locale_rules(kind, &locale);
    let mut b = Brk::new(kind, ja);
    if text != 0 {
        let Some(t) = uchars(text, len as i64) else {
            fail(status, ILLEGAL_ARGUMENT);
            return 0;
        };
        b.set_text(&t);
    }
    // (a NULL status is no place to write the warning to)
    if warn != 0 {
        fail(status, warn as u32);
    }
    let h = mem::heap_alloc(16, true);
    BRKS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, b);
    h
}
/// ubrk_setText(bi, const UChar *, int32_t, UErrorCode *)
fn ubrk_setText(c: &mut Ctx) -> u32 {
    let (b, text, len, status) = (c.a(0), c.a(1), c.a(2) as i32, c.a(3));
    if failed(status) {
        return 0;
    }
    let Some(t) = uchars(text, len as i64) else {
        fail(status, ILLEGAL_ARGUMENT);
        return 0;
    };
    if with(b, |k| k.set_text(&t)).is_none() {
        fail(status, ILLEGAL_ARGUMENT);
    }
    0
}
/// ubrk_setUText(bi, UText *, UErrorCode *)
fn ubrk_setUText(c: &mut Ctx) -> u32 {
    let (b, ut, status) = (c.a(0), c.a(1), c.a(2));
    if failed(status) {
        return 0;
    }
    match utext_text(ut) {
        Some(t) if with(b, |k| k.set_text(&t)).is_some() => {}
        _ => fail(status, ILLEGAL_ARGUMENT),
    }
    0
}
fn ubrk_close(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    if BRKS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&b).is_some() {
        mem::heap_free(b);
    }
    0
}
fn step(c: &mut Ctx, f: fn(&mut Brk) -> i32) -> u32 {
    with(c.a(0), f).unwrap_or(DONE) as u32
}
fn ubrk_current(c: &mut Ctx) -> u32 {
    step(c, |b| b.current())
}
fn ubrk_next(c: &mut Ctx) -> u32 {
    step(c, Brk::next)
}
fn ubrk_previous(c: &mut Ctx) -> u32 {
    step(c, Brk::previous)
}
fn ubrk_first(c: &mut Ctx) -> u32 {
    step(c, Brk::first)
}
fn ubrk_last(c: &mut Ctx) -> u32 {
    step(c, Brk::last)
}
fn ubrk_following(c: &mut Ctx) -> u32 {
    let off = c.a(1) as i32;
    with(c.a(0), |b| b.following(off)).unwrap_or(DONE) as u32
}
fn ubrk_preceding(c: &mut Ctx) -> u32 {
    let off = c.a(1) as i32;
    with(c.a(0), |b| b.preceding(off)).unwrap_or(DONE) as u32
}
fn ubrk_isBoundary(c: &mut Ctx) -> u32 {
    let off = c.a(1) as i32;
    with(c.a(0), |b| b.is_boundary(off)).unwrap_or(false) as u32
}

/// uloc_getDefault(): a Japanese Windows' "ja_JP"
fn uloc_getDefault(_: &mut Ctx) -> u32 {
    static S: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        let p = mem::heap_alloc(8, true);
        mem::write(p, b"ja_JP\0");
        p
    })
}

pub fn init() {
    macro_rules! reg {
        ($($f:ident),*) => { $(implement("icu", stringify!($f), $f);)* };
    }
    reg!(utext_openUChars, utext_close, ubrk_open, ubrk_setText, ubrk_setUText, ubrk_close, ubrk_current, ubrk_next, ubrk_previous, ubrk_first, ubrk_last, ubrk_following, ubrk_preceding, ubrk_isBoundary, uloc_getDefault);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn japanese_lines_keep_small_kana_and_closing_marks() {
        // 「ちょっと、待って。」: no break before ょ / っ (strict), 、 。 」, nor after 「
        let t = u("「ちょっと、待って。」");
        let b = boundaries(LINE, &t, false).unwrap();
        assert_eq!(b, [0, 4, 6, 8, 11]);
        // English: after the spaces
        assert_eq!(boundaries(LINE, &u("a quick fox"), false).unwrap(), [0, 2, 8, 11]);
    }

    /// Windows' icu.dll (icubrk.c) with "ja_JP": line breaks before small kana and ー as ICU's
    /// ja rules allow (not after 「 nor before 、。」！？), words by the dictionary, and the
    /// warnings: line falls back to ja's rules, the rest to the root's
    #[test]
    fn japanese_locale_as_windows() {
        let t = u("「ちょっと、待って。」あぁーッ ト・漢字！？".replace(' ', "").as_str());
        assert_eq!(boundaries(LINE, &t, true).unwrap(), [0, 2, 3, 4, 6, 7, 8, 11, 12, 13, 14, 15, 17, 18, 21]);
        // (Windows' dictionary: 0 1 5 6 7 9 10 11 13 16 17 19 20 21; by script, 待って is one)
        assert_eq!(boundaries(WORD, &t, false).unwrap(), [0, 1, 5, 6, 9, 10, 11, 13, 16, 17, 19, 20, 21]);
        assert_eq!(boundaries(SENTENCE, &t, false).unwrap(), [0, 11, 21]);
        assert_eq!(locale_rules(LINE, "ja_JP"), (true, USING_FALLBACK));
        assert_eq!(locale_rules(LINE, "ja"), (true, 0));
        for k in [CHARACTER, WORD, SENTENCE] {
            assert_eq!(locale_rules(k, "ja_JP"), (false, USING_DEFAULT));
        }
        assert_eq!(locale_rules(LINE, ""), (false, 0));
        assert_eq!(locale_rules(LINE, "en_US"), (false, USING_DEFAULT));
        // the root's line rules keep small kana: あっ。い breaks only before い
        assert_eq!(boundaries(LINE, &u("あっ。い"), false).unwrap(), [0, 3, 4]);
    }

    #[test]
    fn characters_keep_surrogates_and_emoji_sequences() {
        // a surrogate pair, a flag (two regional indicators), a family ZWJ sequence, e + acute
        let t = u("𠀋🇯🇵👨‍👩‍👧e\u{301}");
        let b = boundaries(CHARACTER, &t, false).unwrap();
        assert_eq!(b, [0, 2, 6, 14, 16]);
        assert_eq!(boundaries(CHARACTER, &[], false).unwrap(), [0]);
        assert_eq!(boundaries(9, &t, false), None);
    }

    #[test]
    fn iterator_moves_as_icu() {
        let mut b = Brk::new(LINE, false);
        b.set_text(&u("a quick fox"));
        assert_eq!((b.current(), b.next(), b.next(), b.next(), b.next(), b.current()), (0, 2, 8, 11, DONE, 11));
        assert_eq!((b.previous(), b.first(), b.previous(), b.last()), (8, 0, DONE, 11));
        assert_eq!((b.following(2), b.following(11), b.current()), (8, DONE, 11));
        assert_eq!((b.preceding(8), b.preceding(0), b.current()), (2, DONE, 0));
        assert_eq!((b.is_boundary(8), b.current(), b.is_boundary(5), b.current()), (true, 8, false, 8));
    }

    proptest! {
        /// Boundaries rise from 0 to the length, and never split a surrogate pair
        #[test]
        fn boundaries_rise_and_keep_pairs(s in "\\PC{0,40}", kind in 0u32..4, ja: bool) {
            let t = u(&s);
            let b = boundaries(kind, &t, ja).unwrap();
            prop_assert_eq!(b[0], 0);
            prop_assert_eq!(*b.last().unwrap(), t.len() as i32);
            prop_assert!(b.windows(2).all(|w| w[0] < w[1]));
            for &i in &b[1..b.len().max(2) - 1] {
                prop_assert!(!(0xdc00..0xe000).contains(&t[i as usize]));
            }
        }

        /// next() from the start visits every boundary in order, then DONE; following(x) is the
        /// first boundary past x
        #[test]
        fn walking_visits_every_boundary(s in "\\PC{0,30}", kind in 0u32..4, off in -2i32..40) {
            let t = u(&s);
            let mut b = Brk::new(kind, false);
            b.set_text(&t);
            let mut seen = vec![b.current()];
            loop {
                let n = b.next();
                if n == DONE { break; }
                seen.push(n);
            }
            prop_assert_eq!(&seen, &b.bounds);
            let want = b.bounds.iter().copied().find(|&x| x > off).unwrap_or(DONE);
            prop_assert_eq!(b.following(off), want);
            // the binary searches against linear scans of the same bounds
            let at = b.bounds.iter().position(|&x| x > off).unwrap_or(b.bounds.len() - 1);
            prop_assert_eq!(b.at, at);
            let want = b.bounds.iter().copied().rev().find(|&x| x < off).unwrap_or(DONE);
            prop_assert_eq!(b.preceding(off), want);
            prop_assert_eq!(b.at, b.bounds.iter().rposition(|&x| x < off).unwrap_or(0));
            let is = b.bounds.contains(&off);
            prop_assert_eq!(b.is_boundary(off), is);
            prop_assert_eq!(b.at, b.bounds.iter().position(|&x| x == off).unwrap_or(at));
        }
    }

    /// A garbage length (0x7fffffff units, or a UText whose length field was overwritten) is
    /// U_ILLEGAL_ARGUMENT_ERROR, not a multi-GB read; a NULL status takes no warning (it was
    /// written at guest address 0)
    #[test]
    fn garbage_lengths_and_null_status() {
        let _g = crate::test_guest::enter();
        let text = mem::heap_alloc(64, true);
        for (i, ch) in "あいう。".encode_utf16().enumerate() {
            mem::w16(text + 2 * i as u32, ch);
        }
        let loc = mem::heap_alloc(16, true);
        mem::write(loc, b"ja_JP\0");
        let st = mem::heap_alloc(4, true);
        let b = crate::test_guest::call(ubrk_open, &[LINE, loc, text, 0x7fff_ffff, st]);
        assert_eq!((b, r32(st)), (0, ILLEGAL_ARGUMENT));
        // ("ja_JP_x" falls back to ja: a warning, which a NULL status cannot take)
        mem::write(loc, b"ja_JP_x\0");
        let b = crate::test_guest::call(ubrk_open, &[LINE, loc, text, 4, 0]);
        assert_ne!(b, 0);
        crate::test_guest::call(ubrk_close, &[b]);
        w32(st, 0);
        let b = crate::test_guest::call(ubrk_open, &[LINE, loc, text, 4, st]);
        assert_ne!(b, 0);
        assert_eq!(r32(st), USING_FALLBACK as u32);
        w32(st, 0);
        crate::test_guest::call(ubrk_setText, &[b, text, (-5i32) as u32, st]);
        assert_eq!(r32(st), ILLEGAL_ARGUMENT);
        w32(st, 0);
        crate::test_guest::call(ubrk_setText, &[b, text, 4, st]);
        assert_eq!(r32(st), 0);
        crate::test_guest::call(ubrk_close, &[b]);
        // utext_openUChars with a length past MAX_TEXT (a 64-bit length: low, high)
        w32(st, 0);
        let ut = crate::test_guest::call(utext_openUChars, &[0, text, 0x7fff_ffff, 0, st]);
        assert_eq!((ut, r32(st)), (0, ILLEGAL_ARGUMENT));
        for p in [text, loc, st] {
            mem::heap_free(p);
        }
    }

    proptest! {
        /// uchars reads exactly `len` units, or up to the NUL; a length below -1 is none
        #[test]
        fn uchars_reads_what_it_is_told(units in proptest::collection::vec(1u16.., 0..40), len in -3i64..60) {
            let _g = crate::test_guest::enter();
            let p = mem::heap_alloc(2 * 64, true);
            for (i, &u) in units.iter().enumerate() {
                mem::w16(p + 2 * i as u32, u);
            }
            let got = uchars(p, len);
            mem::heap_free(p);
            match len {
                ..=-2 => prop_assert!(got.is_none()),
                -1 => prop_assert_eq!(got.unwrap(), units),
                n => {
                    let mut want = units.clone();
                    want.resize(64, 0);
                    want.truncate(n as usize);
                    prop_assert_eq!(got.unwrap(), want);
                }
            }
        }
    }
}
