//! cabinet.dll's FDICopy (sake::cabinet::copy) on any bytes as a cabinet: no panic, every
//! write goes to the handle fdintCOPY_FILE just returned and stays within that file's size,
//! and fdintCLOSE_FILE_INFO comes once per copied file, after its bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::cabinet::{copy, header, Note, Sink, FDINT_CABINET_INFO, FDINT_CLOSE_FILE_INFO, FDINT_COPY_FILE};

struct S {
    open: Option<(u32, u64, u64)>, // handle, size, written
    next: u32,
    skip: u8,
    infos: u32,
}

impl Sink for S {
    fn notify(&mut self, kind: u32, n: &Note) -> i32 {
        match kind {
            FDINT_CABINET_INFO => {
                self.infos += 1;
                assert_eq!(self.infos, 1);
                0
            }
            FDINT_COPY_FILE => {
                assert!(self.open.is_none(), "COPY_FILE before the last file's CLOSE_FILE_INFO");
                self.next += 1;
                // (every few files skipped: the decoder skips ahead in its folder)
                if self.next % 3 == (self.skip % 3) as u32 {
                    return 0;
                }
                self.open = Some((self.next, n.cb as u64, 0));
                self.next as i32
            }
            FDINT_CLOSE_FILE_INFO => {
                let (h, size, done) = self.open.take().expect("CLOSE_FILE_INFO without COPY_FILE");
                assert_eq!((n.hf, done), (h, size));
                1
            }
            k => panic!("notification {k}"),
        }
    }
    fn write(&mut self, hf: u32, data: &[u8]) -> u32 {
        let o = self.open.as_mut().expect("write outside a file");
        assert_eq!(hf, o.0);
        o.2 += data.len() as u64;
        assert!(o.2 <= o.1, "more bytes than the file's size");
        data.len() as u32
    }
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let _ = header(&data[1..]);
    let mut s = S { open: None, next: 0, skip: data[0], infos: 0 };
    let r = copy(&data[1..], &mut s);
    if r.is_ok() {
        assert!(s.open.is_none());
    }
});
