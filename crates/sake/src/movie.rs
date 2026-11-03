//! Siglus's movies (`.omv`, Theora) decoded by the browser (WebCodecs, in hardware) instead of
//! by the game's own x86 Theora decoder, which took most of a phone's core in effect-heavy
//! scenes (aurora, smoke: 1920x1080 with alpha at 30 fps).
//!
//! SiglusEngine's movie player has one method that brings a frame into a 32-bit buffer:
//! `bool __thiscall get_frame(frame, dst, pitch, ?, force, ?)` (decoding every frame up to the
//! one asked for). Found by its code at load, it is patched to jump to `frame` here, which hands
//! out the frame `vnvm pack` made into VP9 (`<movie>.omv.vnv`, crates/vnvm/src/movie.rs) and
//! the page decodes (crates/vnhost/src/video.rs). Movies without that file, or no page to decode
//! (node), keep the game's decoder (the original code, through a trampoline).
//!
//! The player: +0xc4 how pixels go out (0: Y Cb Cr 255, 1: Y Cb Cr alpha, the game converting on
//! the GPU; 2: RGB), +0xc8 width, +0xcc height, +0x120/+0x124 the frame decoded last.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::api::{self, Ctx};
use crate::mem::{r32, w32};
use vnhost::video;

/// The method's code up to its first branch on the frame (0x100: any byte).
const SIG: &[u16] = &[
    0x53, 0x8b, 0xdc, 0x83, 0xec, 0x08, 0x83, 0xe4, 0xf8, 0x83, 0xc4, 0x04, 0x55, 0x8b, 0x6b, 0x04, //
    0x89, 0x6c, 0x24, 0x04, 0x8b, 0xec, 0x6a, 0xff, 0x68, 0x100, 0x100, 0x100, 0x100, // push handler
    0x64, 0xa1, 0, 0, 0, 0, 0x50, 0x53, 0x83, 0xec, 0x68, 0xa1, 0x100, 0x100, 0x100, 0x100, // cookie
    0x33, 0xc5, 0x89, 0x45, 0xec, 0x56, 0x57, 0x50, 0x8d, 0x45, 0xf4, 0x64, 0xa3, 0, 0, 0, 0, //
    0x8b, 0xf9, 0x80, 0x7b, 0x18, 0x00, 0x8b, 0x4b, 0x08, 0x8b, 0x43, 0x0c, 0x89, 0x4d, 0x9c, //
    0x89, 0x45, 0x8c, 0x75, 0x0c, 0x3b, 0x8f, 0x24, 0x01, 0x00, 0x00, 0x0f, 0x84,
];
/// The first instructions (moved into the trampoline whole).
const PROLOGUE: usize = 16;

/// Where the original method goes on (its first instructions, then a jump back).
static TRAMPOLINE: Mutex<u32> = Mutex::new(0);

/// Per player: a key of its movie's header, and the stream (None: the game's own decoder).
static PLAYERS: Mutex<Option<HashMap<u32, (u32, Option<u32>)>>> = Mutex::new(None);

/// Patches the movie player of a SiglusEngine exe at `base` (`size` bytes), if it has one.
pub fn patch(base: u32, size: u32) {
    if std::env::var_os("SAKE_NO_MOVIE").is_some() {
        return;
    }
    let img = unsafe { std::slice::from_raw_parts(base as usize as *const u8, size as usize) };
    let at = img.windows(SIG.len()).position(|w| w.iter().zip(SIG).all(|(&b, &s)| s == 0x100 || b as u16 == s));
    let Some(off) = at else { return };
    let f = base + off as u32;
    let mut tr = img[off..off + PROLOGUE].to_vec();
    let back = f + PROLOGUE as u32;
    tr.push(0xe9);
    let t = api::stub(&[0; PROLOGUE + 5]);
    tr.extend((back.wrapping_sub(t + PROLOGUE as u32 + 5)).to_le_bytes());
    crate::mem::write(t, &tr);
    *TRAMPOLINE.lock().unwrap() = t;
    let h = api::host_fn("siglus_movie_frame", frame, 0x18);
    let mut jmp = vec![0xe9];
    jmp.extend(h.wrapping_sub(f + 5).to_le_bytes());
    crate::mem::write(f, &jmp);
    eprintln!("sake: Siglus movies through WebCodecs (player at {f:#x})");
}

/// Where a player keeps its copy of the file's header (0xa8 bytes: sizes, frame and page counts).
const HEADER: u32 = 0x18;
const HEADER_LEN: usize = 0xa8;

/// The movie file a player reads: the open `.omv` whose header is the player's copy (the player
/// keeps the handle elsewhere). Returns the handle and the path.
fn movie_of(this: u32) -> Option<(u32, String)> {
    let copy = unsafe { std::slice::from_raw_parts((this + HEADER) as usize as *const u8, HEADER_LEN) };
    let open: Vec<(u32, String)> = crate::obj::with(|t| {
        t.objs
            .iter()
            .filter_map(|(&h, (o, _))| match o {
                crate::obj::Obj::File(f) if f.path.to_ascii_lowercase().ends_with(".omv") => Some((h, f.path.clone())),
                _ => None,
            })
            .collect()
    });
    open.into_iter().find(|(_, p)| {
        use std::io::Read;
        let mut head = [0u8; HEADER_LEN];
        vnhost::fs::File::open(p).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head[..] == copy[..]
    })
}

fn frame(c: &mut Ctx) -> u32 {
    let this = c.cpu.regs[vnx86::ECX];
    let (n, dst, pitch, force) = (c.a(0) as i32, c.a(1), c.a(2), c.a(4) & 0xff);
    let stream = {
        let mut g = PLAYERS.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        // (a player may open another movie: its header copy changes)
        let key = (0..HEADER_LEN as u32 / 4).fold(0x811c_9dc5u32, |k, i| (k ^ r32(this + HEADER + 4 * i)).wrapping_mul(0x0100_0193));
        let known = m.get(&this).copied().filter(|&(k, _)| k == key);
        match known {
            Some((_, s)) => s,
            None => {
                if let Some((_, Some(old))) = m.remove(&this) {
                    video::close(old);
                }
                let found = movie_of(this);
                let s = found.as_ref().filter(|_| video::available()).and_then(|(_, p)| {
                    let data = vnhost::fs::read(format!("{p}.vnv")).ok()?;
                    video::open(data)
                });
                match &found {
                    Some((_, p)) => eprintln!("sake: movie {p}: {}", if s.is_some() { "WebCodecs" } else { "the game's decoder" }),
                    None => eprintln!("sake: movie player {this:#x}: its .omv not found (the game's decoder)"),
                }
                m.insert(this, (key, s));
                s
            }
        }
    };
    let Some(id) = stream else {
        // the game's own decoder
        c.cpu.eip = *TRAMPOLINE.lock().unwrap();
        c.jumped = true;
        return 0;
    };
    if force == 0 && n == r32(this + 0x124) as i32 {
        return 1;
    }
    let (mode, w, h) = (r32(this + 0xc4), r32(this + 0xc8), r32(this + 0xcc));
    let out = if mode == 2 { video::Out::Rgb } else { video::Out::Ycc };
    unsafe { video::frame(id, n, dst as usize as *mut u8, pitch as usize, w, h, out) };
    w32(this + 0x124, n as u32);
    w32(this + 0x120, n as u32);
    1
}
