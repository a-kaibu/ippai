//! Siglus's movies (`.omv`, Theora) decoded by the browser (WebCodecs, in hardware) instead of
//! by the game's own x86 Theora decoder, which took most of a phone's core in effect-heavy
//! scenes (aurora, smoke: 1920x1080 with alpha at 30 fps).
//!
//! SiglusEngine's movie player has one method that brings a frame into a 32-bit buffer:
//! `bool __thiscall get_frame(frame, dst, pitch, ?, force, ?)` (decoding every frame up to the
//! one asked for). Found by its code at load, it is patched to jump to `frame` here, which hands
//! out the frame `masu pack` made into VP9 (`<movie>.omv.vnv`, crates/masu/src/movie.rs) and
//! the page decodes (crates/kura/src/video.rs). Movies without that file, or no page to decode
//! (node), keep the game's decoder (the original code, through a trampoline).
//!
//! The player: +0xc4 how pixels go out (0: Y Cb Cr 255, 1: Y Cb Cr alpha, the game converting on
//! the GPU; 2: RGB), +0xc8 width, +0xcc height, +0x120/+0x124 the frame decoded last.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::api::{self, Ctx};
use crate::mem::{r32, w32};
use kura::video;

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

/// A player's movie: a key of its header, the stream (None: the game's own decoder), the `.omv`
/// handle and path it was found by, and when the player last asked for a frame.
#[derive(Clone, Debug)]
struct Player {
    key: u32,
    stream: Option<u32>,
    file: Option<(u32, String)>,
    used: Instant,
}
static PLAYERS: Mutex<Option<HashMap<u32, Player>>> = Mutex::new(None);

/// How long a player may ask for no frame before its stream goes (a paused movie opens it again
/// when it goes on: the page's decoder is what is scarce, on a phone a few hardware ones).
const IDLE: Duration = Duration::from_secs(30);

/// The players whose streams go now: a movie whose `.omv` the game closed (the handle not open,
/// or open on another file), or one no frame was asked of for IDLE. Nothing tells sake when
/// the game deletes a player: without this, every movie object at a new address kept its
/// WebCodecs stream for the rest of the session.
fn stale(m: &HashMap<u32, Player>, now: Instant, me: u32, open: impl Fn(u32, &str) -> bool) -> Vec<u32> {
    m.iter()
        .filter(|&(&this, p)| this != me && p.stream.is_some())
        .filter(|(_, p)| now.saturating_duration_since(p.used) > IDLE || p.file.as_ref().is_some_and(|(h, path)| !open(*h, path)))
        .map(|(&this, _)| this)
        .collect()
}
/// Closes the streams of the stale players (see `stale`) other than `me`, at most every second.
fn sweep(m: &mut HashMap<u32, Player>, me: u32, now: Instant) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(1)) {
            return;
        }
        *l = Some(now);
    }
    let open = |h: u32, path: &str| crate::obj::file(h).is_some_and(|f| f.path == path);
    for this in stale(m, now, me, open) {
        if let Some(Player { stream: Some(s), file, .. }) = m.remove(&this) {
            eprintln!("sake: movie {}: done, its stream closed", file.map_or_else(String::new, |f| f.1));
            video::close(s);
        }
    }
}

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
        kura::fs::File::open(p).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head[..] == copy[..]
    })
}

fn frame(c: &mut Ctx) -> u32 {
    let this = c.cpu.regs[koji::ECX];
    let (n, dst, pitch, force) = (c.a(0) as i32, c.a(1), c.a(2), c.a(4) & 0xff);
    let stream = {
        let mut g = PLAYERS.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        let now = Instant::now();
        sweep(m, this, now);
        // (a player may open another movie: its header copy changes)
        let key = (0..HEADER_LEN as u32 / 4).fold(0x811c_9dc5u32, |k, i| (k ^ r32(this + HEADER + 4 * i)).wrapping_mul(0x0100_0193));
        let known = m.get_mut(&this).filter(|p| p.key == key);
        match known {
            Some(p) => {
                p.used = now;
                p.stream
            }
            None => {
                if let Some(Player { stream: Some(old), .. }) = m.remove(&this) {
                    video::close(old);
                }
                let found = movie_of(this);
                let s = found.as_ref().filter(|_| video::available()).and_then(|(_, p)| {
                    video::open_file(format!("{p}.vnv")).map(|(s, _)| s)
                });
                match &found {
                    Some((_, p)) => eprintln!("sake: movie {p}: {}", if s.is_some() { "WebCodecs" } else { "the game's decoder" }),
                    None => eprintln!("sake: movie player {this:#x}: its .omv not found (the game's decoder)"),
                }
                m.insert(this, Player { key, stream: s, file: found, used: now });
                s
            }
        }
    };
    // a stream the page could not decode goes back to the game's own decoder (else the movie
    // is never drawn: ANEMOI's aurora on a phone that took its hardware decoder back)
    let stream = stream.filter(|&id| {
        if !video::failed(id) {
            return true;
        }
        eprintln!("sake: movie player {this:#x}: WebCodecs failed, the game's decoder from here on");
        video::close(id);
        if let Some(e) = PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.get_mut(&this)) {
            e.stream = None;
        }
        false
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn player(stream: Option<u32>, file: Option<(u32, &str)>, used: Instant) -> Player {
        Player { key: 0, stream, file: file.map(|(h, p)| (h, p.to_string())), used }
    }

    #[test]
    fn a_closed_movie_or_an_idle_one_goes() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(40);
        let mut m = HashMap::new();
        m.insert(1, player(Some(11), Some((0x10, "a.omv")), now)); // playing
        m.insert(2, player(Some(12), Some((0x14, "b.omv")), now)); // its file closed
        m.insert(3, player(Some(13), Some((0x18, "c.omv")), t0)); // no frame for 40 s
        m.insert(4, player(None, Some((0x1c, "d.omv")), t0)); // the game's decoder: nothing to close
        m.insert(5, player(Some(15), Some((0x20, "e.omv")), now)); // its handle now another file's
        let open = |h: u32, p: &str| matches!((h, p), (0x10, "a.omv") | (0x18, "c.omv") | (0x1c, "d.omv") | (0x20, "other.omv"));
        let mut v = stale(&m, now, 1, open);
        v.sort();
        assert_eq!(v, [2, 3, 5]);
        // the player asking now never loses its own, idle or not
        assert!(!stale(&m, now, 3, open).contains(&3));
    }

    proptest! {
        /// Against the rule: a player other than the asking one goes exactly when it has a
        /// stream and is idle past IDLE or its file is no longer the open one.
        #[test]
        fn stale_is_the_rule(ps in prop::collection::vec((any::<bool>(), 0u64..60, any::<bool>(), any::<bool>()), 0..12), me in 0u32..12) {
            let t0 = Instant::now();
            let now = t0 + Duration::from_secs(60);
            let mut m = HashMap::new();
            for (i, &(has, idle, has_file, open)) in ps.iter().enumerate() {
                let file = has_file.then(|| (i as u32 * 4, if open { "open.omv" } else { "closed.omv" }));
                m.insert(i as u32, player(has.then_some(100 + i as u32), file, now - Duration::from_secs(idle)));
            }
            let got: std::collections::HashSet<u32> = stale(&m, now, me, |_, p| p == "open.omv").into_iter().collect();
            for (i, &(has, idle, has_file, open)) in ps.iter().enumerate() {
                let want = i as u32 != me && has && (Duration::from_secs(idle) > IDLE || (has_file && !open));
                prop_assert_eq!(got.contains(&(i as u32)), want);
            }
        }
    }
}
