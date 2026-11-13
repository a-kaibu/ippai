//! imagehlp's MapAndLoad / ImageLoad header parsing (sake::imageload::layout) on any bytes as a
//! PE file: no panic, and a layout only for a file with MZ, an in-file "PE\0\0" at e_lfanew, a
//! PE32 / PE32+ optional header and the whole section table inside the file.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(l) = sake::imageload::layout(data) {
        assert_eq!(&data[..2], b"MZ");
        let nt = l.nt as usize;
        assert_eq!(&data[nt..nt + 4], b"PE\0\0");
        assert_eq!(nt as u32, u32::from_le_bytes(data[0x3c..0x40].try_into().unwrap()));
        let magic = u16::from_le_bytes([data[nt + 24], data[nt + 25]]);
        assert!(magic == 0x10b || magic == 0x20b);
        assert!(l.sections as usize + 40 * l.count as usize <= data.len());
        assert_eq!(l.count, u16::from_le_bytes([data[nt + 6], data[nt + 7]]) as u32);
    }
});
