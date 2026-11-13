//! sake's MP3 frame splitter (msacm32's MP3 codec: the stream comes in any slices): the same
//! frames however the stream is cut, each a whole frame the header describes, never more
//! bytes out than in.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::mp3frame::{header, Splitter};

fn split(data: &[u8], cuts: &[u8]) -> Vec<(sake::mp3frame::Header, Vec<u8>)> {
    let mut s = Splitter::default();
    let mut out = Vec::new();
    let mut at = 0;
    for &c in cuts {
        let end = (at + c as usize).min(data.len());
        s.push(&data[at..end], &mut out);
        at = end;
    }
    s.push(&data[at..], &mut out);
    s.finish(&mut out);
    out
}

fuzz_target!(|input: &[u8]| {
    let Some((&n, rest)) = input.split_first() else { return };
    let n = (n as usize % 16).min(rest.len());
    let (cuts, data) = rest.split_at(n);
    let whole = split(data, &[]);
    assert_eq!(split(data, cuts), whole, "cut at {cuts:?}");
    let mut total = 0;
    for (h, b) in &whole {
        assert_eq!(header(b), Some(*h));
        assert_eq!(b.len(), h.len);
        total += b.len();
    }
    assert!(total <= data.len());
});
