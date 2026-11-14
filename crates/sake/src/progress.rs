//! comctl32's progress bar (msctls_progress32): its range, position and step (PBM_SETRANGE,
//! PBM_SETRANGE32, PBM_GETRANGE, PBM_SETPOS, PBM_DELTAPOS, PBM_SETSTEP, PBM_STEPIT, PBM_GETPOS,
//! PBM_GETSTEP), its colours (PBM_SETBARCOLOR, PBM_SETBKCOLOR and their getters), PBM_GETSTATE,
//! and the classic look: a sunken frame, the done part from the left (from the bottom with
//! PBS_VERTICAL) in blocks, or one bar with PBS_SMOOTH. As Wine's progress.c. The copy page of
//! Delphi installers shows one (せきさば！, #222); sake drew nothing.

use super::*;

/// A progress bar's state; the messages as methods (each returns what the message does).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Progress {
    pub min: i32,
    pub max: i32,
    pub pos: i32,
    pub step: i32,
    /// COLORREFs; CLR_DEFAULT (0xff000000) the system's
    pub bar: u32,
    pub bk: u32,
}

const CLR_DEFAULT: u32 = 0xff00_0000;

impl Default for Progress {
    fn default() -> Self {
        Progress { min: 0, max: 100, pos: 0, step: 10, bar: CLR_DEFAULT, bk: CLR_DEFAULT }
    }
}

impl Progress {
    fn coerce(&mut self) {
        if self.pos < self.min {
            self.pos = self.min;
        }
        if self.pos > self.max {
            self.pos = self.max;
        }
    }
    /// PBM_SETRANGE / PBM_SETRANGE32: the old range as MAKELONG(low, high)
    pub fn set_range(&mut self, low: i32, high: i32) -> u32 {
        let old = (self.min as u32 & 0xffff) | (self.max as u32) << 16;
        self.min = low;
        self.max = high;
        self.coerce();
        old
    }
    /// PBM_SETPOS: the old position
    pub fn set_pos(&mut self, pos: i32) -> u32 {
        let old = self.pos;
        self.pos = pos;
        self.coerce();
        old as u32
    }
    /// PBM_DELTAPOS: the old position
    pub fn delta(&mut self, d: i32) -> u32 {
        self.set_pos(self.pos.wrapping_add(d))
    }
    /// PBM_STEPIT: the old position; past an end it starts over from the other (Wine)
    pub fn step_it(&mut self) -> u32 {
        let old = self.pos;
        let span = self.max as i64 - self.min as i64;
        let mut p = self.pos as i64 + self.step as i64;
        if span > 0 {
            if p > self.max as i64 {
                p = (p - self.min as i64) % span + self.min as i64;
            }
            if p < self.min as i64 {
                p = (p - self.min as i64) % span + self.max as i64;
            }
        }
        self.pos = p as i32;
        self.coerce();
        old as u32
    }
    /// The done share of `len` pixels.
    pub fn filled(&self, len: i32) -> i32 {
        let span = self.max as i64 - self.min as i64;
        if span <= 0 || len <= 0 {
            return 0;
        }
        (((self.pos as i64 - self.min as i64).clamp(0, span) * len as i64) / span) as i32
    }
}

static BARS: Mutex<Option<HashMap<u32, Progress>>> = Mutex::new(None);
fn with_bar<R>(h: u32, f: impl FnOnce(&mut Progress) -> R) -> R {
    f(BARS.lock().unwrap().get_or_insert_with(HashMap::default).entry(h).or_default())
}

fn paint(c: &mut Ctx, h: u32) {
    let (style, (w, hh)) = with(|s| s.wins.get(&h).map_or((0, (0, 0)), |w| (w.style, (w.client.w(), w.client.h()))));
    let bar = with_bar(h, |b| *b);
    let ps = mem::heap_alloc(64, true);
    let dc = gcall(c, "user32", "BeginPaint", &[h, ps]);
    let color = |cr: u32, sys: u32| if cr == CLR_DEFAULT { crate::gdi::solid_brush(sys_color(sys)) } else { crate::gdi::solid_brush(cr & 0xff_ffff) };
    let full = Rect { l: 0, t: 0, r: w, b: hh };
    crate::gdi::fill_rect_brush(dc, full, color(bar.bk, 15));
    // the sunken frame: shadow at the top and left, light at the bottom and right
    let line = |r: Rect, cr: u32| crate::gdi::fill_rect_brush(dc, r, crate::gdi::solid_brush(cr));
    line(Rect { l: 0, t: 0, r: w, b: 1 }, 0xa0a0a0);
    line(Rect { l: 0, t: 0, r: 1, b: hh }, 0xa0a0a0);
    line(Rect { l: 0, t: hh - 1, r: w, b: hh }, 0xffffff);
    line(Rect { l: w - 1, t: 0, r: w, b: hh }, 0xffffff);
    let inner = Rect { l: 2, t: 2, r: w - 2, b: hh - 2 };
    let fill = color(bar.bar, 13);
    let vertical = style & 4 != 0;
    let len = if vertical { inner.h() } else { inner.w() };
    let done = bar.filled(len);
    if style & 1 != 0 {
        let r = if vertical { Rect { t: inner.b - done, ..inner } } else { Rect { r: inner.l + done, ..inner } };
        crate::gdi::fill_rect_brush(dc, r, fill);
    } else {
        // blocks two thirds as long as the bar is thick, two pixels apart (comctl32 5)
        let thick = if vertical { inner.w() } else { inner.h() };
        let block = (thick * 2 / 3).max(1);
        let mut at = 0;
        while at < done {
            let n = block.min(len - at);
            let r = if vertical {
                Rect { l: inner.l, t: inner.b - at - n, r: inner.r, b: inner.b - at }
            } else {
                Rect { l: inner.l + at, t: inner.t, r: inner.l + at + n, b: inner.b }
            };
            crate::gdi::fill_rect_brush(dc, r, fill);
            at += block + 2;
        }
    }
    gcall(c, "user32", "EndPaint", &[h, ps]);
    mem::heap_free(ps);
}

pub(super) fn proc_fn(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let changed = |c: &mut Ctx, r: u32, before: Progress| {
        if with_bar(h, |b| *b) != before {
            invalidate(h, None, true);
        }
        let _ = c;
        r
    };
    let before = with_bar(h, |b| *b);
    match m {
        0x401 => changed(c, with_bar(h, |b| b.set_range(lp as u16 as i32, (lp >> 16) as u16 as i32)), before),
        0x402 => changed(c, with_bar(h, |b| b.set_pos(wp as i32)), before),
        0x403 => changed(c, with_bar(h, |b| b.delta(wp as i32)), before),
        0x404 => with_bar(h, |b| std::mem::replace(&mut b.step, wp as i32) as u32),
        0x405 => changed(c, with_bar(h, |b| b.step_it()), before),
        0x406 => changed(c, with_bar(h, |b| b.set_range(wp as i32, lp as i32)), before),
        // PBM_GETRANGE(low?, PBRANGE*)
        0x407 => {
            let (lo, hi) = (before.min, before.max);
            put32(lp, lo as u32);
            if lp != 0 {
                mem::w32(lp + 4, hi as u32);
            }
            if wp != 0 { lo as u32 } else { hi as u32 }
        }
        0x408 => before.pos as u32,
        // PBM_SETBARCOLOR / PBM_SETBKCOLOR (CCM_SETBKCOLOR): the old colour
        0x409 => changed(c, with_bar(h, |b| std::mem::replace(&mut b.bar, lp)), before),
        0x2001 => changed(c, with_bar(h, |b| std::mem::replace(&mut b.bk, lp)), before),
        // PBM_GETSTEP, PBM_GETBKCOLOR, PBM_GETBARCOLOR, PBM_GETSTATE (PBST_NORMAL)
        0x40d => before.step as u32,
        0x40e => before.bk,
        0x40f => before.bar,
        0x411 => 1,
        0xf => {
            paint(c, h);
            0
        }
        0x14 => 1,
        0x2 => {
            BARS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
            def_proc(c, msg_wide())
        }
        _ => def_proc(c, msg_wide()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Op {
        Range(i32, i32),
        Pos(i32),
        Delta(i32),
        Step(i32),
        StepIt,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (-300i32..300, -300i32..300).prop_map(|(a, b)| Op::Range(a.min(b), a.max(b))),
            (-500i32..500).prop_map(Op::Pos),
            (-200i32..200).prop_map(Op::Delta),
            (-150i32..150).prop_map(Op::Step),
            Just(Op::StepIt),
        ]
    }

    proptest! {
        /// Whatever the messages (ranges low <= high): the position stays in the range, each
        /// returns the old position (range: the old range), SETPOS and DELTAPOS clamp, a step
        /// inside the range moves by the step, the filled part grows with the position.
        #[test]
        fn messages(ops in prop::collection::vec(op(), 0..40)) {
            let mut p = Progress::default();
            for o in ops {
                let old = p;
                match o {
                    Op::Range(a, b) => {
                        let r = p.set_range(a, b);
                        prop_assert_eq!(r, (old.min as u32 & 0xffff) | (old.max as u32) << 16);
                    }
                    Op::Pos(x) => {
                        prop_assert_eq!(p.set_pos(x), old.pos as u32);
                        prop_assert_eq!(p.pos, x.clamp(p.min, p.max));
                    }
                    Op::Delta(d) => {
                        prop_assert_eq!(p.delta(d), old.pos as u32);
                        prop_assert_eq!(p.pos, (old.pos + d).clamp(p.min, p.max));
                    }
                    Op::Step(s) => p.step = s,
                    Op::StepIt => {
                        prop_assert_eq!(p.step_it(), old.pos as u32);
                        let t = old.pos + old.step;
                        if (old.min..=old.max).contains(&t) {
                            prop_assert_eq!(p.pos, t);
                        }
                    }
                }
                prop_assert!(p.min <= p.pos && p.pos <= p.max, "{:?}", p);
                prop_assert!(p.filled(200) >= 0 && p.filled(200) <= 200);
            }
        }

        /// The filled pixels: none at the start, all at the end, more as the position grows.
        #[test]
        fn filled(min in -100i32..100, span in 1i32..400, a in 0i32..400, b in 0i32..400, len in 0i32..600) {
            let mut p = Progress { min, max: min + span, ..Progress::default() };
            p.set_pos(min);
            prop_assert_eq!(p.filled(len), 0);
            p.set_pos(min + span);
            prop_assert_eq!(p.filled(len), len.max(0));
            p.set_pos(min + a.min(b));
            let fa = p.filled(len);
            p.set_pos(min + a.max(b));
            prop_assert!(fa <= p.filled(len));
        }
    }

    /// PBM_STEPIT past the end starts over (Wine's progress.c): 95 + 10 in 0..100 is 5.
    #[test]
    fn step_wraps() {
        let mut p = Progress::default();
        p.set_pos(95);
        p.step_it();
        assert_eq!(p.pos, 5);
        p.step = -10;
        p.set_pos(5);
        p.step_it();
        assert_eq!(p.pos, 95);
    }
}
