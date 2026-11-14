//! kura::pages::Pages (a file written in the page's memory, #234) against a plain Vec: any run
//! of writes, truncations and lengthenings, with offsets near page edges, reads back the same
//! from any offset, and never holds more than its pages' worth.
#![no_main]

use kura::pages::{Pages, PAGE};
use libfuzzer_sys::fuzz_target;

/// An offset from 3 bytes: page 0..4 and a nudge of -0x8000..0x8000 around its start.
fn off(b: &[u8]) -> u64 {
    let page = (b[0] as u64 % 4) * PAGE as u64;
    page + u16::from_le_bytes([b[1], b[2]]) as u64 - 0x8000u64.min(page)
}

fuzz_target!(|data: &[u8]| {
    let (mut p, mut m) = (Pages::new(), Vec::<u8>::new());
    let mut at = 0usize;
    while let Some(h) = data.get(at..at + 5) {
        at += 5;
        let pos = off(&h[1..4]);
        match h[0] % 3 {
            0 => {
                let n = (h[4] as usize * 37).min(data.len() - at);
                let bytes = &data[at..at + n];
                at += n;
                p.write(pos, bytes);
                if !bytes.is_empty() {
                    let end = pos as usize + bytes.len();
                    if m.len() < end {
                        m.resize(end, 0);
                    }
                    m[pos as usize..end].copy_from_slice(bytes);
                }
            }
            1 => {
                p.set_len(pos);
                m.resize(pos as usize, 0);
            }
            _ => {
                let mut buf = vec![0x5a; h[4] as usize * 40];
                let k = p.read(pos, &mut buf);
                let s = (pos as usize).min(m.len());
                assert_eq!(&buf[..k], &m[s..(s + buf.len()).min(m.len())]);
            }
        }
        assert_eq!(p.len(), m.len() as u64);
        assert!(p.held() <= m.len().div_ceil(PAGE) * PAGE);
    }
    assert!(p.to_vec() == m);
});
