//! Playback for DirectShow's filter graph (dshow.rs): a movie `ippai pack` made a `.vnv` of
//! (crates/ippai/src/movie.rs: VP9 frames the page decodes with WebCodecs, or uncompressed
//! ones in tests, and its sound as Ogg Vorbis) plays on one host thread, which draws its frames
//! into a display window of its own over the game's (where the video window or the EVR's
//! rectangle is) and queues its sound to kura's audio output.
//!
//! The parts kept free of the guest (the clock, the picture's placement, the sound's decoding)
//! are tested here; the thread and the COM side by host/tests/dshow.c and winref's dshow probe.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kura::video;

/// 100 ns units a second (REFERENCE_TIME)
pub const SEC: u64 = 10_000_000;

/// Now on the clock movies play by, in 100 ns units.
pub fn now() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_nanos() as u64 / 100
}

// ---------------------------------------------------------------------------------------------
// the clock

/// Where a movie is: its position runs with the clock while it runs (at `rate`), up to `stop`.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub duration: u64,
    pub stop: u64,
    pub rate: f64,
    /// the position at `since`, or while not running
    pos: u64,
    since: Option<u64>,
    /// EC_COMPLETE went out (until a seek or a stop)
    pub done: bool,
}

impl Timeline {
    pub fn new(duration: u64) -> Timeline {
        Timeline { duration, stop: duration, rate: 1.0, pos: 0, since: None, done: false }
    }
    pub fn running(&self) -> bool {
        self.since.is_some()
    }
    pub fn position(&self, now: u64) -> u64 {
        match self.since {
            // (a position past the stop stays where it was set: seeking there)
            Some(t) => {
                let p = self.pos.saturating_add((now.saturating_sub(t) as f64 * self.rate) as u64);
                if self.pos >= self.stop { self.pos } else { p.min(self.stop) }
            }
            None => self.pos,
        }
    }
    /// Its position now becomes the one it runs on from.
    fn mark(&mut self, now: u64) {
        self.pos = self.position(now);
        if self.since.is_some() {
            self.since = Some(now);
        }
    }
    pub fn run(&mut self, now: u64) {
        if self.since.is_none() {
            self.since = Some(now);
        }
    }
    pub fn pause(&mut self, now: u64) {
        self.mark(now);
        self.since = None;
    }
    /// Stopped: where it was stays (DirectShow does not rewind), the end may come again.
    pub fn stop(&mut self, now: u64) {
        self.pause(now);
        self.done = false;
    }
    pub fn seek(&mut self, now: u64, to: u64) {
        self.mark(now);
        self.pos = to.min(self.duration);
        self.done = false;
    }
    pub fn set_stop(&mut self, now: u64, to: u64) {
        self.mark(now);
        self.stop = to.min(self.duration);
        if self.pos < self.stop {
            self.done = false;
        }
    }
    pub fn set_rate(&mut self, now: u64, rate: f64) {
        self.mark(now);
        self.rate = rate;
    }
    /// Whether the end was reached just now (once; then `done`).
    pub fn complete(&mut self, now: u64) -> bool {
        if self.done || !self.running() || self.position(now) < self.stop {
            return false;
        }
        self.done = true;
        true
    }
    /// Clock time to the end (None: not running, or past it).
    pub fn until_end(&self, now: u64) -> Option<u64> {
        if !self.running() || self.done || self.rate <= 0.0 {
            return None;
        }
        let left = self.stop.saturating_sub(self.position(now));
        Some((left as f64 / self.rate) as u64)
    }
}

/// The frame shown at `pos` of a movie of `frames` frames at `rate` (frames per second as a
/// fraction): the one whose time has begun, the last one past the end.
pub fn frame_at(pos: u64, rate: (u32, u32), frames: u32) -> u32 {
    if rate.0 == 0 || rate.1 == 0 {
        return 0;
    }
    let n = (pos as u128 * rate.0 as u128 / (rate.1 as u128 * SEC as u128)) as u64;
    n.min(frames.saturating_sub(1) as u64) as u32
}

// ---------------------------------------------------------------------------------------------
// the picture

/// A rectangle: left, top, width, height.
pub type R = (i32, i32, i32, i32);

/// The part of `dst` a `src_w` x `src_h` picture fills keeping its shape (centred).
pub fn letterbox(dst: R, src_w: i32, src_h: i32) -> R {
    if src_w <= 0 || src_h <= 0 || dst.2 <= 0 || dst.3 <= 0 {
        return (dst.0, dst.1, 0, 0);
    }
    let (w, h) = if dst.2 as i64 * src_h as i64 > dst.3 as i64 * src_w as i64 {
        ((dst.3 as i64 * src_w as i64 / src_h as i64) as i32, dst.3)
    } else {
        (dst.2, (dst.2 as i64 * src_h as i64 / src_w as i64) as i32)
    };
    (dst.0 + (dst.2 - w) / 2, dst.1 + (dst.3 - h) / 2, w, h)
}

/// Draws part `src` of a picture (`fw` x `fh`) stretched onto `dst` of a window, nearest
/// pixel, black around it: into `out`, the window's part `view` (rows of `view.2` pixels).
pub fn blit(frame: &[u32], fw: i32, fh: i32, src: R, out: &mut [u32], view: R, dst: R) {
    let src = clip(src, (0, 0, fw, fh));
    let on = if src.2 <= 0 || src.3 <= 0 || dst.2 <= 0 || dst.3 <= 0 { (0, 0, 0, 0) } else { clip(dst, view) };
    // (black around the picture only: a picture over all of the view leaves none)
    if on != view {
        out.fill(0xff00_0000);
    }
    if on.2 <= 0 || on.3 <= 0 {
        return;
    }
    // the source column of each column, the same on every row
    let xs: Vec<usize> = (on.0..on.0 + on.2).map(|x| (src.0 + ((x - dst.0) as i64 * src.2 as i64 / dst.2 as i64) as i32) as usize).collect();
    for y in on.1..on.1 + on.3 {
        let sy = src.1 + ((y - dst.1) as i64 * src.3 as i64 / dst.3 as i64) as i32;
        let row = &frame[(sy * fw) as usize..];
        let o = ((y - view.1) * view.2 + (on.0 - view.0)) as usize;
        for (d, &sx) in out[o..o + xs.len()].iter_mut().zip(&xs) {
            *d = row[sx] | 0xff00_0000;
        }
    }
}

/// `a` within `b`.
pub fn clip(a: R, b: R) -> R {
    let (l, t) = (a.0.max(b.0), a.1.max(b.1));
    let (r, bt) = ((a.0 + a.2).min(b.0 + b.2), (a.1 + a.3).min(b.1 + b.3));
    (l, t, (r - l).max(0), (bt - t).max(0))
}

// ---------------------------------------------------------------------------------------------
// the sound

/// Reads bytes of a movie's sound at an offset into a buffer (false: could not).
pub type ReadAt = Box<dyn FnMut(u64, &mut [u8]) -> bool + Send>;

/// Bytes of the sound read at once.
const SOUND_READ: usize = 64 << 10;

/// A movie's sound: a `.vnv`'s Ogg Vorbis, or an ASF file's (asfmovie.rs).
pub enum Sound {
    Vorbis(Vorbis),
    Asf(crate::asfmovie::Audio),
    /// an MP4's (mf/movie.rs)
    Mf(crate::mf::movie::Audio),
}

impl Sound {
    /// Frames a second.
    pub fn rate(&self) -> u32 {
        match self {
            Sound::Vorbis(v) => v.rate,
            Sound::Asf(a) => a.rate,
            Sound::Mf(a) => a.rate,
        }
    }
    /// Appends `n` stereo frames from frame `from` to `out` (silence past the end).
    pub fn take(&mut self, from: u64, n: usize, out: &mut Vec<f32>) {
        match self {
            Sound::Vorbis(v) => v.take(from, n, out),
            Sound::Asf(a) => a.take(from, n, out),
            Sound::Mf(a) => a.take(from, n, out),
        }
    }
}

/// A movie's sound (Ogg Vorbis), read and decoded as it is wanted: stereo frames from any
/// position (a position before what is kept decodes again from the start).
pub struct Vorbis {
    read: ReadAt,
    len: u64,
    /// bytes read at once (SOUND_READ; small in tests)
    step: usize,
    /// the next byte to read, and the packets of what was read
    pos: u64,
    ogg: vorbis::ogg::Reader,
    /// the identification and setup headers
    heads: [Vec<u8>; 2],
    dec: Option<vorbis::Decoder>,
    channels: usize,
    pub rate: u32,
    /// decoded stereo frames, the first of them frame `base`
    pcm: VecDeque<[f32; 2]>,
    base: u64,
    /// where decoding was every `every` frames (frame, byte, the Ogg reader, the decoder's
    /// carry): a seek back starts from the last one before it, not from the start of the
    /// stream (a late seek decoded minutes of sound again, under the movies' lock)
    marks: Vec<(u64, u64, vorbis::ogg::Reader, vorbis::Carry)>,
    every: u64,
    /// how many times decoding started again from the start (tests)
    pub restarts: u32,
}

impl Vorbis {
    /// The sound in memory (tests).
    #[cfg(test)]
    pub fn new(ogg: &[u8]) -> Option<Vorbis> {
        let d = ogg.to_vec();
        let len = d.len() as u64;
        Vorbis::read(
            len,
            Box::new(move |o, b| match d.get(o as usize..o as usize + b.len()) {
                Some(s) => {
                    b.copy_from_slice(s);
                    true
                }
                None => false,
            }),
        )
    }
    /// The `len` bytes of sound `read` reads.
    pub fn read(len: u64, read: ReadAt) -> Option<Vorbis> {
        let mut s = Vorbis { read, len, step: SOUND_READ, pos: 0, ogg: Default::default(), heads: Default::default(), dec: None, channels: 0, rate: 0, pcm: VecDeque::new(), base: 0, marks: Vec::new(), every: 0, restarts: 0 };
        let id = s.packet()?;
        let _comment = s.packet()?;
        let setup = s.packet()?;
        let i = vorbis::Ident::read(&id).ok()?;
        (s.channels, s.rate) = (i.channels, i.rate);
        s.every = 30 * i.rate as u64;
        s.heads = [id, setup];
        s.restart();
        s.restarts = 0;
        s.dec.as_ref()?;
        Some(s)
    }
    /// The next packet (None: the end, or the sound could not be read).
    fn packet(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(p) = self.ogg.pop() {
                return Some(p);
            }
            if self.pos >= self.len {
                return None;
            }
            let n = (self.step as u64).min(self.len - self.pos) as usize;
            let mut b = vec![0; n];
            if !(self.read)(self.pos, &mut b) {
                self.pos = self.len;
                return None;
            }
            self.pos += n as u64;
            self.ogg.push(&b);
            if self.pos >= self.len {
                self.ogg.finish();
            }
        }
    }
    fn restart(&mut self) {
        self.restarts += 1;
        self.dec = vorbis::Decoder::new(&self.heads[0], &self.heads[1]).ok();
        self.pos = 0;
        self.ogg = Default::default();
        for _ in 0..3 {
            self.packet();
        }
        self.pcm.clear();
        self.base = 0;
    }
    /// Appends `n` stereo frames from frame `from` to `out` (silence past the end).
    pub fn take(&mut self, from: u64, n: usize, out: &mut Vec<f32>) {
        if from < self.base {
            // (from the last mark at or before it: decoding goes on from there as it did)
            let mark = self.marks.iter().rposition(|m| m.0 <= from);
            match (mark, self.dec.as_mut()) {
                (Some(i), Some(dec)) => {
                    let (frame, pos, ogg, carry) = &self.marks[i];
                    dec.resume(carry);
                    (self.base, self.pos, self.ogg) = (*frame, *pos, ogg.clone());
                    self.pcm.clear();
                }
                _ => self.restart(),
            }
        }
        // what is no longer wanted (a second's worth kept behind for small steps back)
        let keep = from.saturating_sub(self.rate as u64);
        if keep > self.base {
            let k = ((keep - self.base) as usize).min(self.pcm.len());
            self.pcm.drain(..k);
            self.base += k as u64;
        }
        let mut buf = Vec::new();
        while self.base + (self.pcm.len() as u64) < from + n as u64 {
            if self.dec.is_none() {
                break;
            }
            let Some(p) = self.packet() else { break };
            buf.clear();
            self.dec.as_mut().unwrap().packet(&p, &mut buf);
            let ch = self.channels.max(1);
            self.pcm.extend(buf.chunks_exact(ch).map(|f| if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] }));
            let end = self.base + self.pcm.len() as u64;
            if self.every > 0 && end >= self.marks.last().map_or(self.every, |m| m.0 + self.every) {
                self.marks.push((end, self.pos, self.ogg.clone(), self.dec.as_ref().unwrap().carry()));
            }
            // (far behind what is wanted: not kept)
            let keep = from.saturating_sub(self.rate as u64);
            if keep > self.base + self.pcm.len() as u64 {
                self.base += self.pcm.len() as u64;
                self.pcm.clear();
            }
        }
        for i in 0..n as u64 {
            let f = (from + i).checked_sub(self.base).and_then(|k| self.pcm.get(k as usize)).copied().unwrap_or([0.0; 2]);
            out.extend(f);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// playing

/// A movie's file read (the `.vnv`, or an ASF file decoded here).
pub struct Movie {
    /// its frames: kura's stream, or asfmovie's (`frame`)
    pub stream: u32,
    pub w: u32,
    pub h: u32,
    pub frames: u32,
    pub rate: (u32, u32),
    pub duration: u64,
    pub sound: Option<Sound>,
}

impl Movie {
    /// The movie in the `.vnv` file at `path` (with the trailer), read as it plays; None for
    /// anything else.
    pub fn open(path: &str) -> Option<Movie> {
        let (stream, i) = video::open_file(path)?;
        // (a rate with a zero in it is no movie: frame_at would divide by it)
        let Some(rate) = i.rate.filter(|r| r.0 != 0 && r.1 != 0) else {
            video::close(stream);
            return None;
        };
        let sound = match i.audio {
            Some((o, l)) => {
                let Ok(mut f) = kura::fs::File::open(path) else {
                    video::close(stream);
                    return None;
                };
                Vorbis::read(l as u64, Box::new(move |at, b| video::read_at(&mut f, o as u64 + at, b))).map(Sound::Vorbis)
            }
            None => None,
        };
        Some(Movie { stream, w: i.w, h: i.h, frames: i.frames(), rate, duration: i.duration, sound })
    }
}

impl Movie {
    /// The movie of the game's file `name`: the `.vnv` next to it that the page can decode,
    /// else the file itself if it is an ASF one sake decodes (asfmovie.rs).
    pub fn find(name: &str) -> Option<Movie> {
        let vnv = crate::files::resolve(&format!("{name}.vnv")).and_then(|p| Movie::open(&p)).filter(|m| video::decodable(m.stream));
        vnv.or_else(|| {
            let a = crate::asfmovie::open(&crate::files::resolve(name)?)?;
            Some(Movie { stream: a.id, w: a.w, h: a.h, frames: a.frames, rate: a.rate, duration: a.duration, sound: a.audio.map(Sound::Asf) })
        })
        // (an MP4: through Media Foundation's source, mf/movie.rs)
        .or_else(|| crate::mf::movie::open_path(&crate::files::resolve(name)?))
    }
}

impl Drop for Movie {
    fn drop(&mut self) {
        if crate::asfmovie::is(self.stream) {
            crate::asfmovie::close(self.stream)
        } else if crate::mf::movie::is(self.stream) {
            crate::mf::movie::close(self.stream)
        } else {
            video::close(self.stream)
        }
    }
}

/// Frame `n` of a movie's frames `stream` (Movie::stream), as kura::video::frame's Rgb601.
///
/// # Safety
/// As kura::video::frame: `dst` holds `h` rows of `pitch` bytes, each `4 * w` long.
pub unsafe fn frame(stream: u32, n: i32, dst: *mut u8, pitch: usize, w: u32, h: u32) -> bool {
    // SAFETY: the caller's
    unsafe {
        if crate::asfmovie::is(stream) {
            crate::asfmovie::frame(stream, n, dst, pitch, w, h)
        } else if crate::mf::movie::is(stream) {
            crate::mf::movie::frame(stream, n, dst, pitch, w, h)
        } else {
            video::frame(stream, n, dst, pitch, w, h, video::Out::Rgb601)
        }
    }
}

/// Whether a movie's frames can no longer come (kura::video::failed).
pub fn failed(stream: u32) -> bool {
    if crate::mf::movie::is(stream) { crate::mf::movie::failed(stream) } else { !crate::asfmovie::is(stream) && video::failed(stream) }
}

/// Where a graph's picture goes.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Place {
    /// the window the rectangles are in (0: the screen)
    pub hwnd: u32,
    /// the video's part of it (client coordinates), and of the video shown
    pub dst: R,
    pub src: R,
    /// keep the video's shape inside `dst` (the EVR's default)
    pub letterbox: bool,
    pub visible: bool,
}

/// A graph playing a movie.
pub struct Play {
    pub movie: Movie,
    pub tl: Timeline,
    pub place: Place,
    /// IBasicAudio's volume (1/100 dB, -10000..0)
    pub volume: i32,
    /// the display window it draws in (made on its first frame), and what it shows
    disp: u32,
    shown: Option<(u32, [i32; 4])>,
    restacked: Option<Instant>,
    /// kura's audio stream and the next frame of sound queued to it
    audio: Option<(i32, u64)>,
}

impl Play {
    pub fn new(movie: Movie) -> Play {
        let tl = Timeline::new(movie.duration);
        let src = (0, 0, movie.w as i32, movie.h as i32);
        Play { movie, tl, place: Place { src, ..Place::default() }, volume: 0, disp: 0, shown: None, restacked: None, audio: None }
    }
}

impl Drop for Play {
    fn drop(&mut self) {
        if self.disp != 0 {
            kura::display::vnhost_window_destroy(self.disp);
        }
        if let Some((a, _)) = self.audio {
            kura::audio::close(a);
        }
    }
}

/// The graphs playing, by graph block.
static PLAYS: Mutex<Option<HashMap<u32, Play>>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();

/// The plays to change: the movie thread takes a turn for what changed.
pub fn with<R>(f: impl FnOnce(&mut HashMap<u32, Play>) -> R) -> R {
    let mut g = PLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let r = f(g.get_or_insert_with(HashMap::new));
    WAKE.notify_all();
    r
}
/// The plays to look at (GetCurrentPosition, the picture's size, ...): the movie thread is not
/// woken (a game polling the position every frame forced a whole turn per poll)
pub fn read<R>(f: impl FnOnce(&HashMap<u32, Play>) -> R) -> R {
    let mut g = PLAYS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}
/// `f` on the timeline of `key`'s play: the movie thread is woken only when it changed
/// (a seek, a run or a stop; not a position query)
pub fn timeline<R>(key: u32, t: u64, f: impl FnOnce(&mut Timeline, u64) -> R) -> Option<R> {
    let mut g = PLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let p = g.get_or_insert_with(HashMap::new).get_mut(&key)?;
    let before = p.tl.clone();
    let r = f(&mut p.tl, t);
    if p.tl != before {
        WAKE.notify_all();
    }
    Some(r)
}

/// Starts playback of graph `key` (replacing a movie it had).
pub fn start(key: u32, p: Play) {
    with(|m| m.insert(key, p));
    thread();
}

pub fn end(key: u32) {
    let p = with(|m| m.remove(&key));
    drop(p);
}

/// Frames of sound kept queued ahead.
const AHEAD: u32 = 4410;

static THREAD_AT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A state is being taken: the movie thread comes out of its wait (and stands still).
pub(crate) fn snap_wake() {
    WAKE.notify_all();
}

/// A restored save state: the movie thread again, if it ran.
pub(crate) fn snap_restore() {
    if THREAD_AT.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        thread();
    }
}

fn thread() {
    // (once per save-state epoch: a restored copy starts it again)
    if kura::snap::once(&THREAD_AT) {
        std::thread::Builder::new()
            .name("movies".into())
            .spawn(|| {
                kura::snap::register("movies", false, 0);
                let mut g = PLAYS.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    drop(g);
                    kura::snap::busy();
                    let wait = turn();
                    kura::snap::idle();
                    g = PLAYS.lock().unwrap_or_else(|e| e.into_inner());
                    g = WAKE.wait_timeout(g, wait).unwrap_or_else(|e| e.into_inner()).0;
                }
            })
            .expect("movie thread");
    }
}

/// One turn of the movie thread: ends, frames, sound. Returns how long it may sleep.
fn turn() -> Duration {
    let t = now();
    // (nothing running or shown: the thread sleeps until a change wakes it; it woke 50 times a
    // second for the rest of the session after the first movie)
    let mut wait = Duration::from_secs(3600);
    let mut ended = Vec::new();
    // sound to decode, outside the lock (reading and decoding the .vnv's Vorbis can take long:
    // every other thread's movie call waited for it)
    let mut sounds = Vec::new();
    // (what to draw, decoded outside the lock: frame waits on the page)
    let mut draws = Vec::new();
    let keys: Vec<(u32, u32)> = with(|m| m.iter().map(|(&k, p)| (k, p.place.hwnd)).collect());
    // the windows' places on the screen (user32's lock, not under ours)
    let screens: HashMap<u32, Option<crate::user::Rect>> = keys.iter().map(|&(_, h)| (h, if h == 0 { None } else { crate::user::client_rect_screen(h) })).collect();
    let screen = crate::user::screen();
    with(|m| {
        for (&k, p) in m.iter_mut() {
            if p.tl.complete(t) {
                ended.push(k);
            }
            if let Some(left) = p.tl.until_end(t) {
                wait = wait.min(Duration::from_nanos(left * 100));
            }
            let n = frame_at(p.tl.position(t), p.movie.rate, p.movie.frames);
            if p.tl.running() {
                // (sound is queued ahead every 20 ms at most)
                wait = wait.min(Duration::from_millis(20));
                // to the next frame
                if p.movie.rate.0 != 0 {
                    let next = (n as u128 + 1) * p.movie.rate.1 as u128 * SEC as u128 / p.movie.rate.0 as u128;
                    let to = (next as u64).saturating_sub(p.tl.position(t));
                    wait = wait.min(Duration::from_nanos((to as f64 / p.tl.rate.max(0.01) * 100.0) as u64).max(Duration::from_millis(2)));
                }
            }
            if p.shown.is_some() {
                // (shown: raised over the game's windows again every 500 ms)
                wait = wait.min(Duration::from_millis(500));
            }
            if let Some(job) = sound_job(p, t) {
                sounds.push((k, job));
            }
            // the window's part of the screen, clipped to it
            // (no window: the screen; a rectangle the game set past it is clipped to it, not a
            // buffer of its size)
            let client = match screens.get(&p.place.hwnd).copied().flatten() {
                Some(r) => (r.l, r.t, r.r - r.l, r.b - r.t),
                None if p.place.hwnd == 0 => (0, 0, screen.0, screen.1),
                None => (0, 0, 0, 0),
            };
            let dst = if p.place.letterbox { letterbox(p.place.dst, p.place.src.2, p.place.src.3) } else { p.place.dst };
            let on = clip((client.0 + dst.0, client.1 + dst.1, dst.2, dst.3), client);
            let rect = [on.0, on.1, on.0 + on.2, on.1 + on.3];
            if !p.place.visible || on.2 <= 0 || on.3 <= 0 {
                if p.disp != 0 && p.shown.is_some() {
                    let r8 = [0i32; 8];
                    unsafe { kura::display::vnhost_window_pos(p.disp, r8.as_ptr(), 0, 0, 0) };
                    p.shown = None;
                }
                continue;
            }
            if p.shown == Some((n, rect)) && p.restacked.is_some_and(|r| r.elapsed() < Duration::from_millis(500)) {
                continue;
            }
            if p.disp == 0 {
                static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                p.disp = 0x7ffe_0000 + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // (on top of the game's windows, again now and then: the game may raise its own)
            let restack = p.shown.map(|s| s.1) != Some(rect) || p.restacked.is_none_or(|r| r.elapsed() >= Duration::from_millis(500));
            if restack {
                let r8 = [rect[0], rect[1], rect[2], rect[3], rect[0], rect[1], rect[2], rect[3]];
                unsafe { kura::display::vnhost_window_pos(p.disp, r8.as_ptr(), 1, 1, 0) };
                p.restacked = Some(Instant::now());
            }
            if p.shown.map(|s| s.0) != Some(n) || p.shown.map(|s| s.1) != Some(rect) {
                // the window's part in the picture's coordinates: dst moved by the clip
                let view = (on.0 - client.0, on.1 - client.1, on.2, on.3);
                draws.push((k, p.movie.stream, n, p.movie.w, p.movie.h, p.place.src, dst, view, p.disp));
            }
            p.shown = Some((n, rect));
        }
    });
    for (k, job) in sounds {
        job.run(k);
    }
    for (k, stream, n, w, h, src, dst, view, disp) in draws {
        let t0 = Instant::now();
        // (the buffers stay for the next frame: no allocation per frame)
        let (mut px, mut out) = BUFS.with(|b| b.borrow_mut().remove(&k)).unwrap_or_default();
        out.resize((view.2 * view.3) as usize, 0);
        // the picture as it is, all of it: straight into what the display gets
        let whole = src == (0, 0, w as i32, h as i32) && dst == view && (dst.2, dst.3) == (w as i32, h as i32);
        let (buf, pitch) = if whole { (&mut out, view.2 as usize) } else { (&mut px, w as usize) };
        buf.resize(pitch * h as usize, 0);
        let ok = unsafe { frame(stream, n as i32, buf.as_mut_ptr() as *mut u8, 4 * pitch, w, h) };
        let t1 = Instant::now();
        if !ok {
            // (nothing yet: drawn again next turn)
            with(|m| {
                if let Some(p) = m.get_mut(&k) {
                    p.shown = None;
                }
            });
            BUFS.with(|b| b.borrow_mut().insert(k, (px, out)));
            continue;
        }
        if !whole {
            blit(&px, w as i32, h as i32, src, &mut out, view, dst);
        }
        let t2 = Instant::now();
        let d = [0, 0, view.2, view.3];
        unsafe { kura::display::vnhost_window_draw(disp, d.as_ptr(), out.as_ptr(), view.2, 0, 0) };
        stats(k, n, [t1 - t0, t2 - t1, t2.elapsed()], out.len());
        BUFS.with(|b| b.borrow_mut().insert(k, (px, out)));
    }
    // (the buffers of movies no longer playing go)
    let live: Vec<u32> = with(|m| m.keys().copied().collect());
    BUFS.with(|b| b.borrow_mut().retain(|k, _| live.contains(k)));
    for k in ended {
        // (a Media Engine's movie, or a graph's)
        if !crate::mf::ended(k) && !crate::mf::session::ended(k) && !crate::amstream::ended(k) && !crate::mci::ended(k) {
            crate::dshow::completed(k);
        }
    }
    wait
}

thread_local! {
    /// The movie thread's picture buffers per graph: the frame, and the window's part.
    static BUFS: std::cell::RefCell<HashMap<u32, (Vec<u32>, Vec<u32>)>> = Default::default();
}

/// SAKE_MOVIE_STATS: every 5 s, the frames drawn and skipped (here and by amstream's Update), and the time per frame to have it
/// (WebCodecs' frame: wait and conversion to RGB), to scale it into the window's part and to
/// hand it to the display, and the pixels handed over.
pub(crate) fn stats(k: u32, n: u32, t: [Duration; 3], pixels: usize) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("SAKE_MOVIE_STATS").is_some()) {
        return;
    }
    #[derive(Default)]
    struct S {
        since: Option<Instant>,
        frames: u32,
        skipped: u32,
        last: HashMap<u32, u32>,
        sum: [Duration; 3],
        max: [Duration; 3],
        pixels: usize,
    }
    static S: Mutex<Option<S>> = Mutex::new(None);
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(S::default);
    let since = *s.since.get_or_insert_with(Instant::now);
    s.frames += 1;
    if let Some(&l) = s.last.get(&k) {
        s.skipped += n.saturating_sub(l + 1);
    }
    s.last.insert(k, n);
    for i in 0..3 {
        s.sum[i] += t[i];
        s.max[i] = s.max[i].max(t[i]);
    }
    s.pixels += pixels;
    if since.elapsed() >= Duration::from_secs(5) {
        let f = s.frames.max(1);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        eprintln!(
            "sake: movie {} frames drawn, {} skipped; per frame ms (avg/max): frame {:.2}/{:.2} scale {:.2}/{:.2} display {:.2}/{:.2}; {:.1} Mpixel/s",
            s.frames,
            s.skipped,
            ms(s.sum[0]) / f as f64,
            ms(s.max[0]),
            ms(s.sum[1]) / f as f64,
            ms(s.max[1]),
            ms(s.sum[2]) / f as f64,
            ms(s.max[2]),
            s.pixels as f64 / since.elapsed().as_secs_f64() / 1e6
        );
        let last = std::mem::take(&mut s.last);
        *s = S { last, ..Default::default() };
    }
}

/// Keeps the sound queued for where the movie is (only while it runs at its own rate).
/// Sound frames of a play to decode and queue: taken out of the play (the decoder goes with the
/// job, outside the lock) and given back by `run`.
struct SoundJob {
    sound: Sound,
    audio: i32,
    next: u64,
    n: usize,
    gain: f32,
}

impl SoundJob {
    /// Decodes (no lock held), then queues what it decoded if the play still plays into the
    /// same audio stream, and gives the sound back.
    fn run(mut self, key: u32) {
        let mut buf = Vec::with_capacity(2 * self.n);
        self.sound.take(self.next, self.n, &mut buf);
        if self.gain != 1.0 {
            buf.iter_mut().for_each(|x| *x *= self.gain);
        }
        with(|m| {
            let Some(p) = m.get_mut(&key) else { return };
            if p.audio.is_some_and(|(a, next)| a == self.audio && next == self.next) {
                kura::audio::write(self.audio, &buf);
                p.audio = Some((self.audio, self.next + self.n as u64));
            }
            if p.movie.sound.is_none() {
                p.movie.sound = Some(self.sound);
            }
        });
    }
}

/// The sound of a play at clock `t`: its audio stream opened, closed or (out of step) opened
/// again, and the frames to queue ahead as a job, if any.
fn sound_job(p: &mut Play, t: u64) -> Option<SoundJob> {
    let s = p.movie.sound.as_mut()?;
    let playing = p.tl.running() && p.tl.rate == 1.0 && !p.tl.done;
    let want = p.tl.position(t) as u128 * s.rate() as u128 / SEC as u128;
    if !playing {
        if let Some((a, _)) = p.audio.take() {
            kura::audio::close(a);
        }
        return None;
    }
    let (a, next) = match p.audio {
        // (out of step by more than 60 ms, after a seek or a pause: queued again from here)
        Some((a, next)) if (next as i128 - kura::audio::queued(a) as i128 - want as i128).abs() <= s.rate() as i128 * 60 / 1000 => (a, next),
        old => {
            if let Some((a, _)) = old {
                kura::audio::close(a);
            }
            (kura::audio::open(), want as u64)
        }
    };
    let q = kura::audio::queued(a);
    p.audio = Some((a, next));
    if q >= AHEAD {
        return None;
    }
    let n = (AHEAD - q) as usize + 441;
    let gain = if p.volume >= 0 { 1.0 } else { 10f32.powf(p.volume.max(-10000) as f32 / 2000.0) };
    Some(SoundJob { sound: p.movie.sound.take()?, audio: a, next, n, gain })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Op {
        Run,
        Pause,
        Stop,
        Seek(u64),
        SetStop(u64),
        Wait(u64),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            Just(Op::Run),
            Just(Op::Pause),
            Just(Op::Stop),
            (0u64..12 * SEC).prop_map(Op::Seek),
            (0u64..12 * SEC).prop_map(Op::SetStop),
            (0u64..3 * SEC).prop_map(Op::Wait),
        ]
    }

    proptest! {
        /// Against a model of DirectShow's clock: the position moves only while running, by
        /// the time waited, never past the stop (unless seeked there) or the length; EC_COMPLETE
        /// comes once per reaching the stop while running, again only after a seek or a stop.
        #[test]
        fn clock_model(ops in prop::collection::vec(op(), 0..40)) {
            let d = 10 * SEC;
            let mut tl = Timeline::new(d);
            let (mut t, mut pos, mut stop, mut run, mut done, mut ends) = (1000u64, 0u64, d, false, false, 0);
            let mut got = 0;
            for o in ops {
                match o {
                    Op::Run => { tl.run(t); run = true; }
                    Op::Pause => { tl.pause(t); run = false; }
                    Op::Stop => { tl.stop(t); run = false; done = false; }
                    Op::Seek(p) => { tl.seek(t, p); pos = p.min(d); done = false; }
                    Op::SetStop(s) => { tl.set_stop(t, s); stop = s.min(d); if pos < stop { done = false; } }
                    Op::Wait(w) => {
                        // (in two steps: the end noticed in between)
                        for dt in [w / 2, w - w / 2] {
                            t += dt;
                            if run && pos < stop { pos = (pos + dt).min(stop); }
                            if tl.complete(t) { got += 1; }
                            if run && !done && pos >= stop { done = true; ends += 1; }
                        }
                    }
                }
                prop_assert_eq!(tl.position(t), pos);
                prop_assert_eq!(tl.running(), run);
                prop_assert_eq!(tl.done, done);
                prop_assert_eq!(got, ends);
                prop_assert!(tl.position(t) <= d);
            }
        }

        /// The frame shown is the one whose time has begun: frame k from k / fps on.
        #[test]
        fn frames_by_time(num in 1u32..240_000, den in 1u32..1001, k in 0u32..100_000, frames in 1u32..100_000) {
            prop_assume!(num as u64 <= 240 * den as u64);
            let start = (k as u128 * den as u128 * SEC as u128).div_ceil(num as u128) as u64;
            let f = frame_at(start, (num, den), frames);
            prop_assert_eq!(f, k.min(frames - 1));
            if start > 0 && k > 0 && k < frames {
                prop_assert_eq!(frame_at(start - 1, (num, den), frames), k - 1);
            }
        }

        /// Stretching: every pixel of `dst` inside the view comes from `src` (scaled), every
        /// other one is black; the same as the straightforward per-pixel mapping.
        #[test]
        fn blit_reference(fw in 1i32..9, fh in 1i32..9, src in (0i32..9, 0i32..9, 0i32..10, 0i32..10), dst in (-5i32..12, -5i32..12, 0i32..14, 0i32..14), view in (-2i32..6, -2i32..6, 1i32..12, 1i32..12)) {
            let frame: Vec<u32> = (0..fw * fh).map(|i| 0x10000 * i as u32 + 7).collect();
            let mut out = vec![0x1234u32; (view.2 * view.3) as usize];
            blit(&frame, fw, fh, src, &mut out, view, dst);
            let s = clip(src, (0, 0, fw, fh));
            for y in 0..view.3 {
                for x in 0..view.2 {
                    let (wx, wy) = (x + view.0, y + view.1);
                    let inside = s.2 > 0 && s.3 > 0 && wx >= dst.0 && wy >= dst.1 && wx < dst.0 + dst.2 && wy < dst.1 + dst.3;
                    let want = if inside {
                        let sx = s.0 + (wx - dst.0) * s.2 / dst.2;
                        let sy = s.1 + (wy - dst.1) * s.3 / dst.3;
                        frame[(sy * fw + sx) as usize] | 0xff00_0000
                    } else { 0xff00_0000 };
                    prop_assert_eq!(out[(y * view.2 + x) as usize], want, "at {},{}", x, y);
                }
            }
        }

        /// Letterboxing keeps the shape (to a pixel), fills one side, stays centred inside.
        #[test]
        fn letterbox_shape(dst in (-50i32..50, -50i32..50, 1i32..3000, 1i32..3000), sw in 1i32..4000, sh in 1i32..4000) {
            let r = letterbox(dst, sw, sh);
            prop_assert!(r.0 >= dst.0 && r.1 >= dst.1 && r.0 + r.2 <= dst.0 + dst.2 && r.1 + r.3 <= dst.1 + dst.3);
            prop_assert!(r.2 == dst.2 || r.3 == dst.3);
            // w/h within a pixel of sw/sh
            prop_assert!(((r.2 as i64 * sh as i64) - (r.3 as i64 * sw as i64)).abs() <= sw.max(sh) as i64);
            prop_assert!((r.0 - dst.0 - (dst.0 + dst.2 - r.0 - r.2)).abs() <= 1);
            prop_assert!((r.1 - dst.1 - (dst.1 + dst.3 - r.1 - r.3)).abs() <= 1);
        }
    }

    #[test]
    fn rate_and_end() {
        let mut tl = Timeline::new(4 * SEC);
        tl.run(0);
        tl.set_rate(SEC, 2.0);
        assert_eq!(tl.position(2 * SEC), 3 * SEC);
        assert_eq!(tl.until_end(2 * SEC), Some(SEC / 2));
        assert!(!tl.complete(2 * SEC));
        assert!(tl.complete(3 * SEC));
        assert!(!tl.complete(4 * SEC), "once");
        assert_eq!(tl.until_end(4 * SEC), None);
        // a seek back: it plays to the end again
        tl.seek(5 * SEC, 0);
        assert!(!tl.done);
        assert!(tl.complete(7 * SEC));
        // run at the end after a stop: it ends again at once
        tl.stop(8 * SEC);
        tl.run(8 * SEC);
        assert!(tl.complete(8 * SEC));
    }

    /// The sound in pieces from anywhere is the sound decoded whole (the test files' reference
    /// samples, crates/vorbis/testdata), stereo.
    #[test]
    fn sound_from_anywhere() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../vorbis/testdata/");
        for name in ["stereo-q5", "mono-q0"] {
            let ogg = std::fs::read(format!("{dir}{name}.ogg")).unwrap();
            let mut s = Vorbis::new(&ogg).unwrap();
            let mut whole = Vec::new();
            s.take(0, 200_000, &mut whole);
            // (read in small pieces, as a movie's sound is in 64 KiB ones: the same)
            for step in [1, 97, 4096] {
                let mut p = Vorbis::new(&ogg).unwrap();
                p.step = step;
                p.restart();
                let mut got = Vec::new();
                p.take(0, 200_000, &mut got);
                assert!(got == whole, "{name} read {step} bytes at a time");
            }
            let total = whole.chunks(2).rposition(|f| f[0] != 0.0 || f[1] != 0.0).unwrap_or(0) as u64 + 1;
            assert!(total > 10_000, "{name}: {total} frames");
            for (from, n) in [(0u64, 1000usize), (5000, 3000), (100, 50), (total - 10, 100), (2000, 7000), (0, 10), (total + 5, 10)] {
                let mut got = Vec::new();
                s.take(from, n, &mut got);
                let want: Vec<f32> = (0..n as u64).flat_map(|i| {
                    let k = (from + i) as usize;
                    if 2 * k + 1 < whole.len() { [whole[2 * k], whole[2 * k + 1]] } else { [0.0, 0.0] }
                }).collect();
                assert_eq!(got, want, "{name} from {from}");
            }
            if name == "mono-q0" {
                assert!(whole.chunks(2).all(|f| f[0] == f[1]));
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        /// Seeks back and forth in any order (marks every `every` frames): what take gives is
        /// the sound decoded whole, and a seek back to a point past the first mark does not
        /// decode from the start again
        #[test]
        fn seeks_back_from_marks(name in prop::sample::select(vec!["stereo-q5", "mono-q0", "stereo-48k"]), every in 200u64..3000,
                                 takes in proptest::collection::vec((0u64..14_000, 1usize..3000), 1..30)) {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../vorbis/testdata/");
            let ogg = std::fs::read(format!("{dir}{name}.ogg")).unwrap();
            let mut whole = Vec::new();
            Vorbis::new(&ogg).unwrap().take(0, 200_000, &mut whole);
            let mut s = Vorbis::new(&ogg).unwrap();
            s.every = every;
            s.step = 512;
            s.restart();
            s.restarts = 0;
            let mut want_restarts = 0;
            for (from, n) in takes {
                let back = from < s.base;
                let from_mark = s.marks.iter().any(|m| m.0 <= from);
                if back && !from_mark {
                    want_restarts += 1;
                }
                let mut got = Vec::new();
                s.take(from, n, &mut got);
                let want: Vec<f32> = (0..n as u64).flat_map(|i| {
                    let k = (from + i) as usize;
                    if 2 * k + 1 < whole.len() { [whole[2 * k], whole[2 * k + 1]] } else { [0.0, 0.0] }
                }).collect();
                prop_assert!(got == want, "{} from {} ({} frames)", name, from, n);
                prop_assert_eq!(s.restarts, want_restarts);
            }
            // (marks at least `every` apart, in order)
            prop_assert!(s.marks.windows(2).all(|w| w[1].0 >= w[0].0 + every));
        }
    }

    proptest! {
        /// frame_at: no rate with a zero in it divides (frame 0), the frame never passes the
        /// last one, and it never goes back as the position moves on
        #[test]
        fn frame_at_is_safe(pos in any::<u64>(), d in 0u64..1 << 40, rate in (0u32..1000, 0u32..1000), frames in 0u32..100_000) {
            let n = frame_at(pos, rate, frames);
            if rate.0 == 0 || rate.1 == 0 {
                prop_assert_eq!(n, 0);
            }
            prop_assert!(n <= frames.saturating_sub(1));
            prop_assert!(frame_at(pos.saturating_add(d), rate, frames) >= n);
        }
    }

    fn sound_play(key: u32) -> u64 {
        let ogg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../vorbis/testdata/stereo-q5.ogg")).unwrap();
        let movie = Movie { stream: 0, w: 4, h: 4, frames: 1, rate: (1, 1), duration: 10 * SEC, sound: Some(Sound::Vorbis(Vorbis::new(&ogg).unwrap())) };
        let mut p = Play::new(movie);
        let t = now();
        p.tl.run(t);
        with(|m| m.insert(key, p));
        t
    }

    /// The sound is decoded outside the lock: the job takes the decoder out of the play, and
    /// gives it back; what it decoded is queued only if the play still plays into the same
    /// stream from the same frame (a seek or a stop while it decoded: thrown away)
    #[test]
    fn sound_jobs_queue_only_what_still_fits() {
        for seek in [false, true] {
            let key = 0x5d00_0000 + seek as u32;
            let t = sound_play(key);
            let job = with(|m| sound_job(m.get_mut(&key).unwrap(), t)).expect("a job to queue sound");
            let (a, next, n) = (job.audio, job.next, job.n);
            assert_eq!(next, 0);
            assert!(read(|m| m[&key].movie.sound.is_none()), "the decoder is with the job");
            // (other threads' calls get the lock while it decodes)
            let other = std::thread::spawn(move || read(|m| m[&key].tl.duration));
            assert_eq!(other.join().unwrap(), 10 * SEC);
            if seek {
                with(|m| m.get_mut(&key).unwrap().audio = Some((a, 12345)));
            }
            job.run(key);
            with(|m| {
                let p = &m[&key];
                assert!(p.movie.sound.is_some(), "the decoder came back");
                if seek {
                    assert_eq!(p.audio, Some((a, 12345)));
                    assert_eq!(kura::audio::queued(a), 0, "nothing queued for the old place");
                } else {
                    assert_eq!(p.audio, Some((a, n as u64)));
                    // (the mixer may have played some already)
                    let q = kura::audio::queued(a);
                    assert!(q > n as u32 / 2 && q <= n as u32, "{q} of {n} queued");
                }
            });
            end(key);
        }
    }

    /// Nothing running or shown: the movie thread sleeps until a change wakes it (it woke 50
    /// times a second for good after the first movie); a running movie keeps it at 20 ms.
    #[test]
    fn idle_turns_sleep() {
        let key = 0x5d00_0100;
        let t = sound_play(key);
        let mut wait = turn();
        assert!(wait <= Duration::from_millis(20), "running: {wait:?}");
        with(|m| m.get_mut(&key).unwrap().tl.stop(t));
        wait = turn();
        end(key);
        // (other tests' plays may be about: only this one is known stopped)
        if read(|m| m.is_empty()) {
            assert!(turn() >= Duration::from_secs(3600));
        }
        assert!(wait >= Duration::from_millis(500) || read(|m| !m.is_empty()), "stopped: {wait:?}");
    }
}
