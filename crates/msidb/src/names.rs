//! Stream names in the compound file: pairs of [0-9A-Za-z._] packed into one character
//! (0x3800 + second << 6 + first), a lone one as 0x4800 + it, and tables prefixed by 0x4840.

pub const TABLE_PREFIX: char = '\u{4840}';

fn b64(c: char) -> Option<u32> {
    Some(match c {
        '0'..='9' => c as u32 - '0' as u32,
        'A'..='Z' => c as u32 - 'A' as u32 + 10,
        'a'..='z' => c as u32 - 'a' as u32 + 36,
        '.' => 62,
        '_' => 63,
        _ => return None,
    })
}

fn unb64(v: u32) -> char {
    char::from_u32(match v {
        0..=9 => '0' as u32 + v,
        10..=35 => 'A' as u32 + v - 10,
        36..=61 => 'a' as u32 + v - 36,
        62 => '.' as u32,
        _ => '_' as u32,
    })
    .unwrap()
}

pub fn encode(name: &str, table: bool) -> String {
    let mut out = String::new();
    if table {
        out.push(TABLE_PREFIX);
    }
    let mut it = name.chars().peekable();
    while let Some(c) = it.next() {
        match b64(c) {
            Some(a) => match it.peek().and_then(|&n| b64(n)) {
                Some(b) => {
                    it.next();
                    out.push(char::from_u32(0x3800 + (b << 6) + a).unwrap());
                }
                None => out.push(char::from_u32(0x4800 + a).unwrap()),
            },
            None => out.push(c),
        }
    }
    out
}

/// The name and whether it is a table's stream.
pub fn decode(name: &str) -> (String, bool) {
    let mut out = String::new();
    let mut it = name.chars().peekable();
    let table = it.peek() == Some(&TABLE_PREFIX);
    if table {
        it.next();
    }
    for c in it {
        let v = c as u32;
        if (0x3800..0x4800).contains(&v) {
            out.push(unb64((v - 0x3800) & 0x3f));
            out.push(unb64((v - 0x3800) >> 6));
        } else if (0x4800..0x4840).contains(&v) {
            out.push(unb64(v - 0x4800));
        } else {
            out.push(c);
        }
    }
    (out, table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn known() {
        assert_eq!(encode("_Columns", true), "\u{4840}\u{3b3f}\u{43f2}\u{4438}\u{45b1}");
        assert_eq!(encode("_Tables", true), "\u{4840}\u{3f7f}\u{4164}\u{422f}\u{4836}");
        assert_eq!(encode("App.exe", false), "\u{44ca}\u{47b3}\u{46e8}\u{4828}");
        assert_eq!(decode("\u{44ca}\u{47b3}\u{46e8}\u{4828}"), ("App.exe".into(), false));
        assert_eq!(decode("\u{5}SummaryInformation"), ("\u{5}SummaryInformation".into(), false));
    }

    proptest! {
        #[test]
        fn round_trip(s in "[0-9A-Za-z._#!あ-ん]{0,40}", t: bool) {
            prop_assert_eq!(decode(&encode(&s, t)), (s, t));
        }
    }
}
