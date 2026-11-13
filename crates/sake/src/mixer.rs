//! winmm's mixer API (mixerGetNumDevs .. mixerSetControlDetails) as Windows Vista and later
//! have it: one mixer for the one waveOut device (no capture device), with one destination
//! (Speakers, line 0xFFFF0000) and one source (Wave out, line 0), each with a volume and a mute
//! control. The destination's controls (ids 0, 1) are the program's own session volume, as on
//! Windows: they scale everything sound.rs plays (waveOut and DirectSound). The source's
//! (ids 2, 3) answer but change nothing, as in Wine. The answers follow Wine's winmm (its
//! mixer tests, tests/winetest) and Windows where they differ (CALLBACK_THREAD / EVENT opens).
//!
//! Games ask for it before they play: Asd4 (しすたぁボーイ体験版) asked "ボリュームコントロールを
//! 初期化する事ができませんでした" when there was no mixer, and fades its music through it.

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicU32, Ordering};

use crate::api::{implement, Ctx};
use crate::mem::{self, ps, r32, w16, w32};

pub(crate) const NOERROR: u32 = 0;
pub(crate) const ERROR: u32 = 1;
pub(crate) const BADDEVICEID: u32 = 2;
pub(crate) const INVALHANDLE: u32 = 5;
pub(crate) const NOTSUPPORTED: u32 = 8;
pub(crate) const INVALFLAG: u32 = 10;
pub(crate) const INVALPARAM: u32 = 11;
pub(crate) const INVALLINE: u32 = 1024;
pub(crate) const INVALCONTROL: u32 = 1025;

/// the destination line (Speakers) and the source (Wave out)
pub(crate) const DEST_LINE: u32 = 0xFFFF_0000;
pub(crate) const SRC_LINE: u32 = 0;
const DST_SPEAKERS: u32 = 4;
const SRC_WAVEOUT: u32 = 0x1008;
const TARGET_WAVEOUT: u32 = 1;
const LINEF_ACTIVE: u32 = 1;
const LINEF_SOURCE: u32 = 0x8000_0000;
pub(crate) const CONTROL_VOLUME: u32 = 0x5003_0001;
pub(crate) const CONTROL_MUTE: u32 = 0x2001_0002;
const CONTROLF_UNIFORM: u32 = 1;

/// The handle mixerOpen gives (one device; the low bits count opens, as Wine's do)
const HMIXER_BASE: u32 = 0x00C0_0000;

/// MIXER_OBJECTF_* in the high nibble of the flags
const OBJ_MIXER: u32 = 0;
const OBJ_HMIXER: u32 = 0x8000_0000;
const OBJ_WAVEOUT: u32 = 0x1000_0000;
const OBJ_HWAVEOUT: u32 = 0x9000_0000;

/// The device name (as waveOutGetDevCaps gives it)
pub(crate) const NAME: &str = "sake WebAudio";

// ---------------------------------------------------------------------------------------------
// the model (no guest memory): what each call answers

/// A line, as MIXERLINE has it (the strings by kind)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    pub dest: u32,
    pub source: u32,
    pub id: u32,
    pub flags: u32,
    pub component: u32,
    pub channels: u32,
    pub connections: u32,
    pub controls: u32,
    pub target_type: u32,
    /// the target's name is the device's (Wave out) or empty (Speakers)
    pub target_named: bool,
}

pub(crate) fn destination() -> Line {
    Line { dest: 0, source: u32::MAX, id: DEST_LINE, flags: LINEF_ACTIVE, component: DST_SPEAKERS, channels: 2, connections: 1, controls: 2, target_type: 0, target_named: false }
}
pub(crate) fn source() -> Line {
    // (volume and mute act on every channel: one channel, as Wine says)
    Line { dest: 0, source: 0, id: SRC_LINE, flags: LINEF_ACTIVE | LINEF_SOURCE, component: SRC_WAVEOUT, channels: 1, connections: 0, controls: 2, target_type: TARGET_WAVEOUT, target_named: true }
}

/// What mixerGetLineInfo asks: MIXER_GETLINEINFOF_* (low 4 bits of the flags) and the fields
/// of the caller's MIXERLINE it reads
#[derive(Clone, Copy, Debug)]
pub(crate) struct LineQuery {
    pub dest: u32,
    pub source: u32,
    pub id: u32,
    pub component: u32,
}

/// mixerGetLineInfo's answer for a valid mixer: the line, or the error
pub(crate) fn line_info(flags: u32, q: LineQuery) -> Result<Line, u32> {
    // DESTINATION 0, SOURCE 1, LINEID 2, COMPONENTTYPE 3, TARGETTYPE 4
    match flags & 0xF {
        0 if q.dest == 0 => Ok(destination()),
        1 if q.source == 0 => Ok(source()),
        2 if q.id == DEST_LINE => Ok(destination()),
        2 if q.id == SRC_LINE => Ok(source()),
        3 if q.component == DST_SPEAKERS => Ok(destination()),
        3 if q.component == SRC_WAVEOUT => Ok(source()),
        0..=4 => Err(INVALLINE),
        _ => Err(INVALFLAG),
    }
}

/// mixerGetLineInfo's flags beyond the query and the object kinds
pub(crate) fn line_flags_ok(flags: u32) -> bool {
    flags & !(0xF | 0xF000_0000) == 0 && flags & 0xF <= 4
}

/// A control, as MIXERCONTROL has it (names by kind)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Control {
    pub id: u32,
    pub kind: u32,
    pub min: u32,
    pub max: u32,
    pub steps: u32,
}

pub(crate) fn volume_control(line: u32) -> Control {
    Control { id: if line == DEST_LINE { 0 } else { 2 }, kind: CONTROL_VOLUME, min: 0, max: 0xFFFF, steps: 192 }
}
pub(crate) fn mute_control(line: u32) -> Control {
    Control { id: if line == DEST_LINE { 1 } else { 3 }, kind: CONTROL_MUTE, min: 0, max: 1, steps: 0 }
}

/// What mixerGetLineControls asks: ALL 0, ONEBYID 1, ONEBYTYPE 2 (low 4 bits) and the caller's
/// MIXERLINECONTROLS fields
#[derive(Clone, Copy, Debug)]
pub(crate) struct ControlsQuery {
    pub line: u32,
    pub id_or_type: u32,
    pub count: u32,
    pub size_ok: bool,
}

/// mixerGetLineControls' answer for a valid mixer
pub(crate) fn line_controls(flags: u32, q: ControlsQuery) -> Result<Vec<Control>, u32> {
    let want = match flags & 0xF {
        0 => 2,
        1 | 2 => 1,
        _ => return Err(NOTSUPPORTED),
    };
    if q.count != want || !q.size_ok {
        return Err(INVALPARAM);
    }
    if q.line != DEST_LINE && q.line != SRC_LINE {
        return Err(INVALLINE);
    }
    match flags & 0xF {
        0 => Ok(vec![volume_control(q.line), mute_control(q.line)]),
        // (by id: 0 and 1 name the line's volume and mute, whichever line; Wine's answer)
        1 if q.id_or_type == 0 => Ok(vec![volume_control(q.line)]),
        1 if q.id_or_type == 1 => Ok(vec![mute_control(q.line)]),
        2 if q.id_or_type == CONTROL_VOLUME => Ok(vec![volume_control(q.line)]),
        2 if q.id_or_type == CONTROL_MUTE => Ok(vec![mute_control(q.line)]),
        _ => Err(NOTSUPPORTED),
    }
}

/// mixerGetLineControls' flags: the query and the mixer object kinds
pub(crate) fn controls_flags_ok(flags: u32) -> bool {
    flags & !(0xF | OBJ_HMIXER) == 0
}

/// The session volume the destination's controls are: a level 0..1 (stored as f32 bits, so the
/// mixer thread reads it without a lock) and the mute.
pub(crate) struct Session {
    level: AtomicU32,
    mute: AtomicU32,
}

impl Session {
    pub(crate) const fn new() -> Session {
        Session { level: AtomicU32::new(0x3f80_0000), mute: AtomicU32::new(0) }
    }
    pub(crate) fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    pub(crate) fn muted(&self) -> bool {
        self.mute.load(Ordering::Relaxed) != 0
    }
    /// The factor everything played is scaled by
    pub(crate) fn gain(&self) -> f32 {
        if self.muted() { 0.0 } else { self.level() }
    }
    /// mixerGetControlDetails: the control's value (None: a source control, which writes nothing)
    pub(crate) fn get(&self, id: u32) -> Result<Option<u32>, u32> {
        match id {
            // (as Wine and Windows: the level times 0xFFFF, truncated)
            0 => Ok(Some((self.level() * 65535.0) as u32)),
            1 => Ok(Some(self.muted() as u32)),
            2 | 3 => Ok(None),
            _ => Err(INVALCONTROL),
        }
    }
    /// mixerSetControlDetails with the control's first value
    pub(crate) fn set(&self, id: u32, v: u32) -> Result<(), u32> {
        match id {
            0 if v > 0xFFFF => Err(INVALPARAM),
            0 => {
                self.level.store((v as f32 / 65535.0).to_bits(), Ordering::Relaxed);
                Ok(())
            }
            1 => {
                self.mute.store((v != 0) as u32, Ordering::Relaxed);
                Ok(())
            }
            2 | 3 => Ok(()),
            _ => Err(INVALCONTROL),
        }
    }
}

/// The program's session, as the destination's controls see it
pub(crate) static SESSION: Session = Session::new();

/// Which mixer a call names: hmix by the flags' object kind. True for our one device: id 0 as a
/// mixer or waveOut device id, a handle mixerOpen gave, an open waveOut handle.
pub(crate) fn names_device(hmix: u32, flags: u32, is_wave: impl Fn(u32) -> bool) -> bool {
    let handle = |h: u32| h & 0xFFFF_0000 == HMIXER_BASE;
    match flags & 0xF000_0000 {
        // (not a device id: a handle, as Windows accepts too)
        OBJ_MIXER => hmix == 0 || handle(hmix) || is_wave(hmix),
        OBJ_HMIXER | OBJ_HWAVEOUT => handle(hmix) || is_wave(hmix),
        OBJ_WAVEOUT => hmix == 0,
        _ => false,
    }
}

/// mixerOpen's checks: the device id and the callback kind (CALLBACK_TYPEMASK 0x70000)
pub(crate) fn open_check(id: u32, callback: u32, flags: u32, is_window: impl Fn(u32) -> bool) -> u32 {
    match flags & 0x7_0000 {
        // CALLBACK_WINDOW: a window or none
        0x1_0000 if callback != 0 && !is_window(callback) => return INVALPARAM,
        // CALLBACK_FUNCTION: not for mixers
        0x3_0000 => return INVALFLAG,
        _ => {}
    }
    if id != 0 {
        return BADDEVICEID;
    }
    NOERROR
}

// ---------------------------------------------------------------------------------------------
// the API on guest memory

static OPENS: AtomicU32 = AtomicU32::new(0);

fn is_wave(h: u32) -> bool {
    crate::sound::is_wave_handle(h)
}

fn mixerGetNumDevs(_: &mut Ctx) -> u32 {
    1
}

/// MIXERCAPS: wMid, wPid, vDriverVersion, szPname[32], fdwSupport, cDestinations
fn caps(c: &mut Ctx, wide: bool) -> u32 {
    let (id, p, size) = (c.a(0), c.a(1), c.a(2));
    if p == 0 {
        return INVALPARAM;
    }
    if size == 0 {
        return NOERROR;
    }
    if id != 0 && !names_device(id, OBJ_HMIXER, is_wave) {
        return BADDEVICEID;
    }
    let name_len = if wide { 64 } else { 32 };
    let mut b = vec![0u8; 8 + name_len + 8];
    b[0] = 0xFF;
    b[2] = 0xFF;
    b[4..8].copy_from_slice(&0x0001_0001u32.to_le_bytes());
    put_name(&mut b[8..8 + name_len], NAME, wide);
    b[8 + name_len + 4..].copy_from_slice(&1u32.to_le_bytes());
    let n = size.min(b.len() as u32) as usize;
    mem::write(p, &b[..n]);
    NOERROR
}

fn put_name(dst: &mut [u8], s: &str, wide: bool) {
    if wide {
        for (i, u) in s.encode_utf16().take(dst.len() / 2 - 1).enumerate() {
            dst[2 * i..2 * i + 2].copy_from_slice(&u.to_le_bytes());
        }
    } else {
        for (i, ch) in s.bytes().take(dst.len() - 1).enumerate() {
            dst[i] = ch;
        }
    }
}

/// mixerOpen(HMIXER*, id, callback, instance, flags)
fn mixerOpen(c: &mut Ctx) -> u32 {
    let (out, id, cb, flags) = (c.a(0), c.a(1), c.a(2), c.a(4));
    if out == 0 {
        return INVALPARAM;
    }
    let r = open_check(id, cb, flags, crate::user::is_window);
    if r != NOERROR {
        return r;
    }
    mem::wp(out, HMIXER_BASE | (OPENS.fetch_add(1, Ordering::Relaxed) & 0xFFFF));
    NOERROR
}
fn mixerClose(_: &mut Ctx) -> u32 {
    NOERROR
}
/// mixerGetID(hmix, UINT*, flags)
fn mixerGetID(c: &mut Ctx) -> u32 {
    let (h, out, flags) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return INVALPARAM;
    }
    if !names_device(h, flags, is_wave) {
        return INVALHANDLE;
    }
    w32(out, 0);
    NOERROR
}
fn mixerMessage(_: &mut Ctx) -> u32 {
    NOTSUPPORTED
}

/// MIXERLINE offsets (mmsystem.h packs to 1; dwUser is a DWORD_PTR): (user, rest, size) where
/// rest is dwComponentType's offset
struct LineAt {
    user: u32,
    comp: u32,
    short: u32,
    name: u32,
    target: u32,
    size: u32,
    wide: bool,
}
fn line_at(wide: bool) -> LineAt {
    line_at_for(wide, ps())
}
fn line_at_for(wide: bool, ps: u32) -> LineAt {
    let comp = 20 + ps;
    let ch = if wide { 2 } else { 1 };
    let short = comp + 16;
    let name = short + 16 * ch;
    let target = name + 64 * ch;
    LineAt { user: 20, comp, short, name, target, size: target + 16 + 32 * ch, wide }
}

fn put_str(at: u32, chars: u32, s: &str, wide: bool) {
    let mut b = vec![0u8; (chars * if wide { 2 } else { 1 }) as usize];
    put_name(&mut b, s, wide);
    mem::write(at, &b);
}

/// mixerGetLineInfo(hmix, MIXERLINE*, flags)
fn line_info_api(c: &mut Ctx, wide: bool) -> u32 {
    let (h, p, flags) = (c.a(0), c.a(1), c.a(2));
    let at = line_at(wide);
    // (the A form wants its exact size; the W form at least its own)
    if p == 0 || if wide { r32(p) < at.size } else { r32(p) != at.size } {
        return INVALPARAM;
    }
    if !line_flags_ok(flags) {
        return INVALFLAG;
    }
    if !names_device(h, flags, is_wave) {
        return INVALHANDLE;
    }
    mem::wp(p + at.user, 0);
    let q = LineQuery { dest: r32(p + 4), source: r32(p + 8), id: r32(p + 12), component: r32(p + at.comp) };
    match line_info(flags, q) {
        Ok(l) => {
            put_line(p, &at, &l);
            NOERROR
        }
        Err(e) => e,
    }
}

fn put_line(p: u32, at: &LineAt, l: &Line) {
    w32(p + 4, l.dest);
    w32(p + 8, l.source);
    w32(p + 12, l.id);
    w32(p + 16, l.flags);
    w32(p + at.comp, l.component);
    w32(p + at.comp + 4, l.channels);
    w32(p + at.comp + 8, l.connections);
    w32(p + at.comp + 12, l.controls);
    put_str(p + at.short, 16, "Volume", at.wide);
    put_str(p + at.name, 64, "Master Volume", at.wide);
    // Target: dwType, dwDeviceID, wMid, wPid, vDriverVersion, szPname[32]
    let t = p + at.target;
    w32(t, l.target_type);
    w32(t + 4, 0);
    w16(t + 8, 0xFFFF);
    w16(t + 10, 0xFFFF);
    w32(t + 12, 0);
    put_str(t + 16, 32, if l.target_named { NAME } else { "" }, at.wide);
}

/// MIXERLINECONTROLS: cbStruct, dwLineID, dwControlID/Type, cControls, cbmxctrl, pamxctrl (packed)
fn lines_controls_size() -> u32 {
    20 + ps()
}
#[cfg(test)]
fn lines_controls_size_for(ps: u32) -> u32 {
    20 + ps
}
/// MIXERCONTROL: cbStruct, dwControlID, dwControlType, fdwControl, cMultipleItems,
/// szShortName[16], szName[64], Bounds (6 DWORDs), Metrics (6 DWORDs)
fn control_size(wide: bool) -> u32 {
    20 + 80 * if wide { 2 } else { 1 } + 48
}

/// mixerGetLineControls(hmix, MIXERLINECONTROLS*, flags)
fn line_controls_api(c: &mut Ctx, wide: bool) -> u32 {
    let (h, p, flags) = (c.a(0), c.a(1), c.a(2));
    let size = lines_controls_size();
    if !controls_flags_ok(flags) {
        // (the A form checks the structure first)
        if !wide && (p == 0 || r32(p) != size) {
            return INVALPARAM;
        }
        return INVALFLAG;
    }
    if p == 0 || if wide { r32(p) < size } else { r32(p) != size } {
        return INVALPARAM;
    }
    let (cb, arr) = (r32(p + 16), mem::rp(p + 20));
    let csize = control_size(wide);
    if wide && arr == 0 || !wide && cb != csize {
        return INVALPARAM;
    }
    if !names_device(h, flags, is_wave) {
        return INVALHANDLE;
    }
    // (ONEBYTYPE through the A form: one control whatever cControls says, as Windows does)
    let count = if !wide && flags & 0xF == 2 { 1 } else { r32(p + 12) };
    let q = ControlsQuery { line: r32(p + 4), id_or_type: r32(p + 8), count, size_ok: cb >= csize };
    match line_controls(flags, q) {
        Ok(v) => {
            for (i, ctl) in v.iter().enumerate() {
                put_control(arr + i as u32 * cb, ctl, wide);
            }
            NOERROR
        }
        Err(e) => e,
    }
}

fn put_control(p: u32, ctl: &Control, wide: bool) {
    mem::fill(p, control_size(wide), 0);
    w32(p, control_size(wide));
    w32(p + 4, ctl.id);
    w32(p + 8, ctl.kind);
    w32(p + 12, CONTROLF_UNIFORM);
    let ch = if wide { 2 } else { 1 };
    let name = if ctl.kind == CONTROL_VOLUME { "Volume" } else { "Mute" };
    put_str(p + 20, 16, name, wide);
    put_str(p + 20 + 16 * ch, 64, name, wide);
    let bounds = p + 20 + 80 * ch;
    w32(bounds, ctl.min);
    w32(bounds + 4, ctl.max);
    w32(bounds + 24, ctl.steps);
}

/// MIXERCONTROLDETAILS: cbStruct, dwControlID, cChannels, hwndOwner/cMultipleItems (a pointer),
/// cbDetails, paDetails (packed)
fn details_at() -> (u32, u32, u32) {
    details_at_for(ps())
}
fn details_at_for(ps: u32) -> (u32, u32, u32) {
    (12 + ps, 16 + ps, 16 + 2 * ps)
}

/// mixerGetControlDetails(hmix, MIXERCONTROLDETAILS*, flags)
fn get_details(c: &mut Ctx, wide: bool) -> u32 {
    let (h, p, flags) = (c.a(0), c.a(1), c.a(2));
    let (cb_at, pa_at, size) = details_at();
    if p == 0 || !wide && r32(p) != size {
        return INVALPARAM;
    }
    // MIXER_GETCONTROLDETAILSF_VALUE 0, LISTTEXT 1
    if !wide && flags & 0xF > 1 {
        return NOTSUPPORTED;
    }
    let (cb, pa) = (r32(p + cb_at), mem::rp(p + pa_at));
    if pa == 0 {
        return INVALPARAM;
    }
    if !names_device(h, flags, is_wave) {
        return INVALHANDLE;
    }
    let id = r32(p + 4);
    if id <= 1 && cb < 4 {
        return INVALPARAM;
    }
    match SESSION.get(id) {
        Ok(Some(v)) => {
            w32(pa, v);
            NOERROR
        }
        Ok(None) => NOERROR,
        Err(e) => e,
    }
}

/// mixerSetControlDetails(hmix, MIXERCONTROLDETAILS*, flags)
fn mixerSetControlDetails(c: &mut Ctx) -> u32 {
    let (h, p, flags) = (c.a(0), c.a(1), c.a(2));
    let (cb_at, pa_at, _) = details_at();
    // MIXER_SETCONTROLDETAILSF_CUSTOM
    if flags & 0xF == 1 {
        return NOTSUPPORTED;
    }
    if p == 0 {
        return INVALPARAM;
    }
    if !names_device(h, flags, is_wave) {
        return INVALHANDLE;
    }
    let (id, cb, pa) = (r32(p + 4), r32(p + cb_at), mem::rp(p + pa_at));
    if id <= 1 && (pa == 0 || cb < 4) {
        return INVALPARAM;
    }
    let v = if pa != 0 { r32(pa) } else { 0 };
    match SESSION.set(id, v) {
        Ok(()) => NOERROR,
        Err(e) => e,
    }
}

pub fn init() {
    let _ = ERROR;
    implement("winmm", "mixerGetNumDevs", mixerGetNumDevs);
    implement("winmm", "mixerGetDevCapsA", |c| caps(c, false));
    implement("winmm", "mixerGetDevCapsW", |c| caps(c, true));
    implement("winmm", "mixerOpen", mixerOpen);
    implement("winmm", "mixerClose", mixerClose);
    implement("winmm", "mixerGetID", mixerGetID);
    implement("winmm", "mixerMessage", mixerMessage);
    implement("winmm", "mixerGetLineInfoA", |c| line_info_api(c, false));
    implement("winmm", "mixerGetLineInfoW", |c| line_info_api(c, true));
    implement("winmm", "mixerGetLineControlsA", |c| line_controls_api(c, false));
    implement("winmm", "mixerGetLineControlsW", |c| line_controls_api(c, true));
    implement("winmm", "mixerGetControlDetailsA", |c| get_details(c, false));
    implement("winmm", "mixerGetControlDetailsW", |c| get_details(c, true));
    implement("winmm", "mixerSetControlDetails", mixerSetControlDetails);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn q(dest: u32, source: u32, id: u32, component: u32) -> LineQuery {
        LineQuery { dest, source, id, component }
    }

    #[test]
    fn lines_by_each_query() {
        assert_eq!(line_info(0, q(0, 9, 9, 9)), Ok(destination()));
        assert_eq!(line_info(0, q(1, 0, 0, 0)), Err(INVALLINE));
        assert_eq!(line_info(1, q(0, 0, 9, 9)), Ok(source()));
        assert_eq!(line_info(1, q(0, 1, 0, 0)), Err(INVALLINE));
        assert_eq!(line_info(2, q(9, 9, DEST_LINE, 9)), Ok(destination()));
        assert_eq!(line_info(2, q(9, 9, SRC_LINE, 9)), Ok(source()));
        assert_eq!(line_info(2, q(9, 9, 5, 9)), Err(INVALLINE));
        assert_eq!(line_info(3, q(9, 9, 9, DST_SPEAKERS)), Ok(destination()));
        assert_eq!(line_info(3, q(9, 9, 9, SRC_WAVEOUT)), Ok(source()));
        // a capture device's lines: there is none
        assert_eq!(line_info(3, q(9, 9, 9, 7)), Err(INVALLINE));
        assert_eq!(line_info(3, q(9, 9, 9, 0x1003)), Err(INVALLINE));
        // TARGETTYPE: not answered (as Wine)
        assert_eq!(line_info(4, q(0, 0, 0, 0)), Err(INVALLINE));
        assert!(!line_flags_ok(0xFFFF_FFFF));
        assert!(line_flags_ok(0x8000_0003));
    }

    #[test]
    fn destination_and_source_agree() {
        let (d, s) = (destination(), source());
        assert_eq!(d.connections, 1);
        assert_eq!(s.dest, d.dest);
        assert_eq!(s.flags & LINEF_SOURCE, LINEF_SOURCE);
        assert_eq!(d.flags & LINEF_SOURCE, 0);
        assert_eq!((d.controls, s.controls), (2, 2));
    }

    #[test]
    fn controls_of_each_line() {
        let all = |line| line_controls(0, ControlsQuery { line, id_or_type: 0, count: 2, size_ok: true });
        assert_eq!(all(DEST_LINE).unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(all(SRC_LINE).unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(all(7), Err(INVALLINE));
        // ALL wants exactly the line's 2 controls
        assert_eq!(line_controls(0, ControlsQuery { line: DEST_LINE, id_or_type: 0, count: 1, size_ok: true }), Err(INVALPARAM));
        assert_eq!(line_controls(0, ControlsQuery { line: DEST_LINE, id_or_type: 0, count: 2, size_ok: false }), Err(INVALPARAM));
        let by_type = |t| line_controls(2, ControlsQuery { line: DEST_LINE, id_or_type: t, count: 1, size_ok: true });
        assert_eq!(by_type(CONTROL_MUTE).unwrap()[0], mute_control(DEST_LINE));
        assert_eq!(by_type(0x5003_0002), Err(NOTSUPPORTED));
        let by_id = |id| line_controls(1, ControlsQuery { line: SRC_LINE, id_or_type: id, count: 1, size_ok: true });
        assert_eq!(by_id(0).unwrap()[0], volume_control(SRC_LINE));
        assert_eq!(by_id(2), Err(NOTSUPPORTED));
    }

    #[test]
    fn session_controls() {
        let s = Session::new();
        assert_eq!(s.get(0), Ok(Some(0xFFFF)));
        assert_eq!(s.get(1), Ok(Some(0)));
        assert_eq!(s.gain(), 1.0);
        assert_eq!(s.set(0, 0x10000), Err(INVALPARAM));
        assert_eq!(s.set(0, 0x8000), Ok(()));
        assert!((s.gain() - 0.5).abs() < 1e-4);
        assert_eq!(s.set(1, 5), Ok(()));
        assert_eq!(s.get(1), Ok(Some(1)));
        assert_eq!(s.gain(), 0.0);
        assert_eq!(s.set(1, 0), Ok(()));
        // source controls answer and change nothing
        assert_eq!(s.set(2, 0), Ok(()));
        assert_eq!(s.get(3), Ok(None));
        assert!((s.gain() - 0.5).abs() < 1e-4);
        assert_eq!(s.get(4), Err(INVALCONTROL));
        assert_eq!(s.set(9, 0), Err(INVALCONTROL));
    }

    #[test]
    fn opens_and_objects() {
        let no_window = |_| false;
        assert_eq!(open_check(0, 0, 0, no_window), NOERROR);
        assert_eq!(open_check(1, 0, 0, no_window), BADDEVICEID);
        assert_eq!(open_check(u32::MAX, 0, 0, no_window), BADDEVICEID);
        assert_eq!(open_check(0, 0, 0x3_0000, no_window), INVALFLAG);
        assert_eq!(open_check(0, 0xdead_beef, 0x1_0000, no_window), INVALPARAM);
        assert_eq!(open_check(0, 0, 0x1_0000, no_window), NOERROR);
        assert_eq!(open_check(0, 0x1234, 0x1_0000, |h| h == 0x1234), NOERROR);
        // CALLBACK_THREAD / EVENT: accepted since Windows 2000
        assert_eq!(open_check(0, 0, 0x2_0000, no_window), NOERROR);
        assert_eq!(open_check(0, 0, 0x5_0000, no_window), NOERROR);
        let no_wave = |_| false;
        assert!(names_device(0, OBJ_MIXER, no_wave));
        assert!(names_device(HMIXER_BASE | 3, OBJ_MIXER, no_wave));
        assert!(names_device(HMIXER_BASE, OBJ_HMIXER, no_wave));
        assert!(!names_device(0, OBJ_HMIXER, no_wave));
        assert!(names_device(0x77, OBJ_HWAVEOUT, |h| h == 0x77));
        assert!(names_device(0, OBJ_WAVEOUT, no_wave));
        assert!(!names_device(1, OBJ_WAVEOUT, no_wave));
        // waveIn / MIDI objects: no such device here
        assert!(!names_device(0, 0x2000_0000, no_wave));
        assert!(!names_device(0, 0x3000_0000, no_wave));
    }

    #[test]
    fn layouts() {
        // sizeof(MIXERLINEA/W) on x86 (mmsystem.h, pack 1): 168 / 280
        assert_eq!(line_at_for(false, 4).size, 168);
        assert_eq!(line_at_for(true, 4).size, 280);
        assert_eq!(lines_controls_size_for(4), 24);
        assert_eq!(details_at_for(4), (16, 20, 24));
        // x64: dwUser and the pointers are 8 bytes, still packed
        assert_eq!(line_at_for(false, 8).size, 172);
        assert_eq!(line_at_for(true, 8).size, 284);
        assert_eq!(lines_controls_size_for(8), 28);
        assert_eq!(details_at_for(8), (20, 24, 32));
        assert_eq!((control_size(false), control_size(true)), (148, 228));
    }

    proptest! {
        /// every query answers a line or one of its errors, never a line for a bad flag
        #[test]
        fn line_queries(flags in any::<u32>(), dest in any::<u32>(), src in any::<u32>(), id in any::<u32>(), comp in any::<u32>()) {
            match line_info(flags, q(dest, src, id, comp)) {
                Ok(l) => {
                    prop_assert!(flags & 0xF <= 3);
                    prop_assert!(l == destination() || l == source());
                    if flags & 0xF == 2 { prop_assert_eq!(l.id, id); }
                    if flags & 0xF == 3 { prop_assert_eq!(l.component, comp); }
                }
                Err(e) => prop_assert!(e == INVALLINE || e == INVALFLAG),
            }
        }

        /// a control asked for is one of the line's, with its type
        #[test]
        fn control_queries(flags in 0u32..4, line in prop_oneof![Just(DEST_LINE), Just(SRC_LINE), any::<u32>()],
                           what in prop_oneof![0u32..4, Just(CONTROL_VOLUME), Just(CONTROL_MUTE), any::<u32>()], count in 0u32..4) {
            if let Ok(v) = line_controls(flags, ControlsQuery { line, id_or_type: what, count, size_ok: true }) {
                prop_assert_eq!(v.len() as u32, count);
                for c in &v {
                    prop_assert!(*c == volume_control(line) || *c == mute_control(line));
                    if flags == 2 { prop_assert_eq!(c.kind, what); }
                }
            }
        }

        /// set then get gives the value back within 1 (Wine's mixer test allows that), and the
        /// gain is the level unless muted
        #[test]
        fn volume_round_trip(v in 0u32..=0xFFFF, m in any::<bool>()) {
            let s = Session::new();
            s.set(0, v).unwrap();
            s.set(1, m as u32).unwrap();
            let got = s.get(0).unwrap().unwrap();
            prop_assert!(got.abs_diff(v) <= 1, "{} -> {}", v, got);
            prop_assert!(got <= 0xFFFF);
            prop_assert_eq!(s.gain(), if m { 0.0 } else { v as f32 / 65535.0 });
            prop_assert!((0.0..=1.0).contains(&s.gain()));
        }

        #[test]
        fn volume_out_of_range(v in 0x10000u32..) {
            let s = Session::new();
            s.set(0, 0x1234).unwrap();
            prop_assert_eq!(s.set(0, v), Err(INVALPARAM));
            prop_assert_eq!(s.get(0).unwrap().unwrap().abs_diff(0x1234) <= 1, true);
        }
    }
}
