//! apitrace files (the game traces sake replays): snappy, the trace container, the replay
//! stream, the raw events. No panic (nor stack overflow, nor huge allocation) on any bytes; snappy gives back what it compressed.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else { return };
    match k % 5 {
        0 => {
            let _ = apitrace::snappy::decompress(rest);
            let _ = apitrace::snappy::decompress_prefix(rest);
        }
        1 => assert_eq!(apitrace::snappy::decompress(&apitrace::snappy::compress(rest)).unwrap(), rest),
        2 => {
            let _ = apitrace::trace::unpack(rest);
            let _ = apitrace::trace::unpack_cut(rest);
        }
        3 => {
            let _ = apitrace::replay::read(rest);
        }
        _ => {
            // (the events as they are, no snappy: deep nesting, huge argument indices)
            if let Ok(mut p) = apitrace::trace::Parser::new(rest) {
                while let Ok(Some(_)) = p.next_call() {}
            }
            let _ = apitrace::replay::convert(rest, None);
        }
    }
});
