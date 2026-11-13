//! A rich edit's RTF reduced to text (sake::rtf::to_text, EM_STREAMIN with SF_RTF, #164) on any
//! bytes: no panic, and the text written back as RTF (sake::rtf::from_text) reads as the same text.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let t = sake::rtf::to_text(data, 932);
    assert_eq!(sake::rtf::to_text(&sake::rtf::from_text(&t), 932), t);
});
