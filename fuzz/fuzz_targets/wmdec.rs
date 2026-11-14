//! sake's Windows Media decoders (wmdec: the codec for a stream's format, its pictures and
//! sound put into an RGB24 / RGB32 / PCM output of any size) on any format bytes and objects:
//! no panic, and every picture the size of its output.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    sake::wmdec_fuzz(data);
});
