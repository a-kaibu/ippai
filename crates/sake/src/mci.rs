//! winmm's MCI (mciSendCommand / mciSendString / mciGetErrorString / mciGetDeviceID) and
//! msvfw32's MCIWnd, for the devices games play sound and movies with:
//!
//! - `waveaudio` (.wav) and `mpegvideo` (MP3 / MPEG / AVI / WMV, a file with no extension opened
//!   with `type mpegvideo`): a sound plays through the mixer (sound.rs) as PCM, MP3 and the
//!   others decoded by the page's AudioDecoder (audiofile.rs, kura::adec); a movie plays from
//!   the `.vnv` `ippai pack` made next to it (dshow_play.rs), in the window the program gives
//!   (MCI_WINDOW / MCI_DGV_OPEN_PARENT / MCIWnd), else on the screen.
//! - open (type, element, alias, by extension), close (one or all), play (from, to, notify,
//!   wait, repeat), stop, pause, resume, seek (to, start, end), status (length, position, mode,
//!   ready, media present, time format, tracks), set (time format: milliseconds; bytes and
//!   samples for waveaudio; frames for a movie), setaudio volume, window, put / where
//!   destination, capability, info.
//! - Notifications as Windows sends them (MM_MCINOTIFY to the callback window): a command with
//!   MCI_NOTIFY reports when it is done; a play's report waits for the end (SUCCESSFUL), and
//!   another command with notify supersedes it (SUPERSEDED), a stop, seek or close aborts it
//!   (ABORTED). See `Notes`.
//! - MCIWnd ("MCIWndClass"): MCIWNDM_OPEN, MCI_PLAY / STOP / PAUSE / RESUME / SEEK / CLOSE,
//!   MCIWNDM_PLAYFROM / PLAYTO / GETMODE / GETLENGTH / GETPOSITION / GETSTART / GETEND /
//!   CAN_PLAY / CAN_WINDOW / GETDEVICEID / SETVOLUME / GETVOLUME / SETREPEAT, and
//!   MCIWNDM_NOTIFYMODE to the parent (MCIWNDF_NOTIFYMODE) as the mode changes; the window
//!   takes the movie's size unless MCIWNDF_NOAUTOSIZEWINDOW. The play bar and menu are not
//!   drawn.
//!
//! Not here: `sequencer` (MIDI) and `cdaudio` (MCIERR_CANNOT_LOAD_DRIVER), recording, and the
//! less used commands (MCIERR_UNSUPPORTED_FUNCTION).

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::api::{Ctx, Handler};
use crate::dshow_play::{self as play, Timeline, SEC};
use crate::mem::{self, r32, w32};

// ---------------------------------------------------------------------------------------------
// constants (mmsystem.h, digitalv.h, vfw.h)

const MCIERR_INVALID_DEVICE_ID: u32 = 257;
const MCIERR_UNRECOGNIZED_KEYWORD: u32 = 259;
const MCIERR_UNRECOGNIZED_COMMAND: u32 = 261;
const MCIERR_INVALID_DEVICE_NAME: u32 = 263;
const MCIERR_CANNOT_LOAD_DRIVER: u32 = 266;
const MCIERR_MISSING_STRING_ARGUMENT: u32 = 269;
const MCIERR_BAD_INTEGER: u32 = 270;
const MCIERR_MISSING_PARAMETER: u32 = 273;
const MCIERR_UNSUPPORTED_FUNCTION: u32 = 274;
const MCIERR_FILE_NOT_FOUND: u32 = 275;
const MCIERR_OUTOFRANGE: u32 = 282;
const MCIERR_EXTENSION_NOT_FOUND: u32 = 281;
const MCIERR_DUPLICATE_ALIAS: u32 = 289;
const MCIERR_BAD_TIME_FORMAT: u32 = 293;
const MCIERR_INVALID_FILE: u32 = 296;
const MCIERR_NULL_PARAMETER_BLOCK: u32 = 297;
const MCIERR_NO_INTEGER: u32 = 312;

const MCI_OPEN: u32 = 0x803;
const MCI_CLOSE: u32 = 0x804;
const MCI_PLAY: u32 = 0x806;
const MCI_SEEK: u32 = 0x807;
const MCI_STOP: u32 = 0x808;
const MCI_PAUSE: u32 = 0x809;
const MCI_INFO: u32 = 0x80a;
const MCI_GETDEVCAPS: u32 = 0x80b;
const MCI_SET: u32 = 0x80d;
const MCI_STATUS: u32 = 0x814;
const MCI_CUE: u32 = 0x830;
const MCI_WINDOW: u32 = 0x841;
const MCI_PUT: u32 = 0x842;
const MCI_WHERE: u32 = 0x843;
const MCI_RESUME: u32 = 0x855;
const MCI_SETAUDIO: u32 = 0x873;

const MCI_NOTIFY: u32 = 1;
const MCI_WAIT: u32 = 2;
const MCI_FROM: u32 = 4;
const MCI_TO: u32 = 8;

const MCI_OPEN_ELEMENT: u32 = 0x200;
const MCI_OPEN_ALIAS: u32 = 0x400;
const MCI_OPEN_TYPE_ID: u32 = 0x1000;
const MCI_OPEN_TYPE: u32 = 0x2000;
const MCI_DGV_OPEN_WS: u32 = 0x1_0000;
const MCI_DGV_OPEN_PARENT: u32 = 0x2_0000;
const MCI_DGV_PLAY_REPEAT: u32 = 0x1_0000;
const MCI_SEEK_TO_START: u32 = 0x100;
const MCI_SEEK_TO_END: u32 = 0x200;
const MCI_STATUS_ITEM: u32 = 0x100;
const MCI_SET_TIME_FORMAT: u32 = 0x400;
const MCI_GETDEVCAPS_ITEM: u32 = 0x100;
const MCI_DGV_WINDOW_HWND: u32 = 0x1_0000;
const MCI_DGV_RECT: u32 = 0x1_0000;
const MCI_DGV_PUT_DESTINATION: u32 = 0x4_0000;
const MCI_DGV_WHERE_DESTINATION: u32 = 0x4_0000;
const MCI_DGV_SETAUDIO_ITEM: u32 = 0x80_0000;
const MCI_DGV_SETAUDIO_VALUE: u32 = 0x100_0000;
const MCI_DGV_SETAUDIO_VOLUME: u32 = 0x4002;

const MCI_STATUS_LENGTH: u32 = 1;
const MCI_STATUS_POSITION: u32 = 2;
const MCI_STATUS_NUMBER_OF_TRACKS: u32 = 3;
const MCI_STATUS_MODE: u32 = 4;
const MCI_STATUS_MEDIA_PRESENT: u32 = 5;
const MCI_STATUS_TIME_FORMAT: u32 = 6;
const MCI_STATUS_READY: u32 = 7;
const MCI_STATUS_CURRENT_TRACK: u32 = 8;
const MCI_DGV_STATUS_VOLUME: u32 = 0x4019;

pub const MCI_MODE_NOT_READY: u32 = 524;
pub const MCI_MODE_STOP: u32 = 525;
pub const MCI_MODE_PLAY: u32 = 526;
pub const MCI_MODE_PAUSE: u32 = 529;
pub const MCI_MODE_OPEN: u32 = 530;

pub const MCI_FORMAT_MILLISECONDS: u32 = 0;
pub const MCI_FORMAT_FRAMES: u32 = 3;
pub const MCI_FORMAT_BYTES: u32 = 8;
pub const MCI_FORMAT_SAMPLES: u32 = 9;

const MCI_ALL_DEVICE_ID: u32 = 0xffff;
const MM_MCINOTIFY: u32 = 0x3b9;
pub const MCI_NOTIFY_SUCCESSFUL: u32 = 1;
pub const MCI_NOTIFY_SUPERSEDED: u32 = 2;
pub const MCI_NOTIFY_ABORTED: u32 = 4;
pub const MCI_NOTIFY_FAILURE: u32 = 8;

// devices (MCI_DEVTYPE_*)
const DEVTYPE_WAVEFORM_AUDIO: u32 = 522;
const DEVTYPE_DIGITAL_VIDEO: u32 = 520;
const DEVTYPE_SEQUENCER: u32 = 523;
const DEVTYPE_CD_AUDIO: u32 = 516;

// ---------------------------------------------------------------------------------------------
// the pure parts

/// The kinds of device MCI opens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Wave,
    Mpeg,
    Avi,
}

/// A device type by its name (`type x`, MCI_OPEN_TYPE): Some(Err) for one Windows has but not
/// here (MIDI, CD), None for one it doesn't know.
pub fn kind_of_type(t: &str) -> Option<Result<Kind, u32>> {
    match t.to_ascii_lowercase().as_str() {
        "waveaudio" => Some(Ok(Kind::Wave)),
        "mpegvideo" | "mpegvideo2" | "digitalvideo" => Some(Ok(Kind::Mpeg)),
        "avivideo" => Some(Ok(Kind::Avi)),
        "sequencer" | "cdaudio" | "videodisc" | "vcr" | "overlay" | "scanner" | "animation" | "dat" => Some(Err(MCIERR_CANNOT_LOAD_DRIVER)),
        _ => None,
    }
}

/// A device type by MCI_OPEN_TYPE_ID's low word (MCI_DEVTYPE_*).
fn kind_of_id(id: u32) -> Result<Kind, u32> {
    match id & 0xffff {
        DEVTYPE_WAVEFORM_AUDIO => Ok(Kind::Wave),
        DEVTYPE_DIGITAL_VIDEO => Ok(Kind::Mpeg),
        DEVTYPE_SEQUENCER | DEVTYPE_CD_AUDIO => Err(MCIERR_CANNOT_LOAD_DRIVER),
        _ => Err(MCIERR_INVALID_DEVICE_NAME),
    }
}

/// The device a file opens by its extension (the registry's [MCI Extensions]): None for one
/// with no extension or one not listed.
pub fn kind_of_file(path: &str) -> Option<Result<Kind, u32>> {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "wav" => Ok(Kind::Wave),
        "avi" => Ok(Kind::Avi),
        "mp3" | "mp2" | "mpa" | "mpg" | "mpeg" | "m1v" | "m2v" | "mpe" | "mpv2" | "m1a" | "m2a" | "wma" | "wmv" | "asf" | "aif" | "aiff" | "aifc" | "au" | "snd" | "m4a" | "mp4" | "mov" => Ok(Kind::Mpeg),
        "mid" | "midi" | "rmi" | "cda" => Err(MCIERR_CANNOT_LOAD_DRIVER),
        _ => return None,
    })
}

/// A device's position or length (100 ns) in its time format: ms, frames of the movie's rate
/// (num/den a second), bytes or samples of the sound's format (rate, block).
pub fn to_format(t: u64, format: u32, rate: u32, block: u32, fps: (u32, u32)) -> u32 {
    let v = match format {
        MCI_FORMAT_FRAMES if fps.0 != 0 => t as u128 * fps.0 as u128 / (fps.1.max(1) as u128 * SEC as u128),
        MCI_FORMAT_SAMPLES => t as u128 * rate as u128 / SEC as u128,
        MCI_FORMAT_BYTES => t as u128 * rate as u128 / SEC as u128 * block as u128,
        _ => (t as u128 + 5_000) / 10_000,
    };
    v.min(u32::MAX as u128) as u32
}

/// A value in the time format back to 100 ns.
pub fn from_format(v: u32, format: u32, rate: u32, block: u32, fps: (u32, u32)) -> u64 {
    let v = v as u128;
    let t = match format {
        // (rounded up: the time of a frame or sample is where it starts, which reads back as it)
        MCI_FORMAT_FRAMES if fps.0 != 0 => (v * fps.1.max(1) as u128 * SEC as u128).div_ceil(fps.0 as u128),
        MCI_FORMAT_SAMPLES if rate != 0 => (v * SEC as u128).div_ceil(rate as u128),
        MCI_FORMAT_BYTES if rate != 0 && block != 0 => (v / block as u128 * SEC as u128).div_ceil(rate as u128),
        _ => v * 10_000,
    };
    t.min(u64::MAX as u128) as u64
}

/// Whether a device of `kind` (a movie when `video`) takes the time format.
pub fn format_ok(kind: Kind, video: bool, format: u32) -> bool {
    match format {
        MCI_FORMAT_MILLISECONDS => true,
        MCI_FORMAT_BYTES | MCI_FORMAT_SAMPLES => kind == Kind::Wave,
        MCI_FORMAT_FRAMES => kind != Kind::Wave && video,
        _ => false,
    }
}

/// MCI's volume (0..1000, setaudio) as hundredths of a dB (DirectSound's), and back.
pub fn volume_db(v: u32) -> i32 {
    if v == 0 {
        return -10000;
    }
    ((2000.0 * (v.min(1000) as f64 / 1000.0).log10()).round() as i32).clamp(-10000, 0)
}
pub fn volume_mci(db: i32) -> u32 {
    if db <= -10000 {
        return 0;
    }
    (1000.0 * 10f64.powf(db.min(0) as f64 / 2000.0)).round() as u32
}

/// The notifications a device owes: the one a play waits to give at its end, if any. Each
/// change returns what goes out now: (window, MCI_NOTIFY_*).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes {
    pub pending: Option<u32>,
}

impl Notes {
    /// A command arrives (`notify`: with MCI_NOTIFY to `hwnd`); `interrupts`: it stops a play
    /// (stop, seek, close, a new play). A pending report is superseded by a new notify and
    /// aborted by an interruption without one.
    pub fn command(&mut self, notify: Option<u32>, interrupts: bool) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        if let Some(w) = self.pending {
            if notify.is_some() {
                out.push((w, MCI_NOTIFY_SUPERSEDED));
                self.pending = None;
            } else if interrupts {
                out.push((w, MCI_NOTIFY_ABORTED));
                self.pending = None;
            }
        }
        out
    }
    /// A play with notify starts waiting for its end.
    pub fn wait_end(&mut self, hwnd: u32) {
        self.pending = Some(hwnd);
    }
    /// The play reached its end.
    pub fn end(&mut self) -> Vec<(u32, u32)> {
        self.pending.take().map(|w| vec![(w, MCI_NOTIFY_SUCCESSFUL)]).unwrap_or_default()
    }
}

/// A command string's words: spaces separate them, double quotes keep spaces in one (and are
/// dropped). None for an unclosed quote.
pub fn words(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if any {
        out.push(cur);
    }
    Some(out)
}

/// Windows' text for an MCI error (en-US), None for a code it has none for.
pub fn error_text(e: u32) -> Option<&'static str> {
    Some(match e {
        0 => "The specified command was carried out.",
        MCIERR_INVALID_DEVICE_ID => "Invalid MCI device ID. Use the ID returned when opening the MCI device.",
        MCIERR_UNRECOGNIZED_KEYWORD => "The driver cannot recognize the specified command parameter.",
        MCIERR_UNRECOGNIZED_COMMAND => "The driver cannot recognize the specified command.",
        MCIERR_INVALID_DEVICE_NAME => "The specified device is not open or is not recognized by MCI.",
        MCIERR_CANNOT_LOAD_DRIVER => "The specified device driver will not load properly.",
        MCIERR_MISSING_STRING_ARGUMENT => "The specified command requires a character-string parameter. Please provide one.",
        MCIERR_BAD_INTEGER => "The specified integer is invalid for this command.",
        MCIERR_MISSING_PARAMETER => "The specified command requires a parameter. Please supply one.",
        MCIERR_UNSUPPORTED_FUNCTION => "The MCI device you are using does not support the specified command.",
        MCIERR_FILE_NOT_FOUND => "Cannot find the specified file. Make sure the path and filename are correct.",
        MCIERR_EXTENSION_NOT_FOUND => "The specified extension has no device type associated with it. Specify a device when performing this command.",
        MCIERR_OUTOFRANGE => "The specified parameter is out of range for the specified command.",
        MCIERR_DUPLICATE_ALIAS => "The specified alias is already being used in this application. Use a unique alias.",
        MCIERR_BAD_TIME_FORMAT => "The specified format is invalid for this command.",
        MCIERR_INVALID_FILE => "The specified file cannot be played on the specified MCI device. The file may be corrupt, not in the correct format, or no file handler available for this format.",
        MCIERR_NULL_PARAMETER_BLOCK => "A null parameter block was passed to MCI.",
        MCIERR_NO_INTEGER => "The parameter for this MCI command must be an integer value.",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// devices

enum Media {
    /// a sound in guest memory the mixer plays under `key`
    Sound { data: u32, size: u32, fmt: crate::sound::Fmt },
    /// a movie (dshow_play) under `key`
    Movie { fps: (u32, u32), size: (u32, u32) },
    /// a sound with nothing to play it from (not decodable here): only its timing
    Silent { rate: u32, block: u32 },
}

struct Dev {
    kind: Kind,
    alias: String,
    element: String,
    media: Media,
    /// the play key of the mixer / the movie thread (a guest block of its own)
    key: u32,
    /// timing of a sound (a movie's is in its Play)
    tl: Timeline,
    duration: u64,
    format: u32,
    notes: Notes,
    /// `repeat`: from the start again at the end
    repeat: bool,
    /// bumped by every command that changes when the end comes (the waiter's stale check)
    gen: u32,
    /// the movie's window (MCI_WINDOW, the open's parent, an MCIWnd) and its place in it
    hwnd: u32,
    dst: Option<(i32, i32, i32, i32)>,
    volume: i32,
    /// the MCIWnd showing it (MCIWNDM_NOTIFYMODE as the mode changes)
    mciwnd: u32,
}

impl Dev {
    fn video(&self) -> bool {
        matches!(self.media, Media::Movie { .. })
    }
    fn rate_block(&self) -> (u32, u32) {
        match &self.media {
            Media::Sound { fmt, .. } => (fmt.rate, fmt.block()),
            Media::Silent { rate, block } => (*rate, *block),
            Media::Movie { .. } => (0, 0),
        }
    }
    fn fps(&self) -> (u32, u32) {
        match self.media {
            Media::Movie { fps, .. } => fps,
            _ => (0, 1),
        }
    }
    fn to_fmt(&self, t: u64) -> u32 {
        let (r, b) = self.rate_block();
        to_format(t, self.format, r, b, self.fps())
    }
    fn from_fmt(&self, v: u32) -> u64 {
        let (r, b) = self.rate_block();
        from_format(v, self.format, r, b, self.fps()).min(self.duration)
    }
}

#[derive(Default)]
struct State {
    devs: HashMap<u32, Dev>,
    next: u32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| State { devs: HashMap::new(), next: 1 }))
}
fn dev<R>(id: u32, f: impl FnOnce(&mut Dev) -> R) -> Option<R> {
    with(|s| s.devs.get_mut(&id).map(f))
}

/// The timeline of device `id` (the movie's own for a movie) to change, with the time now.
fn timeline<R>(id: u32, f: impl FnOnce(&mut Timeline, u64) -> R) -> Option<R> {
    let (key, video) = dev(id, |d| (d.key, d.video()))?;
    let t = play::now();
    if video {
        play::with(|m| m.get_mut(&key).map(|p| f(&mut p.tl, t)))
    } else {
        dev(id, |d| f(&mut d.tl, t))
    }
}

/// playing (running, short of its stop), paused (held mid-play), else stopped
fn mode(id: u32) -> u32 {
    let Some(playing) = timeline(id, |tl, t| tl.running() && tl.position(t) < tl.stop) else { return MCI_MODE_NOT_READY };
    if playing {
        MCI_MODE_PLAY
    } else if dev(id, |d| d.paused()).unwrap_or(false) {
        MCI_MODE_PAUSE
    } else {
        MCI_MODE_STOP
    }
}

impl Dev {
    /// paused (not stopped): a play is under way but held
    fn paused(&self) -> bool {
        self.gen & 0x8000_0000 != 0
    }
}

fn post_all(out: Vec<(u32, u32)>, id: u32) {
    for (w, code) in out {
        if w != 0 {
            crate::user::post(w, MM_MCINOTIFY, code, id);
        }
    }
}

/// The sound's mixer state follows its timeline (where it is, whether it plays).
fn sync_sound(id: u32) {
    let Some((key, frames_at, playing)) = dev(id, |d| {
        let t = play::now();
        let pos = d.tl.position(t);
        let rate = d.rate_block().0 as f64;
        (d.key, pos as f64 * rate / SEC as f64, d.tl.running() && pos < d.tl.stop)
    }) else {
        return;
    };
    crate::sound::music_set(key, Some(frames_at), Some(playing));
}

/// A device's movie gets its place (window, rectangle) and volume.
fn sync_movie(id: u32) {
    let Some((key, hwnd, dst, size, volume)) = dev(id, |d| (d.key, d.hwnd, d.dst, match d.media { Media::Movie { size, .. } => size, _ => (0, 0) }, d.volume)) else { return };
    let client = if hwnd != 0 { crate::user::client_rect_screen(hwnd).map(|r| (r.r - r.l, r.b - r.t)) } else { None };
    let dst = dst.unwrap_or_else(|| match client {
        // (an MCIWnd and a window given: its client area; the screen: the movie's size)
        Some((w, h)) => (0, 0, w, h),
        None => (0, 0, size.0 as i32, size.1 as i32),
    });
    play::with(|m| {
        if let Some(p) = m.get_mut(&key) {
            p.place = play::Place { hwnd, dst, src: (0, 0, size.0 as i32, size.1 as i32), letterbox: false, visible: true };
            p.volume = volume;
        }
    });
}

/// Starts the waiter of device `id`'s end: a guest thread that sleeps until then and, if
/// nothing changed it meanwhile, reports it (notify, repeat, MCIWnd's mode).
fn arm(id: u32) {
    let Some(gen) = dev(id, |d| {
        d.gen = (d.gen + 1) & 0x7fff_ffff;
        d.gen
    }) else {
        return;
    };
    static ENTRY: OnceLock<u32> = OnceLock::new();
    let entry = *ENTRY.get_or_init(|| crate::api::host_fn("mci_waiter", waiter, 4));
    let h = crate::obj::new_thread_obj(0);
    crate::spawn_gated(entry, (id << 16) | (gen & 0xffff), 0x10000, h, false);
}

/// Bumps the generation (a waiter in flight gives up), keeping the paused flag.
fn disarm(id: u32) {
    dev(id, |d| d.gen = (d.gen & 0x8000_0000) | ((d.gen + 1) & 0x7fff_ffff));
}

fn waiter(c: &mut Ctx) -> u32 {
    let (id, gen) = (c.a(0) >> 16, c.a(0) & 0xffff);
    loop {
        let live = dev(id, |d| d.gen & 0xffff == gen && !d.paused()).unwrap_or(false);
        if !live {
            return 0;
        }
        let left = timeline(id, |tl, t| tl.until_end(t)).flatten();
        let Some(left) = left else { return 0 };
        if left > 0 {
            kura::snap::sleep(Duration::from_nanos(left * 100).min(Duration::from_millis(250)));
            crate::snap::point();
            continue;
        }
        // the end: once (a movie's may come from the movie thread first: `ended`)
        if !timeline(id, |tl, t| tl.complete(t)).unwrap_or(false) {
            return 0;
        }
        if !finished(id) {
            return 0;
        }
    }
}

/// Device `id` played to its end: again from the start (repeat; true), or its report goes out
/// and an MCIWnd shows the new mode.
fn finished(id: u32) -> bool {
    let (repeat, mciwnd) = dev(id, |d| (d.repeat, d.mciwnd)).unwrap_or((false, 0));
    if repeat {
        timeline(id, |tl, t| {
            tl.seek(t, 0);
            tl.run(t);
        });
        sync_sound(id);
        return true;
    }
    let out = dev(id, |d| d.notes.end()).unwrap_or_default();
    post_all(out, id);
    sync_sound(id);
    if mciwnd != 0 {
        crate::user::post(mciwnd, WM_MCIWND_MODE, 0, 0);
    }
    false
}

/// The movie thread saw the movie under `key` end: whether it is one of ours.
pub fn ended(key: u32) -> bool {
    let Some(id) = with(|s| s.devs.iter().find(|(_, d)| d.key == key && d.video()).map(|(&i, _)| i)) else { return false };
    if finished(id) {
        // (repeating: its waiter again)
        arm(id);
    }
    true
}

/// Opens device `kind` (None: by the element's extension) on `element`, as `alias`.
fn open(kind: Option<Kind>, element: &str, alias: Option<&str>) -> Result<u32, u32> {
    if element.is_empty() {
        // (a device opened without a file: only a simple device; these are compound)
        return Err(MCIERR_UNSUPPORTED_FUNCTION);
    }
    let kind = match kind {
        Some(k) => k,
        None => kind_of_file(element).ok_or(MCIERR_EXTENSION_NOT_FOUND)??,
    };
    let alias = alias.map(|a| a.to_ascii_lowercase()).unwrap_or_else(|| element.to_ascii_lowercase());
    if with(|s| s.devs.values().any(|d| d.alias == alias)) {
        return Err(MCIERR_DUPLICATE_ALIAS);
    }
    let full = crate::files::full_path(element);
    let vnv = play::Movie::find(&full);
    let key = mem::heap_alloc(4, true);
    let (media, duration) = if let Some(movie) = vnv.filter(|_| kind != Kind::Wave) {
        let m = Media::Movie { fps: movie.rate, size: (movie.w, movie.h) };
        let d = movie.duration;
        eprintln!("sake: movie {element}: {}x{} {} frames (MCI)", movie.w, movie.h, movie.frames);
        let mut p = play::Play::new(movie);
        p.place.visible = false;
        play::start(key, p);
        (m, d)
    } else {
        let Some(bytes) = crate::files::resolve(&full).and_then(|h| kura::fs::read(h).ok()) else {
            mem::heap_free(key);
            return Err(MCIERR_FILE_NOT_FOUND);
        };
        match sound(&bytes) {
            Some(x) => x,
            None => {
                mem::heap_free(key);
                if crate::files::resolve(&full).is_some() && kind != Kind::Wave {
                    eprintln!("sake: mci {element}: not played (no .vnv to decode, see ippai pack)");
                }
                return Err(MCIERR_INVALID_FILE);
            }
        }
    };
    if let Media::Sound { data, size, fmt } = media {
        crate::sound::music_start(key, data, size, fmt, 0, 0);
        crate::sound::music_set(key, Some(0.0), Some(false));
    }
    let id = with(|s| {
        let id = s.next;
        s.next += 1;
        s.devs.insert(
            id,
            Dev { kind, alias, element: element.to_string(), media, key, tl: Timeline::new(duration), duration, format: MCI_FORMAT_MILLISECONDS, notes: Notes::default(), repeat: false, gen: 0, hwnd: 0, dst: None, volume: 0, mciwnd: 0 },
        );
        id
    });
    Ok(id)
}

/// A sound file's samples in guest memory (PCM as it is; the rest decoded by the page), and its
/// length. A sound the page can't decode (or no page) still opens, silent, with its length.
/// A sound file's PCM for the mixer, as MCI's waveaudio / mpegvideo play it: (the PCM in guest
/// memory, its size and format, or None when nothing here decodes it), its sample rate, its
/// duration. DirectShow's graph plays sound-only files with it (dshow.rs).
pub(crate) fn sound_pcm(b: &[u8]) -> Option<(Option<(u32, u32, crate::sound::Fmt)>, u32, u64)> {
    let (m, d) = sound(b)?;
    Some(match m {
        Media::Sound { data, size, fmt } => (Some((data, size, fmt)), fmt.rate, d),
        Media::Silent { rate, .. } => (None, rate, d),
        Media::Movie { .. } => return None,
    })
}

fn sound(b: &[u8]) -> Option<(Media, u64)> {
    use crate::audiofile::Codec;
    let t = crate::audiofile::open(b)?;
    let frames = t.total_frames();
    let duration = frames * SEC / t.rate.max(1) as u64;
    match &t.codec {
        Codec::Pcm { bits, float } => {
            let first = t.packets.first()?;
            let last = t.packets.last()?;
            let (start, end) = (first.0 as usize, (last.0 + last.1 as u64) as usize);
            let pcm = b.get(start..end.min(b.len()))?;
            let p = mem::heap_alloc(pcm.len() as u32, false);
            mem::slice_mut(p, pcm.len() as u32).copy_from_slice(pcm);
            let fmt = crate::sound::Fmt { rate: t.rate, channels: t.channels, bits: *bits as u32, float: *float, law: 0 };
            Some((Media::Sound { data: p, size: pcm.len() as u32, fmt }, duration))
        }
        Codec::Page { codec, desc, .. } => {
            let block = t.channels * 2;
            let silent = (Media::Silent { rate: t.rate, block }, duration);
            let Some(dec) = kura::adec::open(codec, t.rate, t.channels, desc) else { return Some(silent) };
            let mut pcm = Vec::new();
            let ok = kura::adec::decode(dec, t.packets.iter().filter_map(|&(o, n)| b.get(o as usize..o as usize + n as usize)), Duration::from_secs(20), &mut pcm).is_some();
            kura::adec::close(dec);
            if !ok || pcm.is_empty() {
                return Some(silent);
            }
            let bytes: Vec<u8> = pcm.iter().flat_map(|x| x.to_le_bytes()).collect();
            let p = mem::heap_alloc(bytes.len() as u32, false);
            mem::slice_mut(p, bytes.len() as u32).copy_from_slice(&bytes);
            let fmt = crate::sound::Fmt { rate: t.rate, channels: t.channels, bits: 32, float: true, law: 0 };
            Some((Media::Sound { data: p, size: bytes.len() as u32, fmt }, duration))
        }
    }
}

fn close(id: u32) -> bool {
    let Some(d) = with(|s| s.devs.remove(&id)) else { return false };
    post_all(d.notes.clone().command(None, true), id);
    match d.media {
        Media::Movie { .. } => play::end(d.key),
        Media::Sound { data, .. } => {
            crate::sound::music_stop(d.key);
            mem::heap_free(data);
        }
        Media::Silent { .. } => {}
    }
    mem::heap_free(d.key);
    true
}

/// play [from] [to]: runs from `from` (else where it is; at the end: from the start) to `to`
/// (else the end).
fn play_dev(id: u32, from: Option<u32>, to: Option<u32>, repeat: bool) -> Result<(), u32> {
    let Some((f, tt, dur)) = dev(id, |d| (from.map(|v| d.from_fmt(v)), to.map(|v| d.from_fmt(v)), d.duration)) else { return Err(MCIERR_INVALID_DEVICE_ID) };
    if let (Some(a), Some(b)) = (f, tt) {
        if a > b {
            return Err(MCIERR_OUTOFRANGE);
        }
    }
    dev(id, |d| {
        d.repeat = repeat;
        d.gen &= 0x7fff_ffff;
    });
    timeline(id, |tl, t| {
        let at = tl.position(t);
        let from = f.unwrap_or(if at >= tl.stop || at >= dur { 0 } else { at });
        tl.set_stop(t, tt.unwrap_or(dur));
        tl.seek(t, from);
        tl.run(t);
    });
    sync_sound(id);
    sync_movie(id);
    Ok(())
}

fn stop_dev(id: u32, pause: bool) {
    timeline(id, |tl, t| if pause { tl.pause(t) } else { tl.stop(t) });
    dev(id, |d| {
        if pause {
            d.gen |= 0x8000_0000;
        } else {
            d.gen &= 0x7fff_ffff;
        }
    });
    disarm(id);
    sync_sound(id);
}

fn seek_dev(id: u32, to: u64) {
    timeline(id, |tl, t| {
        tl.stop(t);
        tl.seek(t, to);
    });
    dev(id, |d| d.gen &= 0x7fff_ffff);
    disarm(id);
    sync_sound(id);
}

fn position(id: u32) -> u64 {
    timeline(id, |tl, t| tl.position(t)).unwrap_or(0)
}

/// Waits (MCI_WAIT) until the play of `id` ends.
fn wait_end(id: u32) {
    loop {
        let left = timeline(id, |tl, t| tl.until_end(t)).flatten();
        match left {
            Some(l) if l > 0 => {
                kura::snap::sleep(Duration::from_nanos(l * 100).min(Duration::from_millis(100)));
                crate::snap::point();
            }
            _ => break,
        }
    }
    timeline(id, |tl, t| tl.complete(t));
    sync_sound(id);
}

// ---------------------------------------------------------------------------------------------
// mciSendCommand

/// A parameter block's field after `ptrs` pointer-sized fields and then `dwords` DWORDs.
fn field(p: u32, ptrs: u32, dwords: u32) -> u32 {
    p + ptrs * mem::ps() + dwords * 4
}
fn aligned(off: u32) -> u32 {
    let ps = mem::ps();
    off.div_ceil(ps) * ps
}

fn send_command(c: &mut Ctx, id: u32, msg: u32, flags: u32, p: u32, wide: bool) -> u32 {
    let cb = if p != 0 { mem::rp(p) } else { 0 };
    let notify = (flags & MCI_NOTIFY != 0).then_some(cb);
    if msg == MCI_OPEN {
        if p == 0 {
            return MCIERR_NULL_PARAMETER_BLOCK;
        }
        let ps = mem::ps();
        // MCI_OPEN_PARMS { dwCallback, wDeviceID, lpstrDeviceType, lpstrElementName, lpstrAlias }
        let id_at = p + ps;
        let type_at = p + aligned(ps + 4);
        let elem_at = type_at + ps;
        let alias_at = elem_at + ps;
        let s = |a: u32| {
            let v = mem::rp(a);
            if v == 0 { String::new() } else { crate::k32::arg_str(v, wide) }
        };
        let kind = if flags & MCI_OPEN_TYPE_ID != 0 {
            match kind_of_id(mem::rp(type_at)) {
                Ok(k) => Some(k),
                Err(e) => return e,
            }
        } else if flags & MCI_OPEN_TYPE != 0 {
            match kind_of_type(&s(type_at)) {
                Some(Ok(k)) => Some(k),
                Some(Err(e)) => return e,
                None => return MCIERR_INVALID_DEVICE_NAME,
            }
        } else {
            None
        };
        let element = if flags & MCI_OPEN_ELEMENT != 0 { s(elem_at) } else { String::new() };
        let alias = (flags & MCI_OPEN_ALIAS != 0).then(|| s(alias_at));
        let id = match open(kind, &element, alias.as_deref()) {
            Ok(id) => id,
            Err(e) => return e,
        };
        w32(id_at, id);
        // MCI_DGV_OPEN_PARMS: dwStyle, hWndParent after the alias
        if flags & MCI_DGV_OPEN_PARENT != 0 {
            let style_at = alias_at + ps;
            let parent = mem::rp(style_at + ps);
            let child = flags & MCI_DGV_OPEN_WS != 0 && r32(style_at) & 0x4000_0000 != 0;
            dev(id, |d| d.hwnd = if child || parent != 0 { parent } else { 0 });
        }
        post_all(notify.map(|w| vec![(w, MCI_NOTIFY_SUCCESSFUL)]).unwrap_or_default(), id);
        return 0;
    }
    if id == MCI_ALL_DEVICE_ID && msg == MCI_CLOSE {
        let ids: Vec<u32> = with(|s| s.devs.keys().copied().collect());
        for i in ids {
            close(i);
        }
        return 0;
    }
    if dev(id, |_| ()).is_none() {
        return MCIERR_INVALID_DEVICE_ID;
    }
    let interrupts = matches!(msg, MCI_STOP | MCI_SEEK | MCI_CLOSE | MCI_PLAY);
    let out = dev(id, |d| d.notes.command(notify, interrupts)).unwrap_or_default();
    post_all(out, id);
    let r = command(c, id, msg, flags, p, wide);
    if r == 0 {
        if let Some(w) = notify {
            if msg == MCI_PLAY {
                dev(id, |d| d.notes.wait_end(w));
            } else {
                post_all(vec![(w, MCI_NOTIFY_SUCCESSFUL)], id);
            }
        }
        if msg == MCI_PLAY {
            if flags & MCI_WAIT != 0 {
                wait_end(id);
                let out = dev(id, |d| d.notes.end()).unwrap_or_default();
                post_all(out, id);
            } else {
                arm(id);
            }
        }
    } else if let Some(w) = notify {
        post_all(vec![(w, MCI_NOTIFY_FAILURE)], id);
    }
    r
}

fn command(_c: &mut Ctx, id: u32, msg: u32, flags: u32, p: u32, wide: bool) -> u32 {
    let need = |p: u32| p != 0;
    match msg {
        MCI_CLOSE => {
            close(id);
            0
        }
        MCI_PLAY => {
            // MCI_PLAY_PARMS { dwCallback, dwFrom, dwTo }
            let from = (flags & MCI_FROM != 0 && need(p)).then(|| r32(field(p, 1, 0)));
            let to = (flags & MCI_TO != 0 && need(p)).then(|| r32(field(p, 1, 1)));
            match play_dev(id, from, to, flags & MCI_DGV_PLAY_REPEAT != 0) {
                Ok(()) => 0,
                Err(e) => e,
            }
        }
        MCI_STOP => {
            stop_dev(id, false);
            0
        }
        MCI_PAUSE => {
            if mode(id) == MCI_MODE_PLAY {
                stop_dev(id, true);
            }
            0
        }
        MCI_RESUME => {
            if dev(id, |d| d.paused()).unwrap_or(false) {
                dev(id, |d| d.gen &= 0x7fff_ffff);
                timeline(id, |tl, t| tl.run(t));
                sync_sound(id);
                arm(id);
            }
            0
        }
        MCI_SEEK => {
            let to = if flags & MCI_SEEK_TO_START != 0 {
                0
            } else if flags & MCI_SEEK_TO_END != 0 {
                dev(id, |d| d.duration).unwrap_or(0)
            } else if flags & MCI_TO != 0 && need(p) {
                let v = r32(field(p, 1, 0));
                let (t, dur) = dev(id, |d| (d.from_fmt(v), d.duration)).unwrap_or((0, 0));
                if dev(id, |d| d.to_fmt(dur)).unwrap_or(0) < v {
                    return MCIERR_OUTOFRANGE;
                }
                t
            } else {
                return MCIERR_MISSING_PARAMETER;
            };
            seek_dev(id, to);
            0
        }
        MCI_STATUS => {
            if !need(p) {
                return MCIERR_NULL_PARAMETER_BLOCK;
            }
            if flags & MCI_STATUS_ITEM == 0 {
                return MCIERR_MISSING_PARAMETER;
            }
            // MCI_STATUS_PARMS { dwCallback, dwReturn, dwItem, dwTrack }
            let ret = field(p, 1, 0);
            let item = r32(field(p, 2, 0));
            let v = match status(id, item) {
                Ok(v) => v,
                Err(e) => return e,
            };
            mem::wp(ret, v);
            0
        }
        MCI_SET => {
            if flags & MCI_SET_TIME_FORMAT != 0 {
                let f = r32(field(p, 1, 0));
                let ok = dev(id, |d| format_ok(d.kind, d.video(), f)).unwrap_or(false);
                if !ok {
                    return MCIERR_BAD_TIME_FORMAT;
                }
                dev(id, |d| d.format = f);
            }
            0
        }
        MCI_SETAUDIO => {
            // MCI_DGV_SETAUDIO_PARMS { dwCallback, dwItem, dwValue, ... }
            if flags & MCI_DGV_SETAUDIO_ITEM != 0 && flags & MCI_DGV_SETAUDIO_VALUE != 0 && r32(field(p, 1, 0)) == MCI_DGV_SETAUDIO_VOLUME {
                set_volume(id, volume_db(r32(field(p, 1, 1))));
            }
            0
        }
        MCI_WINDOW => {
            // MCI_DGV_WINDOW_PARMS { dwCallback, hWnd, ... }
            if flags & MCI_DGV_WINDOW_HWND != 0 {
                let h = mem::rp(field(p, 1, 0));
                dev(id, |d| d.hwnd = h);
                sync_movie(id);
            }
            0
        }
        MCI_PUT => {
            // MCI_DGV_RECT_PARMS { dwCallback, RECT rc } (right, bottom: width, height)
            if flags & MCI_DGV_PUT_DESTINATION != 0 && flags & MCI_DGV_RECT != 0 {
                let r = field(p, 1, 0);
                let rect = (r32(r) as i32, r32(r + 4) as i32, r32(r + 8) as i32, r32(r + 12) as i32);
                dev(id, |d| d.dst = Some(rect));
                sync_movie(id);
            }
            0
        }
        MCI_WHERE => {
            if flags & MCI_DGV_WHERE_DESTINATION != 0 {
                let (dst, size) = dev(id, |d| (d.dst, match d.media { Media::Movie { size, .. } => size, _ => (0, 0) })).unwrap_or((None, (0, 0)));
                let (x, y, w, h) = dst.unwrap_or((0, 0, size.0 as i32, size.1 as i32));
                let r = field(p, 1, 0);
                for (i, v) in [x, y, w, h].into_iter().enumerate() {
                    w32(r + 4 * i as u32, v as u32);
                }
            }
            0
        }
        MCI_GETDEVCAPS => {
            if flags & MCI_GETDEVCAPS_ITEM == 0 || !need(p) {
                return MCIERR_MISSING_PARAMETER;
            }
            let item = r32(field(p, 2, 0));
            let (kind, video) = dev(id, |d| (d.kind, d.video())).unwrap_or((Kind::Mpeg, false));
            let v = match item {
                // CAN_RECORD, HAS_AUDIO, HAS_VIDEO, DEVICE_TYPE, USES_FILES, COMPOUND_DEVICE, CAN_EJECT, CAN_PLAY, CAN_SAVE
                1 => 0,
                2 => 1,
                3 => video as u32,
                4 => if kind == Kind::Wave { DEVTYPE_WAVEFORM_AUDIO } else { DEVTYPE_DIGITAL_VIDEO },
                5 | 6 => 1,
                7 => 0,
                8 => 1,
                9 => 0,
                _ => return MCIERR_UNSUPPORTED_FUNCTION,
            };
            mem::wp(field(p, 1, 0), v);
            0
        }
        MCI_INFO => {
            // MCI_INFO_PARMS { dwCallback, lpstrReturn, dwRetSize }; MCI_INFO_FILE 0x200, PRODUCT 0x100
            let (buf, size) = (mem::rp(field(p, 1, 0)), r32(field(p, 2, 0)));
            let (kind, el) = dev(id, |d| (d.kind, d.element.clone())).unwrap_or((Kind::Mpeg, String::new()));
            let s = if flags & 0x200 != 0 { el } else { product(kind).to_string() };
            crate::k32::ret_str(buf, size, &s, wide);
            0
        }
        MCI_CUE => 0,
        _ => MCIERR_UNSUPPORTED_FUNCTION,
    }
}

fn product(kind: Kind) -> &'static str {
    match kind {
        Kind::Wave => "WAVE Audio and Video",
        Kind::Avi => "Video for Windows",
        Kind::Mpeg => "DirectShow",
    }
}

fn status(id: u32, item: u32) -> Result<u32, u32> {
    Ok(match item {
        MCI_STATUS_LENGTH => dev(id, |d| d.to_fmt(d.duration)).unwrap_or(0),
        MCI_STATUS_POSITION => {
            let p = position(id);
            dev(id, |d| d.to_fmt(p)).unwrap_or(0)
        }
        MCI_STATUS_NUMBER_OF_TRACKS | MCI_STATUS_CURRENT_TRACK => 1,
        MCI_STATUS_MODE => mode(id),
        MCI_STATUS_MEDIA_PRESENT | MCI_STATUS_READY => 1,
        MCI_STATUS_TIME_FORMAT => dev(id, |d| d.format).unwrap_or(0),
        MCI_DGV_STATUS_VOLUME => dev(id, |d| volume_mci(d.volume)).unwrap_or(0),
        _ => return Err(MCIERR_UNSUPPORTED_FUNCTION),
    })
}

fn set_volume(id: u32, db: i32) {
    let Some((key, video)) = dev(id, |d| {
        d.volume = db;
        (d.key, d.video())
    }) else {
        return;
    };
    if video {
        sync_movie(id);
    } else {
        crate::sound::music_volume(key, db);
    }
}

fn mciSendCommandA(c: &mut Ctx) -> u32 {
    let (id, msg, flags, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    send_command(c, id, msg, flags, p, false)
}
fn mciSendCommandW(c: &mut Ctx) -> u32 {
    let (id, msg, flags, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    send_command(c, id, msg, flags, p, true)
}

// ---------------------------------------------------------------------------------------------
// mciSendString

fn device_id(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    with(|s| s.devs.iter().find(|(_, d)| d.alias == n || d.element.to_ascii_lowercase() == n).map_or(0, |(&i, _)| i))
}

fn int(w: Option<&String>) -> Result<u32, u32> {
    let w = w.ok_or(MCIERR_MISSING_PARAMETER)?;
    w.parse::<u32>().map_err(|_| MCIERR_BAD_INTEGER)
}

/// Runs a command string; the text it returns.
fn send_string(c: &mut Ctx, cmd: &str, cb: u32) -> Result<String, u32> {
    let Some(w) = words(cmd) else { return Err(MCIERR_MISSING_STRING_ARGUMENT) };
    let lw: Vec<String> = w.iter().map(|x| x.to_ascii_lowercase()).collect();
    let Some(verb) = lw.first() else { return Err(MCIERR_MISSING_STRING_ARGUMENT) };
    let Some(name) = w.get(1) else { return Err(MCIERR_MISSING_PARAMETER) };
    let rest = &lw[2..];
    let has = |k: &str| rest.iter().any(|x| x == k);
    // (the word after keyword `k`, as an index into `w`)
    let after = |k: &str| rest.iter().position(|x| x == k).map(|i| i + 3);
    let mut flags = 0;
    if has("notify") {
        flags |= MCI_NOTIFY;
    }
    if has("wait") {
        flags |= MCI_WAIT;
    }
    let notify = (flags & MCI_NOTIFY != 0).then_some(cb);
    if verb == "open" {
        // open <element or device> [type t] [alias a] [shareable] [wait|notify] [style child] [parent h]
        let kind = match after("type").map(|i| w.get(i)) {
            Some(Some(t)) => match kind_of_type(t) {
                Some(Ok(k)) => Some(k),
                Some(Err(e)) => return Err(e),
                None => return Err(MCIERR_INVALID_DEVICE_NAME),
            },
            Some(None) => return Err(MCIERR_MISSING_PARAMETER),
            None => None,
        };
        // (a bare device name: "open waveaudio" has no element)
        let (kind, element) = match (kind, kind_of_type(name)) {
            (None, Some(Ok(k))) => (Some(k), String::new()),
            (None, Some(Err(e))) => return Err(e),
            (k, _) => (k, name.clone()),
        };
        let alias = after("alias").and_then(|i| w.get(i)).map(|s| s.as_str());
        let id = open(kind, &element, alias)?;
        if let Some(h) = after("parent").and_then(|i| w.get(i)).and_then(|s| s.parse::<u32>().ok()) {
            dev(id, |d| d.hwnd = h);
        }
        post_all(notify.map(|w| vec![(w, MCI_NOTIFY_SUCCESSFUL)]).unwrap_or_default(), id);
        return Ok(id.to_string());
    }
    if verb == "close" && lw[1] == "all" {
        let ids: Vec<u32> = with(|s| s.devs.keys().copied().collect());
        for i in ids {
            close(i);
        }
        return Ok(String::new());
    }
    // (the command first: an unknown one is that, whatever the device)
    const VERBS: [&str; 17] = ["play", "stop", "pause", "resume", "close", "cue", "seek", "status", "set", "setaudio", "window", "put", "where", "capability", "info", "open", "close"];
    if !VERBS.contains(&verb.as_str()) {
        return Err(MCIERR_UNRECOGNIZED_COMMAND);
    }
    let id = device_id(name);
    if id == 0 {
        return Err(MCIERR_INVALID_DEVICE_NAME);
    }
    // the command as the parameter block mciSendCommand takes
    let ps = mem::ps();
    let p = mem::heap_alloc(64, true);
    mem::wp(p, cb);
    let mut ret = String::new();
    let r = (|| -> Result<u32, u32> {
        let msg = match verb.as_str() {
            "play" => {
                if let Some(i) = after("from") {
                    flags |= MCI_FROM;
                    w32(p + ps, int(w.get(i))?);
                }
                if let Some(i) = after("to") {
                    flags |= MCI_TO;
                    w32(p + ps + 4, int(w.get(i))?);
                }
                if has("repeat") {
                    flags |= MCI_DGV_PLAY_REPEAT;
                }
                MCI_PLAY
            }
            "stop" => MCI_STOP,
            "pause" => MCI_PAUSE,
            "resume" => MCI_RESUME,
            "close" => MCI_CLOSE,
            "cue" => MCI_CUE,
            "seek" => {
                match after("to").map(|i| lw.get(i).map(|s| s.as_str())) {
                    Some(Some("start")) => flags |= MCI_SEEK_TO_START,
                    Some(Some("end")) => flags |= MCI_SEEK_TO_END,
                    Some(_) => {
                        flags |= MCI_TO;
                        w32(p + ps, int(after("to").and_then(|i| w.get(i)))?);
                    }
                    None => return Err(MCIERR_MISSING_PARAMETER),
                }
                MCI_SEEK
            }
            "status" => {
                let item = rest.iter().filter(|x| *x != "notify" && *x != "wait").cloned().collect::<Vec<_>>().join(" ");
                let it = match item.as_str() {
                    "length" => MCI_STATUS_LENGTH,
                    "position" => MCI_STATUS_POSITION,
                    "mode" => MCI_STATUS_MODE,
                    "ready" => MCI_STATUS_READY,
                    "media present" => MCI_STATUS_MEDIA_PRESENT,
                    "time format" => MCI_STATUS_TIME_FORMAT,
                    "number of tracks" => MCI_STATUS_NUMBER_OF_TRACKS,
                    "current track" => MCI_STATUS_CURRENT_TRACK,
                    "volume" => MCI_DGV_STATUS_VOLUME,
                    "" => return Err(MCIERR_MISSING_PARAMETER),
                    _ => return Err(MCIERR_UNRECOGNIZED_KEYWORD),
                };
                let v = status(id, it)?;
                ret = match it {
                    MCI_STATUS_MODE => match v {
                        MCI_MODE_PLAY => "playing",
                        MCI_MODE_PAUSE => "paused",
                        MCI_MODE_OPEN => "open",
                        MCI_MODE_NOT_READY => "not ready",
                        _ => "stopped",
                    }
                    .into(),
                    MCI_STATUS_READY | MCI_STATUS_MEDIA_PRESENT => if v != 0 { "true" } else { "false" }.into(),
                    MCI_STATUS_TIME_FORMAT => format_name(v).into(),
                    _ => v.to_string(),
                };
                return Ok(0);
            }
            "set" => {
                if let Some(i) = rest.iter().position(|x| x == "format").filter(|&i| i > 0 && rest[i - 1] == "time") {
                    let f = match rest.get(i + 1).map(|s| s.as_str()) {
                        Some("milliseconds") | Some("ms") => MCI_FORMAT_MILLISECONDS,
                        Some("frames") => MCI_FORMAT_FRAMES,
                        Some("bytes") => MCI_FORMAT_BYTES,
                        Some("samples") => MCI_FORMAT_SAMPLES,
                        Some(_) => return Err(MCIERR_BAD_TIME_FORMAT),
                        None => return Err(MCIERR_MISSING_PARAMETER),
                    };
                    flags |= MCI_SET_TIME_FORMAT;
                    w32(p + ps, f);
                }
                MCI_SET
            }
            "setaudio" => {
                if let Some(i) = after("to").filter(|_| has("volume")) {
                    flags |= MCI_DGV_SETAUDIO_ITEM | MCI_DGV_SETAUDIO_VALUE;
                    w32(p + ps, MCI_DGV_SETAUDIO_VOLUME);
                    w32(p + ps + 4, int(w.get(i))?);
                }
                MCI_SETAUDIO
            }
            "window" => {
                if let Some(i) = after("handle") {
                    let h = if lw.get(i).map(|s| s.as_str()) == Some("default") { 0 } else { int(w.get(i))? };
                    flags |= MCI_DGV_WINDOW_HWND;
                    mem::wp(p + ps, h);
                }
                MCI_WINDOW
            }
            "put" => {
                if let Some(i) = after("at").filter(|_| has("destination")) {
                    flags |= MCI_DGV_PUT_DESTINATION | MCI_DGV_RECT;
                    for k in 0..4 {
                        let v = w.get(i + k).ok_or(MCIERR_MISSING_PARAMETER)?.parse::<i32>().map_err(|_| MCIERR_BAD_INTEGER)?;
                        w32(p + ps + 4 * k as u32, v as u32);
                    }
                }
                MCI_PUT
            }
            "where" => {
                flags |= MCI_DGV_WHERE_DESTINATION;
                let r = send_command(c, id, MCI_WHERE, flags, p, false);
                if r == 0 {
                    let v: Vec<String> = (0..4).map(|k| (r32(p + ps + 4 * k) as i32).to_string()).collect();
                    ret = v.join(" ");
                }
                return Ok(r);
            }
            "capability" => {
                let item = rest.join(" ");
                let (kind, video) = dev(id, |d| (d.kind, d.video())).unwrap_or((Kind::Mpeg, false));
                ret = match item.as_str() {
                    "can play" | "has audio" | "uses files" | "compound device" => "true".into(),
                    "can record" | "can eject" | "can save" => "false".into(),
                    "has video" | "can stretch" => if video { "true" } else { "false" }.into(),
                    "device type" => if kind == Kind::Wave { "waveaudio" } else { "digitalvideo" }.into(),
                    _ => return Err(MCIERR_UNRECOGNIZED_KEYWORD),
                };
                return Ok(0);
            }
            "info" => {
                let (kind, el) = dev(id, |d| (d.kind, d.element.clone())).unwrap_or((Kind::Mpeg, String::new()));
                ret = if has("file") { el } else { product(kind).into() };
                return Ok(0);
            }
            _ => return Err(MCIERR_UNRECOGNIZED_COMMAND),
        };
        Ok(send_command(c, id, msg, flags, p, false))
    })();
    mem::heap_free(p);
    match r {
        Ok(0) => Ok(ret),
        Ok(e) | Err(e) => Err(e),
    }
}

fn format_name(f: u32) -> &'static str {
    match f {
        MCI_FORMAT_FRAMES => "frames",
        MCI_FORMAT_BYTES => "bytes",
        MCI_FORMAT_SAMPLES => "samples",
        _ => "milliseconds",
    }
}

fn mci_send_string(c: &mut Ctx, wide: bool) -> u32 {
    let (cmd, ret, size, cb) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if cmd == 0 {
        return MCIERR_MISSING_STRING_ARGUMENT;
    }
    let s = crate::k32::arg_str(cmd, wide);
    match send_string(c, &s, cb) {
        Ok(text) => {
            if ret != 0 && size != 0 {
                crate::k32::ret_str(ret, size, &text, wide);
            }
            0
        }
        Err(e) => e,
    }
}
fn mciSendStringA(c: &mut Ctx) -> u32 {
    mci_send_string(c, false)
}
fn mciSendStringW(c: &mut Ctx) -> u32 {
    mci_send_string(c, true)
}

fn mci_get_error_string(c: &mut Ctx, wide: bool) -> u32 {
    let (e, buf, size) = (c.a(0), c.a(1), c.a(2));
    if buf == 0 || size == 0 {
        return 0;
    }
    match error_text(e) {
        Some(t) => {
            crate::k32::ret_str(buf, size, t, wide);
            1
        }
        None => {
            crate::k32::ret_str(buf, size, "", wide);
            0
        }
    }
}
fn mciGetErrorStringA(c: &mut Ctx) -> u32 {
    mci_get_error_string(c, false)
}
fn mciGetErrorStringW(c: &mut Ctx) -> u32 {
    mci_get_error_string(c, true)
}
fn mciGetDeviceIDA(c: &mut Ctx) -> u32 {
    device_id(&crate::k32::arg_str(c.a(0), false))
}
fn mciGetDeviceIDW(c: &mut Ctx) -> u32 {
    device_id(&crate::k32::arg_str(c.a(0), true))
}

// ---------------------------------------------------------------------------------------------
// MCIWnd (msvfw32)

const WM_USER: u32 = 0x400;
const MCIWNDM_GETDEVICEID: u32 = WM_USER + 100;
const MCIWNDM_GETPOSITIONA: u32 = WM_USER + 102;
const MCIWNDM_GETSTART: u32 = WM_USER + 103;
const MCIWNDM_GETLENGTH: u32 = WM_USER + 104;
const MCIWNDM_GETEND: u32 = WM_USER + 105;
const MCIWNDM_GETMODEA: u32 = WM_USER + 106;
const MCIWNDM_SETVOLUME: u32 = WM_USER + 110;
const MCIWNDM_GETVOLUME: u32 = WM_USER + 111;
const MCIWNDM_SETREPEAT: u32 = WM_USER + 114;
const MCIWNDM_GETREPEAT: u32 = WM_USER + 115;
const MCIWNDM_PLAYFROM: u32 = WM_USER + 122;
const MCIWNDM_PLAYTO: u32 = WM_USER + 123;
const MCIWNDM_CAN_PLAY: u32 = WM_USER + 144;
const MCIWNDM_CAN_WINDOW: u32 = WM_USER + 145;
const MCIWNDM_OPENA: u32 = WM_USER + 153;
const MCIWNDM_NOTIFYMODE: u32 = WM_USER + 200;
const MCIWNDM_GETPOSITIONW: u32 = WM_USER + 202;
const MCIWNDM_GETMODEW: u32 = WM_USER + 206;
const MCIWNDM_OPENW: u32 = WM_USER + 252;
/// (our own: the waiter tells the window the play ended, on the window's thread)
const WM_MCIWND_MODE: u32 = WM_USER + 0x3f0;

const MCIWNDF_NOAUTOSIZEWINDOW: u32 = 1;
const MCIWNDF_NOTIFYMODE: u32 = 0x100;

#[derive(Default)]
struct Wnd {
    dev: u32,
    style: u32,
    parent: u32,
    mode: u32,
    repeat: bool,
}

static WNDS: Mutex<Option<HashMap<u32, Wnd>>> = Mutex::new(None);
fn wnd<R>(h: u32, f: impl FnOnce(&mut Wnd) -> R) -> Option<R> {
    WNDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&h).map(f)
}

fn gcall(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let f = crate::api::export(dll, name);
    c.call(f, args)
}

/// The mode an MCIWnd shows: of its device, MCI_MODE_NOT_READY with none.
fn wnd_mode(h: u32) -> u32 {
    match wnd(h, |w| w.dev).unwrap_or(0) {
        0 => MCI_MODE_NOT_READY,
        d => mode(d),
    }
}

/// MCIWNDM_NOTIFYMODE to the parent when the mode changed (MCIWNDF_NOTIFYMODE).
fn wnd_notify(c: &mut Ctx, h: u32) {
    let m = wnd_mode(h);
    let Some((parent, style, was)) = wnd(h, |w| (w.parent, w.style, std::mem::replace(&mut w.mode, m))) else { return };
    if was != m && style & MCIWNDF_NOTIFYMODE != 0 && parent != 0 {
        crate::user::send(c, parent, MCIWNDM_NOTIFYMODE, h, m);
    }
}

fn wnd_open(c: &mut Ctx, h: u32, file: &str) -> u32 {
    if let Some(d) = wnd(h, |w| std::mem::take(&mut w.dev)) {
        if d != 0 {
            close(d);
        }
    }
    let id = match open(None, file, Some(&format!("mciwnd{h:x}"))).or_else(|e| if e == MCIERR_EXTENSION_NOT_FOUND { open(Some(Kind::Mpeg), file, Some(&format!("mciwnd{h:x}"))) } else { Err(e) }) {
        Ok(id) => id,
        Err(e) => {
            wnd_notify(c, h);
            return e;
        }
    };
    let style = wnd(h, |w| {
        w.dev = id;
        w.style
    })
    .unwrap_or(0);
    dev(id, |d| {
        d.hwnd = h;
        d.mciwnd = h;
    });
    // the window takes the movie's size (its client area: the frame stays)
    if style & MCIWNDF_NOAUTOSIZEWINDOW == 0 {
        if let Some(size) = dev(id, |d| match d.media { Media::Movie { size, .. } => Some(size), _ => None }).flatten() {
            if let Some(r) = crate::user::client_rect_screen(h) {
                let wr = mem::heap_alloc(16, true);
                gcall(c, "user32", "GetWindowRect", &[h, wr]);
                let (fw, fh) = ((r32(wr + 8) as i32 - r32(wr) as i32) - (r.r - r.l), (r32(wr + 12) as i32 - r32(wr + 4) as i32) - (r.b - r.t));
                mem::heap_free(wr);
                // SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE
                gcall(c, "user32", "SetWindowPos", &[h, 0, 0, 0, (size.0 as i32 + fw) as u32, (size.1 as i32 + fh) as u32, 0x2 | 0x4 | 0x10]);
            }
        }
    }
    sync_movie(id);
    wnd_notify(c, h);
    0
}

fn wnd_command(c: &mut Ctx, h: u32, msg: u32, from: Option<u32>, to: Option<u32>) -> u32 {
    let Some(d) = wnd(h, |w| w.dev).filter(|&d| d != 0) else { return MCIERR_INVALID_DEVICE_ID };
    let r = match msg {
        MCI_PLAY => {
            let repeat = wnd(h, |w| w.repeat).unwrap_or(false);
            let out = dev(d, |x| x.notes.command(None, true)).unwrap_or_default();
            post_all(out, d);
            match play_dev(d, from, to, repeat) {
                Ok(()) => {
                    arm(d);
                    0
                }
                Err(e) => e,
            }
        }
        MCI_STOP => {
            stop_dev(d, false);
            0
        }
        MCI_PAUSE => {
            if mode(d) == MCI_MODE_PLAY {
                stop_dev(d, true);
            }
            0
        }
        MCI_RESUME => {
            if dev(d, |x| x.paused()).unwrap_or(false) {
                dev(d, |x| x.gen &= 0x7fff_ffff);
                timeline(d, |tl, t| tl.run(t));
                sync_sound(d);
                arm(d);
            }
            0
        }
        MCI_SEEK => {
            let t = dev(d, |x| x.from_fmt(from.unwrap_or(0))).unwrap_or(0);
            seek_dev(d, t);
            0
        }
        MCI_CLOSE => {
            wnd(h, |w| w.dev = 0);
            close(d);
            0
        }
        _ => MCIERR_UNSUPPORTED_FUNCTION,
    };
    wnd_notify(c, h);
    r
}

fn mciwnd_proc(c: &mut Ctx) -> u32 {
    let (h, msg, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let devid = wnd(h, |w| w.dev).unwrap_or(0);
    match msg {
        // WM_CREATE: CREATESTRUCT's lpCreateParams is the file (or NULL)
        0x1 => {
            let cs = lp;
            // CREATESTRUCT { lpCreateParams, hInstance, hMenu, hwndParent, cy, cx, y, x, style, ... }
            let style = r32(cs + 4 * mem::ps() + 4 * 4);
            let parent = mem::rp(cs + 3 * mem::ps());
            let file = mem::rp(cs);
            WNDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(h, Wnd { style: style & 0xffff, parent, mode: MCI_MODE_NOT_READY, ..Wnd::default() });
            if file != 0 {
                let f = crate::k32::arg_str(file, true);
                wnd_open(c, h, &f);
            }
            0
        }
        MCIWNDM_OPENA | MCIWNDM_OPENW => {
            if lp == 0 {
                return MCIERR_UNSUPPORTED_FUNCTION;
            }
            let f = crate::k32::arg_str(lp, msg == MCIWNDM_OPENW);
            wnd_open(c, h, &f)
        }
        MCI_PLAY | MCI_STOP | MCI_PAUSE | MCI_RESUME | MCI_CLOSE => wnd_command(c, h, msg, None, None),
        MCI_SEEK => wnd_command(c, h, msg, Some(lp), None),
        MCIWNDM_PLAYFROM => wnd_command(c, h, MCI_PLAY, Some(lp), None),
        MCIWNDM_PLAYTO => wnd_command(c, h, MCI_PLAY, None, Some(lp)),
        WM_MCIWND_MODE => {
            wnd_notify(c, h);
            0
        }
        MCIWNDM_GETMODEA | MCIWNDM_GETMODEW => {
            let m = wnd_mode(h);
            if lp != 0 && wp != 0 {
                let s = match m {
                    MCI_MODE_PLAY => "playing",
                    MCI_MODE_PAUSE => "paused",
                    MCI_MODE_NOT_READY => "not ready",
                    _ => "stopped",
                };
                crate::k32::ret_str(lp, wp, s, msg == MCIWNDM_GETMODEW);
            }
            m
        }
        MCIWNDM_GETLENGTH | MCIWNDM_GETEND => dev(devid, |d| d.to_fmt(d.duration)).unwrap_or(0),
        MCIWNDM_GETSTART => 0,
        MCIWNDM_GETPOSITIONA | MCIWNDM_GETPOSITIONW => {
            let p = position(devid);
            dev(devid, |d| d.to_fmt(p)).unwrap_or(0)
        }
        MCIWNDM_GETDEVICEID => devid,
        MCIWNDM_CAN_PLAY => (devid != 0) as u32,
        MCIWNDM_CAN_WINDOW => dev(devid, |d| d.video()).unwrap_or(false) as u32,
        MCIWNDM_SETVOLUME => {
            set_volume(devid, volume_db(lp));
            0
        }
        MCIWNDM_GETVOLUME => dev(devid, |d| volume_mci(d.volume)).unwrap_or(0),
        MCIWNDM_SETREPEAT => {
            wnd(h, |w| w.repeat = lp != 0);
            dev(devid, |d| d.repeat = lp != 0);
            0
        }
        MCIWNDM_GETREPEAT => wnd(h, |w| w.repeat as u32).unwrap_or(0),
        // WM_SIZE / WM_MOVE: the movie follows the client area
        0x5 | 0x3 => {
            if devid != 0 {
                sync_movie(devid);
            }
            gcall(c, "user32", "DefWindowProcW", &[h, msg, wp, lp])
        }
        // WM_ERASEBKGND: black under the movie
        0x14 => {
            let rc = mem::heap_alloc(16, true);
            gcall(c, "user32", "GetClientRect", &[h, rc]);
            let br = gcall(c, "gdi32", "GetStockObject", &[4]);
            gcall(c, "user32", "FillRect", &[wp, rc, br]);
            mem::heap_free(rc);
            1
        }
        // WM_DESTROY: the device closes
        0x2 => {
            if devid != 0 {
                close(devid);
            }
            WNDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&h);
            0
        }
        _ => gcall(c, "user32", "DefWindowProcW", &[h, msg, wp, lp]),
    }
}

/// "MCIWndClass", registered once (MCIWndRegisterClass, MCIWndCreate).
fn register_class(c: &mut Ctx) -> u32 {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_none() {
        let proc_ = {
            static P: OnceLock<u32> = OnceLock::new();
            *P.get_or_init(|| crate::api::host_fn("mciwnd_proc", mciwnd_proc, 16))
        };
        let ps = mem::ps();
        // WNDCLASSW { style, lpfnWndProc, cbClsExtra, cbWndExtra, hInstance, hIcon, hCursor, hbrBackground, lpszMenuName, lpszClassName }
        let wc = mem::heap_alloc(80, true);
        let name: Vec<u16> = "MCIWndClass\0".encode_utf16().collect();
        let np = mem::heap_alloc(name.len() as u32 * 2, false);
        for (i, ch) in name.iter().enumerate() {
            mem::w16(np + 2 * i as u32, *ch);
        }
        // CS_DBLCLKS | CS_GLOBALCLASS
        w32(wc, 0x8 | 0x4000);
        let mut at = aligned(4);
        mem::wp(wc + at, proc_);
        at += ps;
        at += 8;
        at = aligned(at);
        for _ in 0..5 {
            at += ps;
        }
        mem::wp(wc + at, np);
        gcall(c, "user32", "RegisterClassW", &[wc]);
        mem::heap_free(wc);
        let _ = DONE.set(());
    }
    1
}

fn mciwnd_create(c: &mut Ctx, wide: bool) -> u32 {
    let (parent, inst, style, file) = (c.a(0), c.a(1), c.a(2), c.a(3));
    register_class(c);
    // the window's style: WS_CHILD with a parent, else an overlapped window; the MCIWNDF_ bits
    // ride in the low word, which the class reads back in WM_CREATE
    let ws = if style & 0xffff_0000 != 0 { style & 0xffff_0000 } else if parent != 0 { 0x5080_0000 } else { 0x10cf_0000 };
    let name: Vec<u16> = "MCIWndClass\0".encode_utf16().collect();
    let np = mem::heap_alloc(name.len() as u32 * 2, false);
    for (i, ch) in name.iter().enumerate() {
        mem::w16(np + 2 * i as u32, *ch);
    }
    // (the file as a UTF-16 string for the W window)
    let fp = if file != 0 {
        let f: Vec<u16> = crate::k32::arg_str(file, wide).encode_utf16().chain(std::iter::once(0)).collect();
        let p = mem::heap_alloc(f.len() as u32 * 2, false);
        for (i, ch) in f.iter().enumerate() {
            mem::w16(p + 2 * i as u32, *ch);
        }
        p
    } else {
        0
    };
    // CW_USEDEFAULT for a top-level one
    let (x, y, w, h) = if parent != 0 { (0, 0, 320, 240) } else { (0x8000_0000, 0x8000_0000, 320, 240) };
    let hwnd = gcall(c, "user32", "CreateWindowExW", &[0, np, 0, (ws & 0xffff_0000) | (style & 0xffff), x, y, w, h, parent, 0, inst, fp]);
    mem::heap_free(np);
    if fp != 0 {
        mem::heap_free(fp);
    }
    hwnd
}
fn MCIWndCreateA(c: &mut Ctx) -> u32 {
    mciwnd_create(c, false)
}
fn MCIWndCreateW(c: &mut Ctx) -> u32 {
    mciwnd_create(c, true)
}
fn MCIWndRegisterClass(c: &mut Ctx) -> u32 {
    register_class(c)
}

pub fn init() {
    for (n, f) in [
        ("mciSendCommandA", mciSendCommandA as Handler),
        ("mciSendCommandW", mciSendCommandW),
        ("mciSendStringA", mciSendStringA),
        ("mciSendStringW", mciSendStringW),
        ("mciGetErrorStringA", mciGetErrorStringA),
        ("mciGetErrorStringW", mciGetErrorStringW),
        ("mciGetDeviceIDA", mciGetDeviceIDA),
        ("mciGetDeviceIDW", mciGetDeviceIDW),
    ] {
        crate::api::implement("winmm", n, f);
    }
    for (n, f) in [("MCIWndCreateA", MCIWndCreateA as Handler), ("MCIWndCreate", MCIWndCreateA), ("MCIWndCreateW", MCIWndCreateW), ("MCIWndRegisterClass", MCIWndRegisterClass)] {
        crate::api::implement("msvfw32", n, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn devices_by_type_and_extension() {
        assert_eq!(kind_of_type("MPEGVideo"), Some(Ok(Kind::Mpeg)));
        assert_eq!(kind_of_type("waveaudio"), Some(Ok(Kind::Wave)));
        assert_eq!(kind_of_type("sequencer"), Some(Err(MCIERR_CANNOT_LOAD_DRIVER)));
        assert_eq!(kind_of_type("nonsense"), None);
        assert_eq!(kind_of_file("C:\\a\\b.WAV"), Some(Ok(Kind::Wave)));
        assert_eq!(kind_of_file("x.mpg"), Some(Ok(Kind::Mpeg)));
        assert_eq!(kind_of_file("x.avi"), Some(Ok(Kind::Avi)));
        // (no extension: the program must name the type: LiLiM's Data\Sound\se\lilim)
        assert_eq!(kind_of_file("Data\\Sound\\se\\lilim"), None);
        assert_eq!(kind_of_file("dir.d\\lilim"), None);
        assert_eq!(kind_of_file("x.mid"), Some(Err(MCIERR_CANNOT_LOAD_DRIVER)));
    }

    #[test]
    fn time_formats() {
        // 2.5 s: 2500 ms, 25 frames at 10 fps, 22050 samples at 8820 Hz, bytes of 4-byte frames
        let t = 25_000_000;
        assert_eq!(to_format(t, MCI_FORMAT_MILLISECONDS, 0, 0, (0, 1)), 2500);
        assert_eq!(to_format(t, MCI_FORMAT_FRAMES, 0, 0, (10, 1)), 25);
        assert_eq!(to_format(t, MCI_FORMAT_SAMPLES, 8820, 4, (0, 1)), 22050);
        assert_eq!(to_format(t, MCI_FORMAT_BYTES, 8820, 4, (0, 1)), 88200);
        assert!(format_ok(Kind::Wave, false, MCI_FORMAT_BYTES));
        assert!(!format_ok(Kind::Mpeg, false, MCI_FORMAT_BYTES));
        assert!(!format_ok(Kind::Mpeg, false, MCI_FORMAT_FRAMES));
        assert!(format_ok(Kind::Mpeg, true, MCI_FORMAT_FRAMES));
        assert!(!format_ok(Kind::Wave, false, 1));
    }

    #[test]
    fn volumes() {
        assert_eq!(volume_db(1000), 0);
        assert_eq!(volume_db(0), -10000);
        assert_eq!(volume_db(500), -602);
        assert_eq!(volume_mci(0), 1000);
        assert_eq!(volume_mci(-10000), 0);
    }

    #[test]
    fn command_words() {
        assert_eq!(words("open \"C:\\My Music\\a b.mp3\" type mpegvideo alias bgm").unwrap(), ["open", "C:\\My Music\\a b.mp3", "type", "mpegvideo", "alias", "bgm"]);
        assert_eq!(words("  play   bgm  from 0 notify ").unwrap(), ["play", "bgm", "from", "0", "notify"]);
        assert_eq!(words("open \"\" alias x").unwrap(), ["open", "", "alias", "x"]);
        assert_eq!(words("open \"x"), None);
    }

    #[test]
    fn notifications_as_windows() {
        let mut n = Notes::default();
        // play notify, then the end: successful
        n.wait_end(7);
        assert_eq!(n.end(), [(7, MCI_NOTIFY_SUCCESSFUL)]);
        assert_eq!(n.end(), []);
        // play notify, then stop without notify: aborted
        n.wait_end(7);
        assert_eq!(n.command(None, true), [(7, MCI_NOTIFY_ABORTED)]);
        // play notify, then status with notify: superseded
        n.wait_end(7);
        assert_eq!(n.command(Some(8), false), [(7, MCI_NOTIFY_SUPERSEDED)]);
        // play notify, then status without: still pending
        n.wait_end(7);
        assert_eq!(n.command(None, false), []);
        assert_eq!(n.pending, Some(7));
    }

    #[test]
    fn error_texts() {
        assert!(error_text(MCIERR_INVALID_DEVICE_NAME).unwrap().contains("not open"));
        assert!(error_text(MCIERR_FILE_NOT_FOUND).is_some());
        assert_eq!(error_text(1234), None);
    }

    proptest! {
        /// Every report owed goes out exactly once, whatever the commands: the pending one is
        /// either still pending or was reported (successful, superseded or aborted), never twice
        #[test]
        fn each_play_reported_at_most_once(ops in proptest::collection::vec((0u8..4, any::<bool>(), 1u32..5), 0..40)) {
            let mut n = Notes::default();
            let mut owed = 0i32;
            let mut sent = 0i32;
            for (op, flag, w) in ops {
                match op {
                    0 => {
                        // play (interrupts), with notify or not
                        sent += n.command(flag.then_some(w), true).len() as i32;
                        if flag { n.wait_end(w); owed += 1; }
                    }
                    1 => sent += n.command(flag.then_some(w), false).len() as i32,
                    2 => sent += n.command(None, true).len() as i32,
                    _ => sent += n.end().len() as i32,
                }
                prop_assert_eq!(owed, sent + n.pending.is_some() as i32);
            }
        }

        /// Formats round trip: a value in the format, to time and back, stays (for frames and
        /// milliseconds exactly; samples and bytes to the frame)
        #[test]
        fn formats_round_trip(v in 0u32..10_000_000, rate in prop::sample::select(vec![8000u32, 11025, 22050, 44100, 48000]), block in prop::sample::select(vec![1u32, 2, 4]), fps in 1u32..61) {
            prop_assert_eq!(to_format(from_format(v, MCI_FORMAT_MILLISECONDS, rate, block, (fps, 1)), MCI_FORMAT_MILLISECONDS, rate, block, (fps, 1)), v);
            prop_assert_eq!(to_format(from_format(v, MCI_FORMAT_FRAMES, rate, block, (fps, 1)), MCI_FORMAT_FRAMES, rate, block, (fps, 1)), v);
            let s = to_format(from_format(v, MCI_FORMAT_SAMPLES, rate, block, (fps, 1)), MCI_FORMAT_SAMPLES, rate, block, (fps, 1));
            prop_assert!(v - s <= 1, "{} {}", v, s);
            let b = to_format(from_format(v, MCI_FORMAT_BYTES, rate, block, (fps, 1)), MCI_FORMAT_BYTES, rate, block, (fps, 1));
            prop_assert!(v - b <= 2 * block, "{} {}", v, b);
        }

        /// The volume map is monotonic and stays in range
        #[test]
        fn volume_monotonic(a in 0u32..=1000, b in 0u32..=1000) {
            if a <= b { prop_assert!(volume_db(a) <= volume_db(b)); }
            prop_assert!((-10000..=0).contains(&volume_db(a)));
            prop_assert!(volume_mci(volume_db(a)).abs_diff(a) <= 1 || a < 10);
        }

        /// Any command string splits without panicking, and unquoted words keep their text
        #[test]
        fn words_never_panic(s in "[ a-z0-9\"\\\\.]{0,40}") {
            if let Some(w) = words(&s) {
                if !s.contains('"') {
                    prop_assert_eq!(w, s.split_whitespace().map(String::from).collect::<Vec<_>>());
                }
            }
        }
    }
}
