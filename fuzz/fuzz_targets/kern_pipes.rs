//! sake's shared kernel pipes (sake::kern, review P1-4) under any sequence of writes, reads,
//! peeks, mode changes, a closed reader, and the state written to bytes and read back (what
//! another process gets): what is read is what was written, in order; at most the reader's
//! buffer (or PIPE_PIECE) waits in a byte pipe; a write is done only once what waits fits the
//! buffer; all pipes together hold at most PIPE_QUOTA; the state reads back as itself.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::kern::enc::Enc;
use sake::kern::{Kern, NewPipe, Write, PENDING, PIPE_QUOTA, SUCCESS};

const PID: u32 = 9;

fn state(k: &Kern) -> Vec<u8> {
    let mut v = Vec::new();
    k.put(&mut v);
    v
}

fuzz_target!(|data: &[u8]| {
    let Some((&sizes, mut ops)) = data.split_first() else { return };
    let mut k = Kern::default();
    // a byte pipe (CreatePipe) of some size, and a message pipe whose server reads messages
    let size = [0u32, 1, 16, 100, 4096, 9000][sizes as usize % 6];
    let room = if size == 0 { 4096 } else { size as usize };
    let (r, w) = k.create_anon(size, PID);
    let msg_in = [16u32, 512, 70000][(sizes as usize / 6) % 3];
    let p = NewPipe { access: 3, msg_type: true, msg_read: true, nonblock: false, max: 1, outsize: 512, insize: msg_in, timeout: 0, first: false };
    let s = k.create("fuzz", &p, PID).unwrap();
    let c = k.open("fuzz", true, true, PID).unwrap();

    let (mut sent, mut got) = (Vec::<u8>::new(), Vec::<u8>::new());
    // the byte write under way: its bytes and progress
    let mut pending: Option<(Vec<u8>, Write)> = None;
    let mut reader_gone = false;
    let (mut msgs, mut msgs_got) = (Vec::<u8>::new(), Vec::<u8>::new());
    let mut n = 0u8;
    while let Some((&op, rest)) = ops.split_first() {
        ops = rest;
        n = n.wrapping_add(31);
        let arg = ops.first().copied().unwrap_or(0);
        match op % 8 {
            // WriteFile on the byte pipe (or asked again)
            0 => {
                let (d, mut st) = pending.take().unwrap_or_else(|| ((0..arg as usize * 37).map(|i| n ^ i as u8).collect(), Write::default()));
                match k.write(w, &d, &mut st) {
                    Ok(len) => {
                        assert_eq!(len as usize, d.len());
                        assert!(reader_gone || d.is_empty() || k.avail(r) as usize <= room, "done with {} waiting in a {size} pipe", k.avail(r));
                        sent.extend(&d);
                    }
                    Err(PENDING) => pending = Some((d, st)),
                    Err(_) => assert!(reader_gone, "a write failed with its reader there"),
                }
            }
            // ReadFile on the byte pipe
            1 if !reader_gone => match k.read(r, arg as u32 * 50 + 1) {
                Ok((d, more)) => {
                    assert!(!more && !d.is_empty());
                    got.extend(d);
                }
                Err(e) => assert_eq!(e, PENDING),
            },
            // a message from the client (overlapped: not waited for)
            2 => {
                let d: Vec<u8> = (0..arg as usize).map(|i| n.wrapping_add(i as u8)).collect();
                match k.write(c, &d, &mut Write::default()) {
                    Ok(_) | Err(PENDING) => msgs.extend(&d),
                    Err(e) => assert_eq!(e, sake::kern::QUOTA_EXCEEDED),
                }
            }
            // the server reads a message (or part of one)
            3 => match k.read(s, arg as u32 + 1) {
                Ok((d, _)) => msgs_got.extend(d),
                Err(e) => assert_eq!(e, PENDING),
            },
            4 if !reader_gone => {
                let pk = k.peek(r, arg as u32 * 9).unwrap();
                assert_eq!(pk.avail, k.avail(r));
            }
            5 => assert_eq!(k.set_mode(c, None, Some(arg & 1 != 0)), SUCCESS),
            // another process reads the state back
            6 => {
                let b = state(&k);
                let back = Kern::get(&mut b.as_slice()).expect("the state reads back");
                assert!(state(&back) == b);
                k = back;
            }
            7 if arg == 0xff && !reader_gone => {
                k.release(r, PID);
                reader_gone = true;
            }
            _ => {}
        }
        if !reader_gone {
            assert!(k.avail(r) as usize <= room.max(4096), "{} waiting in a {size} pipe", k.avail(r));
            // what was read is what was written, in order (the pending write's bytes come after)
            let all: Vec<u8> = sent.iter().chain(pending.iter().flat_map(|p| p.0.iter())).copied().collect();
            assert!(got.len() <= all.len() && all[..got.len()] == got[..]);
        }
        assert!(msgs_got.len() <= msgs.len() && msgs[..msgs_got.len()] == msgs_got[..]);
        assert!(k.avail(r) as usize + k.avail(s) as usize <= PIPE_QUOTA + 70000);
    }
});
