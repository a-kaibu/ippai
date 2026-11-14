//! Records: fields 0..=count, each NULL, an integer, a string or a stream.

use crate::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum Field {
    #[default]
    Null,
    Int(i32),
    Str(String),
    Stream(Vec<u8>),
}

impl Field {
    pub fn is_null(&self) -> bool {
        matches!(self, Field::Null) || matches!(self, Field::Str(s) if s.is_empty())
    }
    /// MsiRecordGetString's text: integers in decimal, streams and NULL empty.
    pub fn text(&self) -> String {
        match self {
            Field::Int(i) => i.to_string(),
            Field::Str(s) => s.clone(),
            _ => String::new(),
        }
    }
    /// MsiRecordGetInteger: strings that are whole decimal numbers, else NULL_INT.
    pub fn int(&self) -> i32 {
        match self {
            Field::Int(i) => *i,
            Field::Str(s) => parse_int(s).unwrap_or(NULL_INT),
            _ => NULL_INT,
        }
    }
}

/// Windows' string-to-integer for records: optional '-', digits only, no spaces.
pub fn parse_int(s: &str) -> Option<i32> {
    let (neg, d) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s),
    };
    if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // (digits past 32 bits wrap, as Windows' MsiRecordGetInteger does: "4294967297" is 1)
    let mut v: i32 = 0;
    for b in d.bytes() {
        v = v.wrapping_mul(10).wrapping_add((b - b'0') as i32);
    }
    Some(if neg { v.wrapping_neg() } else { v })
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Record {
    pub f: Vec<Field>,
    /// read positions of stream fields (MsiRecordReadStream)
    pub pos: Vec<usize>,
    /// the view and result row a fetched record came from (MsiViewModify)
    pub origin: Option<(u64, usize)>,
}

impl Record {
    pub fn new(count: usize) -> Record {
        Record { f: vec![Field::Null; count + 1], pos: vec![0; count + 1], origin: None }
    }
    pub fn count(&self) -> usize {
        self.f.len() - 1
    }
    pub fn get(&self, i: usize) -> &Field {
        static NULL: Field = Field::Null;
        self.f.get(i).unwrap_or(&NULL)
    }
    pub fn set(&mut self, i: usize, v: Field) -> R<()> {
        if i >= self.f.len() {
            return Err(ERROR_INVALID_PARAMETER);
        }
        self.f[i] = match v {
            Field::Str(s) if s.is_empty() => Field::Null,
            v => v,
        };
        self.pos[i] = 0;
        Ok(())
    }
    pub fn clear(&mut self) {
        for f in self.f.iter_mut() {
            *f = Field::Null;
        }
    }
    /// MsiRecordDataSize: string length, 4 (2 for 16-bit columns: not known here) for
    /// integers, stream length.
    pub fn data_size(&self, i: usize) -> u32 {
        match self.get(i) {
            Field::Null => 0,
            Field::Int(_) => 4,
            Field::Str(s) => s.encode_utf16().count() as u32,
            Field::Stream(d) => d.len() as u32,
        }
    }
    /// MsiRecordReadStream: up to `n` bytes from the read position.
    pub fn read_stream(&mut self, i: usize, n: usize) -> R<Vec<u8>> {
        let Some(Field::Stream(d)) = self.f.get(i) else {
            return Err(ERROR_DATATYPE_MISMATCH);
        };
        let at = self.pos[i].min(d.len());
        // (saturating: a count near usize::MAX after a first read wrapped `end` below `at`
        // and the slice panicked, review A-7)
        let end = at.saturating_add(n).min(d.len());
        let out = d[at..end].to_vec();
        self.pos[i] = end;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(parse_int("123"), Some(123));
        assert_eq!(parse_int("-2147483648"), Some(i32::MIN));
        assert_eq!(parse_int("2147483648"), Some(i32::MIN));
        assert_eq!(parse_int("1000000000000000"), Some(-1530494976));
        assert_eq!(parse_int("4294967297"), Some(1));
        assert_eq!(parse_int(" 1"), None);
        assert_eq!(parse_int("1a"), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(Field::Str("42".into()).int(), 42);
    }

    #[test]
    fn stream_reads() {
        let mut r = Record::new(1);
        r.set(1, Field::Stream(b"abcdef".to_vec())).unwrap();
        assert_eq!(r.read_stream(1, 4).unwrap(), b"abcd");
        assert_eq!(r.read_stream(1, 4).unwrap(), b"ef");
        assert_eq!(r.read_stream(1, 4).unwrap(), b"");
        assert_eq!(r.set(2, Field::Null), Err(ERROR_INVALID_PARAMETER));
    }

    /// Review A-7: a count near usize::MAX after a first read (`at + n` wrapped below `at`).
    #[test]
    fn huge_counts_read_the_rest() {
        for n in [usize::MAX, usize::MAX - 1, usize::MAX - 5, u32::MAX as usize] {
            let mut r = Record::new(1);
            r.set(1, Field::Stream(b"abcdef".to_vec())).unwrap();
            assert_eq!(r.read_stream(1, 2).unwrap(), b"ab");
            assert_eq!(r.read_stream(1, n).unwrap(), b"cdef");
            assert_eq!(r.read_stream(1, n).unwrap(), b"");
        }
    }

    proptest::proptest! {
        /// Any counts in a row: the reads are the stream in order, never past its end.
        #[test]
        fn reads_in_order(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64), ns in proptest::collection::vec(proptest::prop_oneof![0usize..10, proptest::prelude::Just(usize::MAX), (usize::MAX - 16)..=usize::MAX], 1..12)) {
            let mut r = Record::new(1);
            r.set(1, Field::Stream(data.clone())).unwrap();
            let mut got = Vec::new();
            for &n in &ns {
                let b = r.read_stream(1, n).unwrap();
                proptest::prop_assert!(b.len() <= n);
                got.extend(b);
            }
            proptest::prop_assert_eq!(&got[..], &data[..got.len()]);
        }
    }
}
