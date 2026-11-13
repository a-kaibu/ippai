//! TrackMouseEvent's state for one thread: which window it watches, for leaving (WM_MOUSELEAVE)
//! and for hovering (WM_MOUSEHOVER once the cursor has stayed inside a 4x4 rectangle for the
//! hover time). Pure: the caller tells it where the cursor is and what time it is, and posts the
//! messages it answers. (KiriKiri 2 and Delphi's VCL ask for TME_LEAVE on every mouse move over
//! their windows; sake had no TrackMouseEvent, so they never heard that the mouse left.)

pub const TME_HOVER: u32 = 1;
pub const TME_LEAVE: u32 = 2;
pub const TME_NONCLIENT: u32 = 0x10;
pub const TME_QUERY: u32 = 0x4000_0000;
pub const TME_CANCEL: u32 = 0x8000_0000;
/// HOVER_DEFAULT: the system's hover time (SPI_GETMOUSEHOVERTIME, 400 ms)
pub const HOVER_DEFAULT: u32 = 0xffff_ffff;
pub const DEFAULT_HOVER_MS: u32 = 400;
/// SM_CXMOUSEHOVER / SM_CYMOUSEHOVER
pub const HOVER_RECT: i32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Track {
    pub hwnd: u32,
    /// TME_HOVER | TME_LEAVE (| TME_NONCLIENT)
    pub flags: u32,
    pub hover_ms: u32,
    /// when WM_MOUSEHOVER is due (ms), while TME_HOVER is on
    pub due: u64,
    /// the cursor (screen) where the hover rectangle is centred
    pub anchor: (i32, i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// WM_MOUSELEAVE (WM_NCMOUSELEAVE when nonclient) to this window
    Leave(u32, bool),
    /// WM_MOUSEHOVER (WM_NCMOUSEHOVER when nonclient) to this window, the cursor (screen)
    Hover(u32, bool, (i32, i32)),
}

fn hover_ms(ms: u32) -> u32 {
    if ms == HOVER_DEFAULT || ms == 0 { DEFAULT_HOVER_MS } else { ms }
}

/// TrackMouseEvent(flags, hwnd, hover time) (not TME_QUERY) with the window `under` the cursor.
pub fn request(cur: Option<Track>, hwnd: u32, flags: u32, ms: u32, under: u32, cursor: (i32, i32), now: u64) -> (Option<Track>, Option<Event>) {
    let what = flags & (TME_HOVER | TME_LEAVE);
    let nc = flags & TME_NONCLIENT != 0;
    if flags & TME_CANCEL != 0 {
        return match cur {
            Some(t) if t.hwnd == hwnd && (t.flags & TME_NONCLIENT != 0) == nc => {
                let left = t.flags & !what;
                ((left & (TME_HOVER | TME_LEAVE) != 0).then_some(Track { flags: left, ..t }), None)
            }
            other => (other, None),
        };
    }
    if what == 0 {
        return (cur, None);
    }
    // not over the window: it has left already (and nothing is tracked)
    if under != hwnd {
        return (if cur.is_some_and(|t| t.hwnd == hwnd) { None } else { cur }, (what & TME_LEAVE != 0).then_some(Event::Leave(hwnd, nc)));
    }
    // the same window and area adds to what is tracked; another replaces it
    let kept = match cur {
        Some(t) if t.hwnd == hwnd && (t.flags & TME_NONCLIENT != 0) == nc => t.flags & (TME_HOVER | TME_LEAVE),
        _ => 0,
    };
    let mut t = Track { hwnd, flags: kept | what | if nc { TME_NONCLIENT } else { 0 }, hover_ms: DEFAULT_HOVER_MS, due: 0, anchor: cursor };
    if let Some(c) = cur.filter(|c| c.hwnd == hwnd && kept & TME_HOVER != 0) {
        (t.hover_ms, t.due, t.anchor) = (c.hover_ms, c.due, c.anchor);
    }
    if what & TME_HOVER != 0 {
        t.hover_ms = hover_ms(ms);
        t.due = now + t.hover_ms as u64;
        t.anchor = cursor;
    }
    (Some(t), None)
}

/// The cursor moved to `cursor`, over window `under`.
pub fn moved(cur: Option<Track>, under: u32, cursor: (i32, i32), now: u64) -> (Option<Track>, Option<Event>) {
    let Some(mut t) = cur else { return (None, None) };
    let nc = t.flags & TME_NONCLIENT != 0;
    if under != t.hwnd {
        // leaving ends every kind of tracking
        return (None, (t.flags & TME_LEAVE != 0).then_some(Event::Leave(t.hwnd, nc)));
    }
    if t.flags & TME_HOVER != 0 && ((cursor.0 - t.anchor.0).abs() > HOVER_RECT / 2 || (cursor.1 - t.anchor.1).abs() > HOVER_RECT / 2) {
        t.anchor = cursor;
        t.due = now + t.hover_ms as u64;
    }
    (Some(t), None)
}

/// Time passed: WM_MOUSEHOVER once it is due (hover tracking then ends; leave tracking stays).
pub fn tick(cur: Option<Track>, now: u64) -> (Option<Track>, Option<Event>) {
    match cur {
        Some(t) if t.flags & TME_HOVER != 0 && now >= t.due => {
            let left = t.flags & !TME_HOVER;
            ((left & TME_LEAVE != 0).then_some(Track { flags: left, ..t }), Some(Event::Hover(t.hwnd, t.flags & TME_NONCLIENT != 0, t.anchor)))
        }
        other => (other, None),
    }
}

/// When the next WM_MOUSEHOVER is due, if one is.
pub fn next_due(cur: Option<Track>) -> Option<u64> {
    cur.filter(|t| t.flags & TME_HOVER != 0).map(|t| t.due)
}

/// TME_QUERY's answer: (flags, hwnd, hover time) of what is tracked.
pub fn query(cur: Option<Track>) -> (u32, u32, u32) {
    match cur {
        Some(t) => (t.flags, t.hwnd, if t.flags & TME_HOVER != 0 { t.hover_ms } else { 0 }),
        None => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const W: u32 = 0x10020;
    const CHILD: u32 = 0x10030;

    #[test]
    fn leave_once_when_the_cursor_goes() {
        let (t, e) = request(None, W, TME_LEAVE, 0, W, (10, 10), 0);
        assert_eq!(e, None);
        let (t, e) = moved(t, W, (20, 10), 5);
        assert_eq!(e, None);
        let (t, e) = moved(t, CHILD, (30, 10), 6);
        assert_eq!(e, Some(Event::Leave(W, false)));
        assert_eq!(t, None);
        // (nothing more until asked again)
        assert_eq!(moved(t, W, (10, 10), 7), (None, None));
    }

    #[test]
    fn asking_when_not_over_the_window_leaves_at_once() {
        assert_eq!(request(None, W, TME_LEAVE, 0, CHILD, (0, 0), 0), (None, Some(Event::Leave(W, false))));
        // hover alone over another window: nothing
        assert_eq!(request(None, W, TME_HOVER, 0, CHILD, (0, 0), 0), (None, None));
    }

    #[test]
    fn hover_after_the_hover_time_inside_the_rectangle() {
        let (t, _) = request(None, W, TME_HOVER | TME_LEAVE, HOVER_DEFAULT, W, (100, 100), 1000);
        assert_eq!(next_due(t), Some(1400));
        // a 2-pixel wobble keeps the timer; 3 pixels restart it
        let (t, _) = moved(t, W, (102, 98), 1200);
        assert_eq!(next_due(t), Some(1400));
        let (t, _) = moved(t, W, (103, 98), 1300);
        assert_eq!(next_due(t), Some(1700));
        assert_eq!(tick(t, 1699).1, None);
        let (t, e) = tick(t, 1700);
        assert_eq!(e, Some(Event::Hover(W, false, (103, 98))));
        // leave tracking stays, hover is over
        assert_eq!(query(t), (TME_LEAVE, W, 0));
        assert_eq!(tick(t, 5000).1, None);
    }

    #[test]
    fn cancel_and_query() {
        let (t, _) = request(None, W, TME_HOVER | TME_LEAVE, 50, W, (0, 0), 0);
        assert_eq!(query(t), (TME_HOVER | TME_LEAVE, W, 50));
        let (t, _) = request(t, W, TME_CANCEL | TME_HOVER, 0, W, (0, 0), 1);
        assert_eq!(query(t), (TME_LEAVE, W, 0));
        // cancelling for another window changes nothing
        let (t, _) = request(t, CHILD, TME_CANCEL | TME_LEAVE, 0, W, (0, 0), 2);
        assert_eq!(query(t), (TME_LEAVE, W, 0));
        let (t, _) = request(t, W, TME_CANCEL | TME_LEAVE, 0, W, (0, 0), 3);
        assert_eq!(t, None);
    }

    #[derive(Clone, Debug)]
    enum Op {
        Req(bool, u32, bool, u32),
        Move(bool, i32, i32),
        Tick(u64),
    }
    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (any::<bool>(), 0u32..4, any::<bool>(), 0u32..1000).prop_map(|(w, f, cancel, ms)| Op::Req(w, f, cancel, ms)),
            (any::<bool>(), -10i32..10, -10i32..10).prop_map(|(w, x, y)| Op::Move(w, x, y)),
            (0u64..2000).prop_map(Op::Tick),
        ]
    }

    proptest! {
        /// Any sequence: a leave or hover only for the tracked window and only while that kind
        /// is tracked; at most one leave per tracking; after a leave nothing is tracked; a hover
        /// never comes before its due time; what is tracked always names a kind.
        #[test]
        fn events_follow_the_tracking(ops in proptest::collection::vec(op(), 0..60)) {
            let (mut t, mut now, mut cursor) = (None::<Track>, 0u64, (0i32, 0i32));
            for o in ops {
                let before = t;
                let (nt, e) = match o {
                    Op::Req(w, f, cancel, ms) => {
                        let h = if w { W } else { CHILD };
                        request(t, h, f | if cancel { TME_CANCEL } else { 0 }, ms, W, cursor, now)
                    }
                    Op::Move(w, dx, dy) => {
                        cursor = (cursor.0 + dx, cursor.1 + dy);
                        moved(t, if w { W } else { CHILD }, cursor, now)
                    }
                    Op::Tick(d) => { now += d; tick(t, now) }
                };
                match (o, e) {
                    (Op::Move(..), Some(Event::Leave(h, _))) => {
                        let b = before.unwrap();
                        prop_assert_eq!(h, b.hwnd);
                        prop_assert!(b.flags & TME_LEAVE != 0);
                        prop_assert_eq!(nt, None);
                    }
                    (Op::Req(..), Some(Event::Leave(h, _))) => prop_assert_eq!(h, CHILD),
                    (Op::Tick(_), Some(Event::Hover(h, _, _))) => {
                        let b = before.unwrap();
                        prop_assert_eq!(h, b.hwnd);
                        prop_assert!(b.flags & TME_HOVER != 0 && now >= b.due);
                    }
                    (_, None) => {}
                    (o, e) => prop_assert!(false, "{:?} gave {:?}", o, e),
                }
                if let Some(x) = nt {
                    prop_assert!(x.flags & (TME_HOVER | TME_LEAVE) != 0);
                    if x.flags & TME_HOVER != 0 { prop_assert!(x.hover_ms > 0); }
                }
                t = nt;
            }
        }
    }
}
