//! A file's bytes in memory, kept in pages of PAGE bytes instead of one Vec: a big file written
//! a piece at a time (an installer extracting a 457 MB app.asar) never needs one block of twice
//! its size, as a doubling Vec does (#234), and never moves what is already written. A page holds
//! only what was written into it (bytes past its end, up to the file's length, read as zero), so
//! a small file costs its size and a file made long by set_len costs nothing until written.

/// Bytes per page.
pub const PAGE: usize = 1 << 20;

#[derive(Default, Clone)]
pub struct Pages {
    len: u64,
    /// `ceil(len / PAGE)` pages; page `i` holds at most `min(PAGE, len - i * PAGE)` bytes.
    pages: Vec<Vec<u8>>,
}

impl std::fmt::Debug for Pages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pages({} bytes)", self.len)
    }
}

impl Pages {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_slice(b: &[u8]) -> Self {
        Pages { len: b.len() as u64, pages: b.chunks(PAGE).map(<[u8]>::to_vec).collect() }
    }

    pub fn from_vec(v: Vec<u8>) -> Self {
        if v.len() <= PAGE {
            let len = v.len() as u64;
            return Pages { len, pages: if v.is_empty() { Vec::new() } else { vec![v] } };
        }
        Self::from_slice(&v)
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copies bytes from `off` into `buf`; how many (fewer at the end of the file).
    pub fn read(&self, off: u64, buf: &mut [u8]) -> usize {
        if off >= self.len {
            return 0;
        }
        let n = buf.len().min((self.len - off) as usize);
        let mut done = 0;
        while done < n {
            let at = off + done as u64;
            let (i, o) = ((at / PAGE as u64) as usize, (at % PAGE as u64) as usize);
            let k = (PAGE - o).min(n - done);
            let page = &self.pages[i];
            let have = page.len().saturating_sub(o).min(k);
            if have > 0 {
                buf[done..done + have].copy_from_slice(&page[o..o + have]);
            }
            buf[done + have..done + k].fill(0);
            done += k;
        }
        n
    }

    /// Writes `data` at `off`, making the file longer (zeros between) when it ends past it.
    pub fn write(&mut self, off: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let end = off + data.len() as u64;
        if end > self.len {
            self.set_len(end);
        }
        let mut done = 0;
        while done < data.len() {
            let at = off + done as u64;
            let (i, o) = ((at / PAGE as u64) as usize, (at % PAGE as u64) as usize);
            let k = (PAGE - o).min(data.len() - done);
            let page = &mut self.pages[i];
            if page.len() < o + k {
                let want = o + k;
                if page.capacity() < want {
                    // doubling, but never past a page
                    let cap = want.max(page.capacity() * 2).min(PAGE);
                    page.reserve_exact(cap - page.len());
                }
                page.resize(want, 0);
            }
            page[o..o + k].copy_from_slice(&data[done..done + k]);
            done += k;
        }
    }

    /// Whether the file can be `end` bytes long: its page list made room for (false when the
    /// memory is not there; a page itself is at most PAGE bytes, its data what is written).
    pub fn room(&mut self, end: u64) -> bool {
        let Ok(count) = usize::try_from(end.div_ceil(PAGE as u64)) else { return false };
        count <= self.pages.len() || self.pages.try_reserve(count - self.pages.len()).is_ok()
    }

    /// Cuts or lengthens (with zeros) the file to `n` bytes.
    pub fn set_len(&mut self, n: u64) {
        let count = n.div_ceil(PAGE as u64) as usize;
        if n < self.len {
            self.pages.truncate(count);
            if let Some(last) = self.pages.last_mut() {
                let keep = (n - (count as u64 - 1) * PAGE as u64) as usize;
                last.truncate(keep);
                if last.capacity() > 2 * keep.max(4096) {
                    last.shrink_to_fit();
                }
            }
        } else {
            self.pages.resize_with(count, Vec::new);
        }
        self.len = n;
    }

    /// The bytes in order, a piece at a time (zeros for what was never written).
    pub fn for_each(&self, mut f: impl FnMut(&[u8])) {
        let zeros = [0u8; 4096];
        for (i, page) in self.pages.iter().enumerate() {
            let size = (self.len - i as u64 * PAGE as u64).min(PAGE as u64) as usize;
            f(page);
            let mut rest = size - page.len();
            while rest > 0 {
                let k = rest.min(zeros.len());
                f(&zeros[..k]);
                rest -= k;
            }
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len as usize);
        self.for_each(|b| v.extend_from_slice(b));
        v
    }

    /// Bytes held in memory (what was written, not the file's length).
    pub fn held(&self) -> usize {
        self.pages.iter().map(Vec::capacity).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Write(u64, Vec<u8>),
        SetLen(u64),
    }

    /// Offsets around page edges, where the arithmetic can slip.
    fn off() -> impl Strategy<Value = u64> {
        prop_oneof![0u64..4 * PAGE as u64, (0u64..5).prop_flat_map(|p| (p * PAGE as u64).saturating_sub(8)..p * PAGE as u64 + 8)]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (off(), prop::collection::vec(any::<u8>(), 0..3000)).prop_map(|(o, d)| Op::Write(o, d)),
            1 => off().prop_map(Op::SetLen),
        ]
    }

    fn apply(model: &mut Vec<u8>, op: &Op) {
        match op {
            Op::Write(o, d) => {
                if d.is_empty() {
                    return;
                }
                let end = *o as usize + d.len();
                if model.len() < end {
                    model.resize(end, 0);
                }
                model[*o as usize..end].copy_from_slice(d);
            }
            Op::SetLen(n) => model.resize(*n as usize, 0),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Any writes and truncations read back as a plain Vec would hold them, from any offset.
        #[test]
        fn pages_match_a_vec(ops in prop::collection::vec(op(), 1..20), probes in prop::collection::vec((off(), 0usize..5000), 8)) {
            let (mut p, mut m) = (Pages::new(), Vec::new());
            for o in &ops {
                match o {
                    Op::Write(at, d) => p.write(*at, d),
                    Op::SetLen(n) => p.set_len(*n),
                }
                apply(&mut m, o);
                prop_assert_eq!(p.len(), m.len() as u64);
            }
            prop_assert!(p.to_vec() == m, "contents differ");
            for (at, n) in probes {
                let mut got = vec![0xaa; n];
                let k = p.read(at, &mut got);
                let start = (at as usize).min(m.len());
                let want = &m[start..(start + n).min(m.len())];
                prop_assert_eq!(k, want.len());
                prop_assert!(&got[..k] == want);
            }
            prop_assert!(p.held() <= p.pages.len() * PAGE);
        }

        /// A file written in pieces holds the same bytes whatever order the pieces come in.
        #[test]
        fn write_order_does_not_matter(data in prop::collection::vec(any::<u8>(), 1..(3 * PAGE + 77)).no_shrink(), cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..12), order in any::<u64>()) {
            let mut at: Vec<usize> = cuts.iter().map(|c| c.index(data.len())).collect();
            at.extend([0, data.len()]);
            at.sort_unstable();
            at.dedup();
            let mut pieces: Vec<(usize, usize)> = at.windows(2).map(|w| (w[0], w[1])).collect();
            let mut s = order | 1;
            for i in (1..pieces.len()).rev() {
                s ^= s << 13; s ^= s >> 7; s ^= s << 17;
                pieces.swap(i, (s % (i as u64 + 1)) as usize);
            }
            let mut p = Pages::new();
            for (a, b) in pieces {
                p.write(a as u64, &data[a..b]);
            }
            prop_assert!(p.to_vec() == data);
        }

        /// Snapshots and exports go through from_slice / for_each: the same bytes come back.
        #[test]
        fn from_slice_round_trips(data in prop::collection::vec(any::<u8>(), 0..(2 * PAGE + 9)).no_shrink()) {
            prop_assert!(Pages::from_slice(&data).to_vec() == data);
            prop_assert!(Pages::from_vec(data.clone()).to_vec() == data);
        }
    }

    /// A 600 MB file written 64 KiB at a time (as nsis7z writes app.asar) never holds more than
    /// its own size plus one page, where a doubling Vec reserves up to twice its size in one block.
    #[test]
    fn room_says_when_the_page_list_cannot_be_made() {
        let mut p = Pages::from_slice(b"abc");
        assert!(p.room(10 << 30));
        assert!(!p.room(isize::MAX as u64 - 8));
        assert!(!p.room(u64::MAX));
        assert_eq!(p.len(), 3);
        let mut b = [0u8; 3];
        assert_eq!(p.read(0, &mut b), 3);
        assert_eq!(&b, b"abc");
    }

    #[test]
    fn a_big_file_written_in_pieces_holds_about_its_size() {
        let size = 600usize << 20;
        let piece = vec![7u8; 64 << 10];
        let mut p = Pages::new();
        let mut at = 0;
        while at < size {
            p.write(at as u64, &piece);
            at += piece.len();
        }
        assert_eq!(p.len(), size as u64);
        assert!(p.held() <= size + PAGE, "held {} for {}", p.held(), size);
        let mut b = [0u8; 3];
        assert_eq!(p.read(size as u64 - 2, &mut b), 2);
        assert_eq!(&b[..2], &[7, 7]);
    }

    /// set_len past the end costs nothing until written, and reads as zero.
    #[test]
    fn a_long_empty_file_holds_nothing() {
        let mut p = Pages::new();
        p.set_len(10 << 30);
        assert_eq!(p.held(), 0);
        let mut b = [1u8; 16];
        assert_eq!(p.read((5 << 30) + 3, &mut b), 16);
        assert_eq!(b, [0; 16]);
        p.write(1 << 30, b"x");
        assert!(p.held() <= 4096);
    }

    /// A small file costs about its size, not a page.
    #[test]
    fn a_small_file_holds_its_size() {
        let mut p = Pages::new();
        for i in 0..100u64 {
            p.write(i * 10, b"0123456789");
        }
        assert!(p.held() < 4096, "held {}", p.held());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Reads and writes stay inside their pages and the page the offset names: for any offset
    /// and length, the (page, offset in page, length in page) walk covers the range exactly once.
    #[kani::proof]
    #[kani::unwind(4)]
    fn page_walk_covers_the_range() {
        let off: u64 = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= 2 * PAGE && off < (1u64 << 40));
        let mut done = 0usize;
        let mut steps = 0;
        while done < n {
            let at = off + done as u64;
            let (i, o) = ((at / PAGE as u64) as usize, (at % PAGE as u64) as usize);
            let k = (PAGE - o).min(n - done);
            assert!(k > 0 && o + k <= PAGE);
            assert_eq!(i as u64 * PAGE as u64 + o as u64, at);
            done += k;
            steps += 1;
            assert!(steps <= 3);
        }
        assert_eq!(done, n);
    }

    /// set_len keeps the invariant the reads rely on: ceil(len / PAGE) pages, the last one no
    /// longer than what is left of the file.
    #[kani::proof]
    fn set_len_page_count() {
        let n: u64 = kani::any();
        kani::assume(n < (1u64 << 40));
        let count = n.div_ceil(PAGE as u64);
        if n > 0 {
            let keep = n - (count - 1) * PAGE as u64;
            assert!(keep >= 1 && keep <= PAGE as u64);
        } else {
            assert_eq!(count, 0);
        }
    }
}
