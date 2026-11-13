//! RTF as a rich edit control shows it, reduced to its text (EM_STREAMIN with SF_RTF: a game's
//! license agreement, ダンジョンズ＆シネステジア 体験版's eula.rtf, #164). Formatting is dropped;
//! what is kept is what the control would let you select: the characters, \par / \line as
//! CR LF, \tab, \'xx in the document's code page (\ansicpg, else \ansi = 1252 / the ANSI code
//! page), \uN with its \ucN fallback characters skipped, and the result text of fields. Groups
//! that are no text (font, color and style tables, \info, \pict, \* destinations ...) are
//! skipped whole.

/// Destinations whose contents are not text.
const SKIP: &[&[u8]] = &[
    b"fonttbl", b"colortbl", b"stylesheet", b"info", b"pict", b"object", b"header", b"headerl", b"headerr", b"headerf",
    b"footer", b"footerl", b"footerr", b"footerf", b"footnote", b"fldinst", b"themedata", b"colorschememapping",
    b"latentstyles", b"listtable", b"listoverridetable", b"rsidtbl", b"generator", b"xmlnstbl", b"datastore",
    b"filetbl", b"revtbl", b"pgdsctbl", b"objdata", b"nonshppict", b"bkmkstart", b"bkmkend", b"author", b"operator",
    b"title", b"subject", b"keywords", b"comment", b"doccomm", b"company", b"falt", b"panose", b"pntext", b"pntxta",
    b"pntxtb", b"listtext",
];

#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    uc: u32,
}

/// The text of `rtf` (UTF-16). `acp`: the code page of \'xx bytes when the document names
/// none (the ANSI one of the program).
pub fn to_text(rtf: &[u8], acp: u32) -> Vec<u16> {
    let mut out = Vec::new();
    let mut bytes: Vec<u8> = Vec::new(); // \'xx and plain bytes not decoded yet (DBCS pairs)
    let mut cp = acp;
    let mut st = Group { skip: false, uc: 1 };
    let mut stack: Vec<Group> = Vec::new();
    // fallback characters still to skip after a \uN
    let mut pending: u32 = 0;
    let mut i = 0;
    let flush = |bytes: &mut Vec<u8>, out: &mut Vec<u16>, cp: u32| {
        if !bytes.is_empty() {
            out.extend(crate::text::mb2w(cp, bytes));
            bytes.clear();
        }
    };
    // (deeply nested braces: a document is not that deep; bounded so that hostile input
    // cannot grow the stack without end)
    const DEPTH: usize = 4096;
    while i < rtf.len() {
        let b = rtf[i];
        match b {
            b'{' => {
                flush(&mut bytes, &mut out, cp);
                if stack.len() < DEPTH {
                    stack.push(st);
                }
                pending = 0;
                i += 1;
            }
            b'}' => {
                flush(&mut bytes, &mut out, cp);
                if let Some(g) = stack.pop() {
                    st = g;
                }
                pending = 0;
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&c) = rtf.get(i) else { break };
                if c.is_ascii_alphabetic() {
                    let s = i;
                    while i < rtf.len() && rtf[i].is_ascii_alphabetic() && i - s < 32 {
                        i += 1;
                    }
                    let word = &rtf[s..i];
                    let ns = i;
                    if i < rtf.len() && rtf[i] == b'-' {
                        i += 1;
                    }
                    while i < rtf.len() && rtf[i].is_ascii_digit() && i - ns < 11 {
                        i += 1;
                    }
                    let param: Option<i64> = std::str::from_utf8(&rtf[ns..i]).ok().and_then(|p| p.parse().ok());
                    if i < rtf.len() && rtf[i] == b' ' {
                        i += 1;
                    }
                    // a control word counts as one fallback character of a \uN
                    if pending > 0 {
                        pending -= 1;
                        continue;
                    }
                    if SKIP.contains(&word) {
                        st.skip = true;
                        continue;
                    }
                    match word {
                        b"ansicpg" => {
                            if let Some(p) = param.filter(|p| (1..65536).contains(p)) {
                                flush(&mut bytes, &mut out, cp);
                                cp = p as u32;
                            }
                        }
                        b"uc" => st.uc = param.unwrap_or(1).clamp(0, 16) as u32,
                        _ if st.skip => {}
                        b"par" | b"line" | b"sect" | b"page" => {
                            flush(&mut bytes, &mut out, cp);
                            out.extend([13, 10]);
                        }
                        b"tab" | b"cell" => {
                            flush(&mut bytes, &mut out, cp);
                            out.push(9);
                        }
                        b"row" => {
                            flush(&mut bytes, &mut out, cp);
                            out.extend([13, 10]);
                        }
                        b"emdash" => { flush(&mut bytes, &mut out, cp); out.push(0x2014) }
                        b"endash" => { flush(&mut bytes, &mut out, cp); out.push(0x2013) }
                        b"bullet" => { flush(&mut bytes, &mut out, cp); out.push(0x2022) }
                        b"lquote" => { flush(&mut bytes, &mut out, cp); out.push(0x2018) }
                        b"rquote" => { flush(&mut bytes, &mut out, cp); out.push(0x2019) }
                        b"ldblquote" => { flush(&mut bytes, &mut out, cp); out.push(0x201c) }
                        b"rdblquote" => { flush(&mut bytes, &mut out, cp); out.push(0x201d) }
                        b"u" => {
                            if let Some(p) = param {
                                flush(&mut bytes, &mut out, cp);
                                // (signed 16-bit: \u-3913 is U+F0B7)
                                out.push((p.rem_euclid(65536)) as u16);
                                pending = st.uc;
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                i += 1;
                match c {
                    b'\'' => {
                        let h = rtf.get(i..i + 2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                        if let Some(v) = h {
                            i += 2;
                            if pending > 0 {
                                pending -= 1;
                            } else if !st.skip {
                                bytes.push(v);
                            }
                        }
                    }
                    b'*' => st.skip = true,
                    _ if pending > 0 => pending -= 1,
                    _ if st.skip => {}
                    b'\\' | b'{' | b'}' => bytes.push(c),
                    b'~' => { flush(&mut bytes, &mut out, cp); out.push(0xa0) }
                    b'_' => { flush(&mut bytes, &mut out, cp); out.push(0x2011) }
                    b'\r' | b'\n' => {
                        flush(&mut bytes, &mut out, cp);
                        out.extend([13, 10]);
                    }
                    // \- (optional hyphen), \| \: (formula, index) and others: nothing
                    _ => {}
                }
            }
            // line breaks in the source are not text
            b'\r' | b'\n' => i += 1,
            _ => {
                if pending > 0 {
                    pending -= 1;
                } else if !st.skip {
                    bytes.push(b);
                }
                i += 1;
            }
        }
    }
    flush(&mut bytes, &mut out, cp);
    out
}

/// `text` as RTF whose `to_text` is `text` (the fuzz target's and tests' inverse): ASCII as it
/// is (\, {, } escaped), CR LF as \par, everything else \uN?.
pub fn from_text(text: &[u16]) -> Vec<u8> {
    let mut r = b"{\\rtf1\\ansi\\uc1 ".to_vec();
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        match c {
            13 if text.get(i + 1) == Some(&10) => {
                r.extend(b"\\par ");
                i += 1;
            }
            9 => r.extend(b"\\tab "),
            0x5c | 0x7b | 0x7d => {
                r.push(b'\\');
                r.push(c as u8);
            }
            0x20..=0x7e => r.push(c as u8),
            _ => r.extend(format!("\\u{}?", c as i16).bytes()),
        }
        i += 1;
    }
    r.push(b'}');
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn t(s: &[u8]) -> String {
        String::from_utf16_lossy(&to_text(s, 932))
    }

    #[test]
    fn license_like_document() {
        // what WordPad / Word write: tables first, then the text in \'xx of \ansicpg932
        let doc = b"{\\rtf1\\ansi\\ansicpg932\\deff0{\\fonttbl{\\f0\\fnil\\fcharset128 \\'82\\'6c\\'82\\'72 \\'83\\'53\\'83\\'56\\'83\\'62\\'83\\'4e;}}\r\n{\\colortbl ;\\red255\\green0\\blue0;}\r\n{\\*\\generator Msftedit 5.41.21.2510;}\\viewkind4\\uc1\\pard\\lang1041\\f0\\fs20\\'8e\\'67\\'97\\'70\\'8b\\'96\\'91\\'f8\\par\r\nOK {\\b bold} \\{x\\}\\par\r\n}";
        assert_eq!(t(doc), "使用許諾\r\nOK bold {x}\r\n");
    }

    #[test]
    fn unicode_and_fallbacks() {
        assert_eq!(t(b"{\\rtf1\\uc1 a\\u12354?b}"), "aあb");
        assert_eq!(t(b"{\\rtf1\\uc2 \\u12354\\'82\\'a0c}"), "あc");
        assert_eq!(t(b"{\\rtf1\\uc0 \\u12354 x}"), "あx");
        assert_eq!(t(b"{\\rtf1 \\u-3913?}"), "\u{f0b7}");
        // \ucN is scoped to its group
        assert_eq!(t(b"{\\rtf1 {\\uc2 \\u12354??}\\u12354?z}"), "ああz");
    }

    #[test]
    fn skipped_destinations_and_fields() {
        assert_eq!(t(b"{\\rtf1{\\info{\\title T}{\\author A}}{\\*\\unknowndest x}{\\field{\\*\\fldinst HYPERLINK \"u\"}{\\fldrslt link}} end}"), "link end");
        assert_eq!(t(b"{\\rtf1{\\pict\\wmetafile8 0102abcd}after}"), "after");
    }

    #[test]
    fn plain_and_malformed() {
        assert_eq!(t(b"hello"), "hello");
        assert_eq!(t(b"{\\rtf1 a}}}}b"), "ab");
        assert_eq!(t(b"{\\rtf1 a\\"), "a");
        assert_eq!(t(b"{\\rtf1 \\'8"), "8");
        assert_eq!(t(b"{\\rtf1 \\'zz}"), "zz");
    }

    proptest! {
        // any text written as RTF comes back
        #[test]
        fn from_text_round_trips(s in prop::collection::vec(prop_oneof![0x20u16..0x7f, 0x80u16..0xd800, 0xe000u16..=0xffff, Just(9u16)], 0..200)) {
            prop_assert_eq!(to_text(&from_text(&s), 932), s);
        }
        // CR LF pairs too (a lone CR or LF is not kept by to_text's source rule, and from_text writes it as \uN)
        #[test]
        fn paragraphs_round_trip(lines in prop::collection::vec("[a-zA-Z0-9 ]{0,20}", 1..10)) {
            let s: Vec<u16> = lines.join("\r\n").encode_utf16().collect();
            prop_assert_eq!(to_text(&from_text(&s), 932), s);
        }
        // any bytes: no panic, and the text of the text's RTF is the text
        #[test]
        fn any_bytes(b in prop::collection::vec(any::<u8>(), 0..400)) {
            let x = to_text(&b, 932);
            prop_assert_eq!(to_text(&from_text(&x), 932), x);
        }
        // a skipped destination never shows, whatever is in it
        #[test]
        fn fonttbl_never_shows(junk in "[a-z0-9 ;]{0,40}", text in "[A-Z]{1,10}") {
            let doc = format!("{{\\rtf1{{\\fonttbl{{\\f0 {junk};}}}}{text}}}");
            prop_assert_eq!(t(doc.as_bytes()), text);
        }
    }
}
