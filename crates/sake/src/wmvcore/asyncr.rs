//! WMCreateReader's reader: IWMReader and IWMReaderAdvanced2 over the sync reader's parts
//! (the same Reader: outputs, their types and decoders, the program's allocators). A guest
//! thread of its own tells the program's IWMReaderCallback what happens (OnStatus: opened,
//! started, stopped, the end, closed) and hands it the samples (OnSample) when they are due: by
//! the reader's clock from Start (sound a little ahead, as Windows' reader does), or, with a
//! clock of the program's (SetUserProvidedClock), up to what DeliverTime allows, then
//! IWMReaderCallbackAdvanced::OnTime.

use super::*;
use crate::dshow_play::now;

pub const IID_READER: Guid = wm(0x9640_6bd6);
pub const IID_ADVANCED: [Guid; 2] = [wm(0x9640_6bea), guid(0xae14_a945, 0xb90c, 0x4d0d, [0x91, 0x27, 0x80, 0xd6, 0x65, 0xf7, 0xd7, 0x3e])];
const IID_CALLBACK_ADVANCED: Guid = wm(0x9640_6beb);
/// DirectShow's {56a86897-0ad4-11ce-b03a-0020af0ba770}: Windows' reader is a clock too
pub const IID_REFERENCE_CLOCK: Guid = guid(0x56a8_6897, 0x0ad4, 0x11ce, [0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70]);

// WMT_STATUS
const WMT_OPENED: u32 = 1;
const WMT_END_OF_FILE: u32 = 4;
const WMT_END_OF_STREAMING: u32 = 6;
const WMT_STARTED: u32 = 11;
const WMT_STOPPED: u32 = 12;
const WMT_CLOSED: u32 = 13;

/// How far ahead of the clock samples go out
const LEAD: u64 = 5_000_000;

#[derive(Default)]
pub struct Async {
    /// the program's IWMReaderCallback (a reference held), its IWMReaderCallbackAdvanced (one
    /// held, or 0), and the context Open / Start gave
    cb: u32,
    adv: u32,
    ctx: u32,
    /// OnStatus to tell: status, HRESULT
    events: VecDeque<(u32, u32)>,
    running: bool,
    /// the clock: the position `at` clock time `since` (100 ns)
    pos: u64,
    since: u64,
    paused: bool,
    user_clock: bool,
    /// DeliverTime's time, and whether OnTime has told it
    deliver: u64,
    told: bool,
    /// the next sample, not due yet: output, time, duration, bytes, key
    held: Option<(usize, u64, u64, Vec<u8>, bool)>,
    thread: bool,
    /// the outputs / streams whose buffers IWMReaderCallbackAdvanced allocates
    /// (SetAllocateForOutput / SetAllocateForStream TRUE)
    alloc_out: Vec<u32>,
    alloc_stream: Vec<u32>,
}

impl Async {
    /// The position at clock time `now`
    fn clock(&self, now: u64) -> u64 {
        if self.paused { self.pos } else { self.pos + now.saturating_sub(self.since) }
    }
    fn start(&mut self, from: u64, ctx: u32, now: u64) {
        self.ctx = ctx;
        self.running = true;
        self.paused = false;
        self.pos = from;
        self.since = now;
        self.held = None;
        self.deliver = from;
        self.told = true;
        self.events.push_back((WMT_STARTED, 0));
    }
    fn stop(&mut self) {
        self.running = false;
        self.held = None;
        self.events.push_back((WMT_STOPPED, 0));
    }
    fn pause(&mut self, now: u64) {
        self.pos = self.clock(now);
        self.paused = true;
    }
    fn resume(&mut self, now: u64) {
        if self.paused {
            self.paused = false;
            self.since = now;
        }
    }
    /// DeliverTime; false without a user clock
    fn deliver_to(&mut self, t: u64) -> bool {
        if !self.user_clock {
            return false;
        }
        self.deliver = t;
        self.told = false;
        true
    }
    /// The file's end
    fn ended(&mut self) {
        self.running = false;
        self.events.push_back((WMT_END_OF_FILE, 0));
        self.events.push_back((WMT_END_OF_STREAMING, 0));
    }
    /// What the delivery thread does next, at clock time `now`
    fn step(&mut self, now: u64) -> Step {
        if let Some((s, hr)) = self.events.pop_front() {
            return Step::Status(self.cb, s, hr, self.ctx);
        }
        if !self.running || self.cb == 0 {
            return Step::Wait;
        }
        let Some(h) = self.held.as_ref() else { return Step::Read };
        let limit = if self.user_clock { self.deliver } else { self.clock(now) + LEAD };
        if h.1 <= limit {
            return Step::Sample(self.cb, self.ctx, self.held.take().unwrap());
        }
        if self.user_clock && !self.told && self.adv != 0 {
            self.told = true;
            return Step::Time(self.adv, self.deliver, self.ctx);
        }
        Step::Wait
    }
}

fn asy<R>(p: u32, f: impl FnOnce(&mut Async) -> R) -> Option<R> {
    let b = block(p)?;
    table(|m| m.get_mut(&b).and_then(|o| o.a.as_mut()).map(f))
}

struct Vt {
    reader: u32,
    adv: u32,
    clock: u32,
}

fn vts() -> &'static Vt {
    static V: std::sync::OnceLock<Vt> = std::sync::OnceLock::new();
    V.get_or_init(|| Vt {
        reader: vtable(
            "IWMReader",
            &[
                (qi, 12),
                (add_ref, 4),
                (release, 4),
                (open, 16),
                (r_close, 4),
                (sr_get_output_count, 8),
                (sr_get_output_props, 12),
                (sr_set_output_props, 12),
                (sr_get_output_format_count, 12),
                (sr_get_output_format, 16),
                (start, 28),
                (stop, 4),
                (pause, 4),
                (resume, 4),
            ],
        ),
        clock: vtable(
            "IReferenceClock",
            &[(qi, 12), (add_ref, 4), (release, 4), (get_time, 8), (notimpl, 24), (notimpl, 28), (ok8, 8)],
        ),
        adv: vtable(
            "IWMReaderAdvanced2",
            &[
                (qi, 12),
                (add_ref, 4),
                (release, 4),
                (set_user_clock, 8),
                (get_user_clock, 8),
                (deliver_time, 12),
                (ok8, 8),          // SetManualStreamSelection
                (get_false, 8),    // GetManualStreamSelection
                (sr_set_streams_selected, 16),
                (sr_get_stream_selected, 12),
                (ok8, 8),          // SetReceiveSelectionCallbacks
                (get_false, 8),    // GetReceiveSelectionCallbacks
                (sr_set_read_stream_samples, 12),
                (sr_get_read_stream_samples, 12),
                (set_alloc_output, 12),
                (get_alloc_output, 12),
                (set_alloc_stream, 12),
                (get_alloc_stream, 12),
                (notimpl, 8),      // GetStatistics
                (ok8, 8),          // SetClientInfo
                (sr_get_max_output_sample_size, 12),
                (sr_get_max_stream_sample_size, 12),
                (ok12, 12),        // NotifyLateDelivery(QWORD)
                // IWMReaderAdvanced2
                (ok8, 8),          // SetPlayMode
                (get_zero, 8),     // GetPlayMode
                (buffer_progress, 12),
                (download_progress, 20),
                (notimpl, 8),      // GetSaveAsProgress
                (notimpl, 8),      // SaveFileAs
                (notimpl, 12),     // GetProtocolName
                (notimpl, 24),     // StartAtMarker
                (sr_get_output_setting, 24),
                (sr_set_output_setting, 24),
                (ok20, 20),        // Preroll(QWORD, QWORD, float)
                (ok8, 8),          // SetLogClientID
                (get_false, 8),    // GetLogClientID
                (ok4, 4),          // StopBuffering
                (open_stream, 16),
            ],
        ),
    })
}

fn ok4(_: &mut Ctx) -> u32 {
    0
}
fn ok8(_: &mut Ctx) -> u32 {
    0
}
fn ok12(_: &mut Ctx) -> u32 {
    0
}
fn ok20(_: &mut Ctx) -> u32 {
    0
}
fn get_false(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 0);
    0
}
fn get_zero(c: &mut Ctx) -> u32 {
    get_false(c)
}
/// GetBufferProgress(DWORD *percent, QWORD *buffering): all of it, a local file
fn buffer_progress(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        w32(c.a(1), 100);
    }
    if c.a(2) != 0 {
        w64(c.a(2), 0);
    }
    0
}
/// GetDownloadProgress(DWORD *percent, QWORD *downloaded, QWORD *time)
fn download_progress(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        w32(c.a(1), 100);
    }
    for p in [c.a(2), c.a(3)] {
        if p != 0 {
            w64(p, 0);
        }
    }
    0
}

fn set_alloc(c: &mut Ctx, stream: bool) -> u32 {
    let (n, on) = (if stream { c.a(1) & 0xffff } else { c.a(1) }, c.a(2) != 0);
    asy(c.a(0), |a| {
        let v = if stream { &mut a.alloc_stream } else { &mut a.alloc_out };
        v.retain(|&x| x != n);
        if on {
            v.push(n);
        }
    })
    .map_or(E_UNEXPECTED, |_| 0)
}
fn get_alloc(c: &mut Ctx, stream: bool) -> u32 {
    let (n, out) = (if stream { c.a(1) & 0xffff } else { c.a(1) }, c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let on = asy(c.a(0), |a| if stream { a.alloc_stream.contains(&n) } else { a.alloc_out.contains(&n) }).unwrap_or(false);
    w32(out, on as u32);
    0
}
/// SetAllocateForOutput(DWORD output, BOOL): the callback's AllocateForOutput gives its buffers
fn set_alloc_output(c: &mut Ctx) -> u32 {
    set_alloc(c, false)
}
fn get_alloc_output(c: &mut Ctx) -> u32 {
    get_alloc(c, false)
}
/// SetAllocateForStream(WORD stream, BOOL)
fn set_alloc_stream(c: &mut Ctx) -> u32 {
    set_alloc(c, true)
}
fn get_alloc_stream(c: &mut Ctx) -> u32 {
    get_alloc(c, true)
}

/// A buffer of `data` from IWMReaderCallbackAdvanced::AllocateForStream (slot 7) /
/// AllocateForOutput (8)(num, size, INSSBuffer **, context); 0 when it gives none.
fn callback_buffer(c: &mut Ctx, adv: u32, stream: bool, num: u32, data: &[u8], ctx: u32) -> u32 {
    let pp = mem::heap_alloc(8, true);
    let hr = crate::dshow::com(c, adv, if stream { 7 } else { 8 }, &[num, data.len() as u32, pp, ctx]);
    let b = rp(pp);
    mem::heap_free(pp);
    if hr & 0x8000_0000 != 0 || b == 0 {
        return 0;
    }
    // GetBuffer(&p), SetLength(n)
    let p = mem::heap_alloc(8, true);
    crate::dshow::com(c, b, 6, &[p]);
    let dst = rp(p);
    mem::heap_free(p);
    if dst != 0 {
        mem::write(dst, data);
    }
    crate::dshow::com(c, b, 4, &[data.len() as u32]);
    b
}

/// IReferenceClock::GetTime(REFERENCE_TIME *): the time movies play by
fn get_time(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w64(c.a(1), now());
    0
}

/// A new async reader: its IWMReader.
pub fn new_reader() -> u32 {
    let b = new_sync_reader();
    let v = vts();
    wp(b + ps() * W_SYNC, v.reader);
    wp(b + ps() * W_ADV, v.adv);
    wp(b + ps() * W_CLOCK, v.clock);
    table(|m| m.get_mut(&b).map(|o| o.a = Some(Async::default())));
    b
}

/// Holds the program's callback (and its IWMReaderCallbackAdvanced), tells it OPENED, and
/// starts the delivery thread.
fn opened(c: &mut Ctx, b: u32, cb: u32, ctx: u32, hr: u32) {
    crate::dshow::com(c, cb, 1, &[]);
    let iid = mem::heap_alloc(24, true);
    put_guid(iid, &IID_CALLBACK_ADVANCED);
    let hr_qi = crate::dshow::com(c, cb, 0, &[iid, iid + 16]);
    let adv = if hr_qi == 0 { rp(iid + 16) } else { 0 };
    mem::heap_free(iid);
    let (old, old_adv, spawn) = asy(b, |a| {
        let old = (std::mem::replace(&mut a.cb, cb), std::mem::replace(&mut a.adv, adv));
        a.ctx = ctx;
        a.events.push_back((WMT_OPENED, hr));
        (old.0, old.1, !std::mem::replace(&mut a.thread, true))
    })
    .unwrap_or((0, 0, false));
    for p in [old, old_adv] {
        if p != 0 {
            crate::dshow::com(c, p, 2, &[]);
        }
    }
    if spawn {
        static ENTRY: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let entry = *ENTRY.get_or_init(|| crate::api::host_fn("wm_reader_thread", thread, 4));
        crate::spawn_gated(entry, b, 0x10000, crate::obj::new_thread_obj(0), false);
    }
}

/// Open(LPCWSTR url, IWMReaderCallback *, void *context)
fn open(c: &mut Ctx) -> u32 {
    let (this, url, cb, ctx) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(b) = block(this) else { return E_UNEXPECTED };
    if url == 0 || cb == 0 {
        return E_INVALIDARG;
    }
    let hr = sr_open(c);
    if hr != 0 {
        return hr;
    }
    opened(c, b, cb, ctx, 0);
    0
}

/// OpenStream(IStream *, IWMReaderCallback *, void *context)
fn open_stream(c: &mut Ctx) -> u32 {
    let (this, cb, ctx) = (c.a(0), c.a(2), c.a(3));
    let Some(b) = block(this) else { return E_UNEXPECTED };
    if cb == 0 {
        return E_INVALIDARG;
    }
    let hr = sr_open_stream(c);
    if hr != 0 {
        return hr;
    }
    opened(c, b, cb, ctx, 0);
    0
}

fn r_close(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let Some(rr) = reader(this) else { return E_UNEXPECTED };
    {
        let mut r = lock(&rr);
        if r.r.is_none() {
            return NS_E_INVALID_REQUEST;
        }
        close(c, &mut r);
    }
    asy(this, |a| {
        a.running = false;
        a.held = None;
        a.events.push_back((WMT_CLOSED, 0));
    });
    0
}

/// Start(QWORD start, QWORD duration, float rate, void *context)
fn start(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let (from, dur) = (q(c, 1, 1), q(c, 3, 2));
    let ctx = if mem::x64() { c.a64(4) as u32 } else { c.a(6) };
    let Some(rr) = reader(this) else { return E_UNEXPECTED };
    {
        let mut r = lock(&rr);
        if r.r.is_none() {
            return NS_E_INVALID_REQUEST;
        }
        // (0: on from where it is, as Windows' "resume")
        seek(c, &mut r, from, if dur as i64 > 0 { dur } else { 0 });
    }
    asy(this, |a| a.start(from, ctx, now()));
    0
}

fn stop(c: &mut Ctx) -> u32 {
    asy(c.a(0), Async::stop).map_or(E_UNEXPECTED, |_| 0)
}

fn pause(c: &mut Ctx) -> u32 {
    asy(c.a(0), |a| a.pause(now())).map_or(E_UNEXPECTED, |_| 0)
}

fn resume(c: &mut Ctx) -> u32 {
    asy(c.a(0), |a| a.resume(now())).map_or(E_UNEXPECTED, |_| 0)
}

/// SetUserProvidedClock(BOOL)
fn set_user_clock(c: &mut Ctx) -> u32 {
    let on = c.a(1) != 0;
    asy(c.a(0), |a| a.user_clock = on).map_or(E_UNEXPECTED, |_| 0)
}

fn get_user_clock(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let on = asy(c.a(0), |a| a.user_clock).unwrap_or(false);
    w32(c.a(1), on as u32);
    0
}

/// DeliverTime(QWORD): samples up to this time go out, then OnTime
fn deliver_time(c: &mut Ctx) -> u32 {
    let t = q(c, 1, 1);
    asy(c.a(0), |a| if a.deliver_to(t) { 0 } else { NS_E_INVALID_REQUEST }).unwrap_or(E_UNEXPECTED)
}

/// What the delivery thread does next.
enum Step {
    Status(u32, u32, u32, u32),
    /// a sample to hand out: callback, context, (output, time, duration, bytes, key)
    Sample(u32, u32, (usize, u64, u64, Vec<u8>, bool)),
    Time(u32, u64, u32),
    /// a sample is wanted from the file
    Read,
    Wait,
    Gone,
}

fn next_step(b: u32) -> Step {
    let t = now();
    asy(b, |a| a.step(t)).unwrap_or(Step::Gone)
}

/// The delivery thread of reader block `b` (its parameter): until the reader goes.
fn thread(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    // (the callback held, let go when the reader is gone)
    let mut held = (0u32, 0u32);
    loop {
        let step = next_step(b);
        if let Some(x) = asy(b, |a| (a.cb, a.adv)) {
            held = x;
        }
        match step {
            Step::Status(cb, s, hr, ctx) => {
                // OnStatus(WMT_STATUS, HRESULT, WMT_ATTR_DATATYPE, BYTE *value, void *context)
                let v = mem::heap_alloc(8, true);
                crate::dshow::com(c, cb, 3, &[s, hr, 0, v, ctx]);
                mem::heap_free(v);
            }
            Step::Sample(cb, ctx, (k, time, dur, data, key)) => {
                let Some(rr) = reader(b) else { continue };
                let (alloc, flags, own) = {
                    let mut r = lock(&rr);
                    let Some(o) = r.outputs.get_mut(k) else { continue };
                    let mut flags = if key { WM_SF_CLEANPOINT } else { 0 };
                    if std::mem::replace(&mut o.discontinuity, false) {
                        flags |= WM_SF_DISCONTINUITY;
                    }
                    let alloc = if o.alloc_stream != 0 && o.compressed {
                        (o.alloc_stream, true, o.stream as u32)
                    } else if o.alloc_output != 0 && !o.compressed {
                        (o.alloc_output, false, k as u32)
                    } else {
                        (0, false, 0)
                    };
                    // (the callback's own buffers, if it asked to allocate them)
                    let num = if o.compressed { o.stream as u32 } else { k as u32 };
                    let own = asy(b, |a| if o.compressed { a.alloc_stream.contains(&num) } else { a.alloc_out.contains(&num) }).unwrap_or(false);
                    (alloc, flags, own.then_some((o.compressed, num)))
                };
                let p = mem::heap_alloc(8, true);
                let hr = match own {
                    Some((stream, num)) => {
                        let adv = asy(b, |a| a.adv).unwrap_or(0);
                        let buf = if adv != 0 { callback_buffer(c, adv, stream, num, &data, ctx) } else { 0 };
                        if buf != 0 { putp(p, buf); 0 } else { E_OUTOFMEMORY }
                    }
                    None => hand_out(c, alloc, &data, time, dur, flags, p),
                };
                if hr == 0 {
                    let buf = rp(p);
                    // OnSample(DWORD output, QWORD time, QWORD duration, DWORD flags,
                    // INSSBuffer *, void *context)
                    crate::dshow::com(c, cb, 4, &[k as u32, time as u32, (time >> 32) as u32, dur as u32, (dur >> 32) as u32, flags, buf, ctx]);
                    crate::dshow::com(c, buf, 2, &[]);
                }
                mem::heap_free(p);
            }
            Step::Time(adv, t, ctx) => {
                // IWMReaderCallbackAdvanced::OnTime(QWORD, void *context)
                crate::dshow::com(c, adv, 4, &[t as u32, (t >> 32) as u32, ctx]);
            }
            Step::Read => {
                let Some(rr) = reader(b) else { continue };
                let got = {
                    let mut r = lock(&rr);
                    if r.r.is_none() { None } else { Some(with_ctx(c, || next_sample(&mut r, 0))) }
                };
                asy(b, |a| match got {
                    Some(Some(s)) => a.held = Some(s),
                    // the end
                    _ => a.ended(),
                });
            }
            Step::Wait => {
                kura::snap::sleep(std::time::Duration::from_millis(5));
                crate::snap::point();
            }
            Step::Gone => break,
        }
    }
    for p in [held.0, held.1] {
        if p != 0 {
            crate::dshow::com(c, p, 2, &[]);
        }
    }
    crate::k32::exit_thread(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A reader with a callback (and an advanced one) whose file holds samples at `times`
    fn run(a: &mut Async, file: &mut VecDeque<u64>, now: u64, out: &mut Vec<Step>) {
        // (to the first Wait: what the thread does at once)
        for _ in 0..1000 {
            match a.step(now) {
                Step::Read => match file.pop_front() {
                    Some(t) => a.held = Some((0, t, 1, vec![], true)),
                    None => a.ended(),
                },
                Step::Wait => return,
                s => out.push(s),
            }
        }
        panic!("no Wait");
    }

    fn reader(user_clock: bool) -> Async {
        Async { cb: 0x100, adv: 0x200, user_clock, ..Default::default() }
    }

    fn statuses(out: &[Step]) -> Vec<u32> {
        out.iter().filter_map(|s| if let Step::Status(_, s, _, _) = s { Some(*s) } else { None }).collect()
    }
    fn samples(out: &[Step]) -> Vec<u64> {
        out.iter().filter_map(|s| if let Step::Sample(_, _, x) = s { Some(x.1) } else { None }).collect()
    }

    #[test]
    fn started_then_samples_then_the_end() {
        let mut a = reader(false);
        let mut file: VecDeque<u64> = (0..10).map(|i| i * 1_000_000).collect();
        let mut out = vec![];
        a.start(0, 7, 0);
        run(&mut a, &mut file, 100_000_000, &mut out);
        assert_eq!(statuses(&out), [WMT_STARTED, WMT_END_OF_FILE, WMT_END_OF_STREAMING]);
        assert_eq!(samples(&out), (0..10).map(|i| i * 1_000_000).collect::<Vec<_>>());
        assert!(matches!(out[0], Step::Status(0x100, WMT_STARTED, 0, 7)));
        assert!(out[1..11].iter().all(|s| matches!(s, Step::Sample(0x100, 7, _))));
    }

    #[test]
    fn samples_go_out_half_a_second_ahead_of_the_clock() {
        let mut a = reader(false);
        let mut file: VecDeque<u64> = (0..100).map(|i| i * 1_000_000).collect();
        let mut out = vec![];
        a.start(0, 0, 1000);
        run(&mut a, &mut file, 1000, &mut out);
        assert_eq!(samples(&out).last(), Some(&LEAD));
        out.clear();
        run(&mut a, &mut file, 1000 + 20_000_000, &mut out);
        assert_eq!(samples(&out), (6..=25).map(|i| i * 1_000_000).collect::<Vec<_>>());
        // (paused: no more until resumed)
        a.pause(1000 + 20_000_000);
        out.clear();
        run(&mut a, &mut file, 1000 + 90_000_000, &mut out);
        assert!(out.is_empty());
        a.resume(1000 + 90_000_000);
        run(&mut a, &mut file, 1000 + 91_000_000, &mut out);
        assert_eq!(samples(&out), [26_000_000]);
    }

    #[test]
    fn user_clock_delivers_up_to_deliver_time_then_on_time() {
        let mut a = reader(true);
        let mut file: VecDeque<u64> = (0..100).map(|i| i * 1_000_000).collect();
        let mut out = vec![];
        a.start(0, 3, 0);
        run(&mut a, &mut file, u64::MAX / 2, &mut out);
        // (only what is at the start; no OnTime without DeliverTime)
        assert_eq!(samples(&out), [0]);
        assert!(!out.iter().any(|s| matches!(s, Step::Time(..))));
        out.clear();
        assert!(a.deliver_to(4_000_000));
        run(&mut a, &mut file, 0, &mut out);
        assert_eq!(samples(&out), [1_000_000, 2_000_000, 3_000_000, 4_000_000]);
        assert!(matches!(out.last(), Some(Step::Time(0x200, 4_000_000, 3))));
        out.clear();
        run(&mut a, &mut file, 0, &mut out);
        assert!(out.is_empty(), "OnTime once");
        assert!(!reader(false).deliver_to(1));
    }

    #[test]
    fn stop_ends_delivery() {
        let mut a = reader(false);
        let mut file: VecDeque<u64> = (0..100).map(|i| i * 1_000_000).collect();
        let mut out = vec![];
        a.start(0, 0, 0);
        run(&mut a, &mut file, 0, &mut out);
        a.stop();
        out.clear();
        run(&mut a, &mut file, 1_000_000_000, &mut out);
        assert_eq!(statuses(&out), [WMT_STOPPED]);
        assert!(samples(&out).is_empty());
    }

    #[derive(Debug, Clone)]
    enum Op {
        Start(u64),
        Stop,
        Pause,
        Resume,
        Deliver(u64),
        Later(u64),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u64..50).prop_map(|t| Op::Start(t * 1_000_000)),
            Just(Op::Stop),
            Just(Op::Pause),
            Just(Op::Resume),
            (0u64..60).prop_map(|t| Op::Deliver(t * 1_000_000)),
            (0u64..30_000_000).prop_map(Op::Later),
        ]
    }

    proptest! {
        /// Whatever the program does: statuses in the order things happened, samples only while
        /// running, in time order from where Start put the file, never past the clock's lead
        /// or DeliverTime, and OnTime once per DeliverTime with nothing before it held back.
        #[test]
        fn delivery_follows_the_clock(user_clock in any::<bool>(), times in prop::collection::vec(0u64..40, 0..40), ops in prop::collection::vec(op(), 1..60)) {
            let mut times: Vec<u64> = times.into_iter().map(|t| t * 1_000_000).collect();
            times.sort();
            let mut a = reader(user_clock);
            let mut file = VecDeque::new();
            let (mut now, mut want, mut got, mut last, mut started) = (0u64, vec![], vec![], None::<u64>, false);
            let mut out = vec![];
            for o in ops {
                match o {
                    Op::Start(t) => {
                        // (the file seeks to t, as Start does)
                        file = times.iter().copied().filter(|&x| x >= t).collect();
                        a.start(t, 9, now);
                        want.push(WMT_STARTED);
                        last = None;
                        started = true;
                    }
                    Op::Stop => {
                        a.stop();
                        want.push(WMT_STOPPED);
                        started = false;
                    }
                    Op::Pause => a.pause(now),
                    Op::Resume => a.resume(now),
                    Op::Deliver(t) => {
                        prop_assert_eq!(a.deliver_to(t), user_clock);
                    }
                    Op::Later(d) => now += d,
                }
                out.clear();
                let before = file.len();
                run(&mut a, &mut file, now, &mut out);
                let mut times_told = 0;
                for s in &out {
                    match s {
                        Step::Status(cb, s, hr, _) => {
                            prop_assert_eq!((*cb, *hr), (0x100, 0));
                            got.push(*s);
                        }
                        Step::Sample(_, ctx, x) => {
                            prop_assert!(started && a.running || file.is_empty() && !a.running);
                            prop_assert_eq!(*ctx, 9);
                            prop_assert!(last.map_or(true, |l| l <= x.1));
                            last = Some(x.1);
                            let limit = if user_clock { a.deliver } else { a.clock(now) + LEAD };
                            prop_assert!(x.1 <= limit, "{} past {}", x.1, limit);
                        }
                        Step::Time(adv, t, _) => {
                            prop_assert!(user_clock);
                            prop_assert_eq!(*adv, 0x200);
                            prop_assert_eq!(*t, a.deliver);
                            // (only with the next sample after it)
                            prop_assert!(a.held.as_ref().map_or(false, |h| h.1 > *t));
                            times_told += 1;
                        }
                        _ => {}
                    }
                }
                prop_assert!(times_told <= 1);
                prop_assert!(before >= file.len());
                // (what is held is not yet due)
                if let (true, Some(h)) = (a.running, a.held.as_ref()) {
                    let limit = if user_clock { a.deliver } else { a.clock(now) + LEAD };
                    prop_assert!(h.1 > limit);
                }
                if !a.running {
                    prop_assert!(!out.iter().skip_while(|s| !matches!(s, Step::Status(_, WMT_STOPPED | WMT_END_OF_STREAMING, _, _))).any(|s| matches!(s, Step::Sample(..))));
                }
            }
            // (Start / Stop as asked, each end told as END_OF_FILE then END_OF_STREAMING)
            let asked: Vec<u32> = got.iter().copied().filter(|&s| s == WMT_STARTED || s == WMT_STOPPED).collect();
            prop_assert_eq!(asked, want);
            for (i, s) in got.iter().enumerate() {
                if *s == WMT_END_OF_FILE {
                    prop_assert_eq!(got.get(i + 1), Some(&WMT_END_OF_STREAMING));
                }
            }
        }
    }
}
