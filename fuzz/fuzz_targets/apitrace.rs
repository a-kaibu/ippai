//! apitrace files (the game traces sake replays): snappy, the trace container, the replay
//! stream. No panic on any bytes; snappy gives back what it compressed.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else { return };
    match k % 4 {
        0 => {
            let _ = apitrace::snappy::decompress(rest);
            let _ = apitrace::snappy::decompress_prefix(rest);
        }
        1 => assert_eq!(apitrace::snappy::decompress(&apitrace::snappy::compress(rest)).unwrap(), rest),
        2 => {
            let _ = apitrace::trace::unpack(rest);
            let _ = apitrace::trace::unpack_cut(rest);
        }
        _ => {
            let _ = apitrace::replay::read(rest);
        }
    }
});
