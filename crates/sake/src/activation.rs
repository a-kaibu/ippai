//! Who hears WM_ACTIVATEAPP when the active window changes. Windows (and Wine's
//! set_active_window) send it when the activation moves to another thread: wParam FALSE to every
//! top-level window of the thread losing it (lParam: the thread gaining it), then TRUE to every
//! top-level window of the thread gaining it (lParam: the thread losing it, 0 when none), hidden
//! ones included, message-only windows (HWND_MESSAGE) not. Nothing when both windows belong to
//! the same thread.
//!
//! sake sent it to the newly active window alone. Delphi's TApplication keeps its own hidden
//! top-level window, and only its WM_ACTIVATEAPP fires Application.OnActivate (Active, the
//! TApplicationEvents.OnActivate handlers): LiveMaker titles (live.dll + DelphiX) set their
//! "active" flag there and drop mouse clicks while it is off, so their title buttons lit up under
//! the cursor (mouse moves don't check it) but no click started the game (#189, #204).

/// A top-level window: (hwnd, its thread, message-only).
pub type Top = (u32, u32, bool);

/// The WM_ACTIVATEAPP messages, in order: (hwnd, wParam, lParam). `old`: the thread of the window
/// that was active (None: no window was, or it is gone), `new`: the thread of the window being
/// activated; `tops`: the top-level windows in the order they are told (Z order, top first).
pub fn app_messages(old: Option<u32>, new: u32, tops: &[Top]) -> Vec<(u32, u32, u32)> {
    if old == Some(new) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(o) = old {
        out.extend(tops.iter().filter(|t| t.1 == o && !t.2).map(|t| (t.0, 0, new)));
    }
    out.extend(tops.iter().filter(|t| t.1 == new && !t.2).map(|t| (t.0, 1, old.unwrap_or(0))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The windows a Delphi 5 program has when its main form is shown (LiveMaker's 6人姉妹,
    /// #189): TApplication's window 10020 (visible, 0x0), TThread's and the hint's hidden
    /// windows, the form 10060.
    #[test]
    fn delphi_application_window_hears_it() {
        let tops = [(0x100b0, 1, false), (0x10070, 1, false), (0x10060, 1, false), (0x10030, 1, false), (0x10020, 1, false)];
        let m = app_messages(None, 1, &tops);
        assert!(m.contains(&(0x10020, 1, 0)), "TApplication's window: {m:x?}");
        assert_eq!(m.len(), 5);
        assert!(m.iter().all(|&(_, w, l)| w == 1 && l == 0));
    }

    #[test]
    fn same_thread_says_nothing() {
        let tops = [(0x10020, 1, false), (0x10030, 1, false)];
        assert_eq!(app_messages(Some(1), 1, &tops), vec![]);
    }

    #[test]
    fn other_thread_first_loses_then_gains() {
        let tops = [(0x10020, 1, false), (0x10030, 2, false), (0x10040, 1, true), (0x10050, 2, false)];
        assert_eq!(app_messages(Some(1), 2, &tops), vec![(0x10020, 0, 2), (0x10030, 1, 1), (0x10050, 1, 1)]);
    }

    fn top() -> impl Strategy<Value = Top> {
        (0x10020u32..0x10100, 1u32..4, any::<bool>())
    }

    proptest! {
        /// Windows' rule, stated per window: a window hears it at most once; exactly the
        /// non-message-only top-level windows of the two threads hear it when the threads differ,
        /// with wParam saying which side it is on and lParam naming the other thread; every
        /// "lost" comes before every "gained"; the order of `tops` is kept within each side.
        #[test]
        fn follows_windows_rule(old in proptest::option::of(1u32..4), new in 1u32..4, tops in proptest::collection::vec(top(), 0..12)) {
            let m = app_messages(old, new, &tops);
            if old == Some(new) {
                prop_assert!(m.is_empty());
                return Ok(());
            }
            let want_lost: Vec<u32> = tops.iter().filter(|t| Some(t.1) == old && !t.2).map(|t| t.0).collect();
            let want_gained: Vec<u32> = tops.iter().filter(|t| t.1 == new && !t.2).map(|t| t.0).collect();
            let lost: Vec<u32> = m.iter().filter(|x| x.1 == 0).map(|x| x.0).collect();
            let gained: Vec<u32> = m.iter().filter(|x| x.1 == 1).map(|x| x.0).collect();
            prop_assert_eq!(lost, want_lost);
            prop_assert_eq!(gained, want_gained);
            let first_gain = m.iter().position(|x| x.1 == 1).unwrap_or(m.len());
            prop_assert!(m[first_gain..].iter().all(|x| x.1 == 1));
            for &(_, w, l) in &m {
                prop_assert_eq!(l, if w == 0 { new } else { old.unwrap_or(0) });
            }
            // (a message-only window never hears it)
            prop_assert!(m.iter().all(|x| !tops.iter().any(|t| t.0 == x.0 && t.2 && !tops.iter().any(|u| u.0 == x.0 && !u.2))));
        }
    }
}
