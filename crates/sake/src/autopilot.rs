//! SAKE_AUTO: answers an installer's or self-extractor's windows as a person going through it
//! with the defaults would (tests/corpus-prep.sh runs setup programs in node this way, so the
//! game they install or unpack can be run afterwards). Nothing changes without SAKE_AUTO.
//!
//! Every SAKE_AUTO_MS (500) ms it looks at the program's windows. When they have stayed the same
//! for a moment (the program is waiting for the person), it acts on the window in front, the
//! active one or else the last enabled top-level window with buttons:
//!   1. an unchecked radio button that accepts a licence (同意する, I accept...),
//!   2. a checked box that would start something afterwards (起動, readme, Launch...): cleared,
//!   3. the first push button whose text is on GO (インストール, 解凍, 次へ, OK, 完了...), never
//!      one on STOP (キャンセル, 戻る, 参照...),
//!   4. else the default push button, else (a dialog with no buttons it knows) Enter.
//! A click is the mouse's: move, down and up at the control's centre, through the input queue
//! as the page's clicks come. The same action on the same unchanged windows at most
//! SAKE_AUTO_REPEAT (5) times. SAKE_AUTO_LOG prints each action ("sake auto: ...").
//! Message boxes need nothing: sake answers them itself (user::box_answer).

/// A control as decide() sees it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ctrl {
    pub h: u32,
    pub kind: Kind,
    pub text: String,
    pub checked: bool,
    pub enabled: bool,
    /// centre on the screen
    pub at: (i32, i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Push,
    DefPush,
    Check,
    Radio,
    Other,
}

/// A visible top-level window and its visible controls.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Top {
    pub h: u32,
    pub text: String,
    pub dialog: bool,
    pub enabled: bool,
    pub active: bool,
    pub ctrls: Vec<Ctrl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Act {
    /// click control h at (x, y); the text for the log
    Click(u32, i32, i32, String),
    /// Enter to window h
    Enter(u32),
}

/// Push buttons that go on with the defaults, the most wanted first.
pub const GO: &[&str] = &[
    "インストール", "install", "解凍", "展開", "extract", "unzip", "開始", "start", "はい", "yes", "同意する", "同意します",
    "i agree", "agree", "accept", "次へ", "next", "ok", "了解", "続行", "continue", "完了", "finish", "終了", "閉じる", "close",
];
/// Push buttons never pressed.
pub const STOP: &[&str] = &[
    "キャンセル", "cancel", "いいえ", "no", "戻る", "back", "参照", "browse", "中止", "abort", "ヘルプ", "help", "アンインストール",
    "uninstall", "削除", "remove", "インストール先", "変更", "change", "設定", "settings", "詳細", "custom", "同意しない", "同意しません", "i do not accept", "i don't accept", "decline", "disagree",
];
/// Radio buttons that accept a licence.
const AGREE: &[&str] = &["同意する", "同意します", "i agree", "i accept", "accept", "agree", "はい"];
const DISAGREE: &[&str] = &["同意しない", "同意しません", "not", "don't", "do not", "いいえ"];
/// Check boxes that start something once the setup ends (cleared).
const AFTER: &[&str] = &["起動", "実行", "readme", "read me", "表示", "開く", "見る", "launch", "run ", "start ", "view", "show", "open"];

/// A control's text as compared: no mnemonic "&" (nor its "(&N)"), no "...", "<" or ">", no
/// spaces at the ends, lower case.
pub fn norm(t: &str) -> String {
    let mut s = t.replace("&&", "\u{1}").replace('&', "").replace('\u{1}', "&");
    // "(N)" left of a "(&N)" mnemonic, ASCII letter or digit
    loop {
        let b = s.as_bytes();
        let Some(i) = s.find('(') else { break };
        if b.len() >= i + 3 && b[i + 2] == b')' && b[i + 1].is_ascii_alphanumeric() {
            s.replace_range(i..i + 3, "");
        } else {
            break;
        }
    }
    let s: String = s.chars().filter(|&c| !matches!(c, '<' | '>' | '…' | '＞' | '＜')).collect();
    s.replace("...", "").trim().to_lowercase()
}

/// Whether normalized text `t` is word `w` (or starts with it, for words of more than 3 bytes:
/// "インストール開始", "next >"; short ones such as "ok" and "no" only whole).
fn is(t: &str, w: &str) -> bool {
    t == w || (w.len() > 3 && t.starts_with(w))
}

fn any_of(t: &str, ws: &[&str]) -> bool {
    ws.iter().any(|w| is(t, w))
}

/// The window to act on: the active one when it is an enabled top-level window with controls
/// or a dialog, else the last such (highest handle: the newest).
pub fn front(tops: &[Top]) -> Option<&Top> {
    let usable = |t: &&Top| t.enabled && (t.dialog || !t.ctrls.is_empty());
    tops.iter().filter(usable).find(|t| t.active).or_else(|| tops.iter().filter(usable).max_by_key(|t| t.h))
}

/// What a person taking the defaults does next with these windows (None: nothing to do).
pub fn decide(tops: &[Top]) -> Option<Act> {
    let w = front(tops)?;
    let live = |c: &&Ctrl| c.enabled;
    let click = |c: &Ctrl| Act::Click(c.h, c.at.0, c.at.1, c.text.clone());
    for c in w.ctrls.iter().filter(live) {
        let t = norm(&c.text);
        if c.kind == Kind::Radio && !c.checked && any_of(&t, AGREE) && !DISAGREE.iter().any(|d| t.contains(d)) {
            return Some(click(c));
        }
    }
    for c in w.ctrls.iter().filter(live) {
        let t = norm(&c.text);
        if c.kind == Kind::Check && c.checked && AFTER.iter().any(|a| t.contains(a)) {
            return Some(click(c));
        }
    }
    let buttons: Vec<&Ctrl> = w.ctrls.iter().filter(live).filter(|c| matches!(c.kind, Kind::Push | Kind::DefPush)).collect();
    for g in GO {
        if let Some(c) = buttons.iter().find(|c| {
            let t = norm(&c.text);
            is(&t, g) && !any_of(&t, STOP)
        }) {
            return Some(click(c));
        }
    }
    if let Some(c) = buttons.iter().find(|c| c.kind == Kind::DefPush && !any_of(&norm(&c.text), STOP)) {
        return Some(click(c));
    }
    if w.dialog && buttons.is_empty() {
        return Some(Act::Enter(w.h));
    }
    None
}

/// The program's windows now.
fn snapshot() -> Vec<Top> {
    use crate::user::{with, Window};
    const WS_CHILD: u32 = 0x4000_0000;
    let raw: Vec<(u32, Vec<(u32, u32, String, bool, (i32, i32), bool)>, String, bool, bool, bool)> = with(|s| {
        let shown = |mut h: u32| {
            while let Some(w) = s.wins.get(&h) {
                if !w.visible {
                    return false;
                }
                if w.style & WS_CHILD == 0 {
                    return true;
                }
                h = w.parent;
            }
            false
        };
        let class = |w: &Window| s.classes.get(w.class).map(|c| c.name.to_lowercase()).unwrap_or_default();
        let mut out = Vec::new();
        for (&h, w) in s.wins.iter() {
            if w.style & WS_CHILD != 0 || !w.visible {
                continue;
            }
            let mut ctrls = Vec::new();
            let mut todo = w.children.clone();
            while let Some(k) = todo.pop() {
                let Some(c) = s.wins.get(&k) else { continue };
                todo.extend(c.children.iter().copied());
                if !shown(k) {
                    continue;
                }
                let r = crate::user::client_screen(s, k);
                let at = ((r.l + r.r) / 2, (r.t + r.b) / 2);
                let cls = class(c);
                let kind = if cls == "button" { c.style & 0xf } else { 0x100 };
                ctrls.push((k, kind, String::from_utf16_lossy(&c.text), c.enabled, at, cls == "button"));
            }
            out.push((h, ctrls, String::from_utf16_lossy(&w.text), w.enabled, class(w) == "#32770", s.active == h));
        }
        out
    });
    let mut tops: Vec<Top> = raw
        .into_iter()
        .map(|(h, ctrls, text, enabled, dialog, active)| {
            let mut ctrls: Vec<Ctrl> = ctrls
                .into_iter()
                .map(|(k, style, text, enabled, at, button)| {
                    let kind = match (button, style) {
                        (true, 0) => Kind::Push,
                        (true, 1) => Kind::DefPush,
                        (true, 2 | 3 | 5 | 6) => Kind::Check,
                        (true, 4 | 9) => Kind::Radio,
                        _ => Kind::Other,
                    };
                    let checked = matches!(kind, Kind::Check | Kind::Radio) && crate::user::button_checked(k);
                    Ctrl { h: k, kind, text, checked, enabled, at }
                })
                .collect();
            // (child order as a person reads them: top to bottom, left to right)
            ctrls.sort_by_key(|c| (c.at.1, c.at.0, c.h));
            Top { h, text, dialog, enabled, active, ctrls }
        })
        .collect();
    tops.sort_by_key(|t| t.h);
    tops
}

fn act(a: &Act) {
    use kura::display::*;
    match *a {
        Act::Click(_, x, y, _) => {
            vnhost_input_push(INPUT_MOUSE_MOVE, x, y, 0);
            vnhost_input_push(INPUT_MOUSE_DOWN, x, y, 0);
            vnhost_input_push(INPUT_MOUSE_UP, x, y, 0);
        }
        Act::Enter(_) => {
            // (VK_RETURN, scan code 28)
            vnhost_input_push(INPUT_KEY_DOWN, 0, 0, 0x0d | 28 << 16);
            vnhost_input_push(INPUT_KEY_UP, 0, 0, 0x0d | 28 << 16);
        }
    }
}

/// SAKE_AUTO is set.
pub fn on() -> bool {
    crate::env_on!("SAKE_AUTO")
}

/// A message box's answer under SAKE_AUTO where it differs from sake's own (user::box_answer,
/// Yes): No to a Yes/No question that would start the installed program or open its readme
/// once the setup ends (「インストール完了しました。実行しますか。」), so the setup ends there.
pub fn box_answer(text: &str, style: u32) -> Option<u32> {
    // (MB_YESNOCANCEL 3, MB_YESNO 4)
    if !matches!(style & 0xf, 3 | 4) {
        return None;
    }
    let t = text.to_lowercase();
    const ASKS: &[&str] = &["実行しますか", "起動しますか", "プレイしますか", "始めますか", "表示しますか", "開きますか", "読みますか", "見ますか", "run ", "launch", "start ", "play ", "readme", "view "];
    ASKS.iter().any(|a| t.contains(a)).then_some(7)
}

/// Starts the autopilot when SAKE_AUTO is set (once per process).
pub fn start() {
    if std::env::var_os("SAKE_AUTO").is_none() {
        return;
    }
    let ms: u64 = std::env::var("SAKE_AUTO_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    let repeat: u32 = std::env::var("SAKE_AUTO_REPEAT").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let log = std::env::var_os("SAKE_AUTO_LOG").is_some();
    std::thread::spawn(move || {
        let mut pilot = Pilot::new(repeat);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let tops = snapshot();
            if let Some(a) = pilot.step(&tops) {
                if log {
                    let w = front(&tops).map(|t| t.text.clone()).unwrap_or_default();
                    match &a {
                        Act::Click(_, x, y, t) => eprintln!("sake auto: [{w}] click \"{t}\" at {x},{y}"),
                        Act::Enter(_) => eprintln!("sake auto: [{w}] Enter"),
                    }
                }
                act(&a);
            }
        }
    });
}

/// When to act: the windows unchanged since the last look (settled), the same action on the
/// same windows at most `repeat` times.
pub struct Pilot {
    last: Option<Vec<Top>>,
    done: Option<(Vec<Top>, Act, u32)>,
    repeat: u32,
}

impl Pilot {
    pub fn new(repeat: u32) -> Pilot {
        Pilot { last: None, done: None, repeat }
    }
    pub fn step(&mut self, tops: &[Top]) -> Option<Act> {
        let settled = self.last.as_deref() == Some(tops);
        self.last = Some(tops.to_vec());
        if !settled {
            return None;
        }
        let a = decide(tops)?;
        match &mut self.done {
            Some((w, d, n)) if w.as_slice() == tops && *d == a => {
                if *n >= self.repeat {
                    return None;
                }
                *n += 1;
            }
            _ => self.done = Some((tops.to_vec(), a.clone(), 1)),
        }
        Some(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn b(h: u32, kind: Kind, text: &str) -> Ctrl {
        Ctrl { h, kind, text: text.into(), checked: false, enabled: true, at: (h as i32, 10) }
    }
    fn top(h: u32, ctrls: Vec<Ctrl>) -> Top {
        Top { h, text: "setup".into(), dialog: true, enabled: true, active: false, ctrls }
    }
    fn clicked(a: Option<Act>) -> Option<u32> {
        match a {
            Some(Act::Click(h, ..)) => Some(h),
            Some(Act::Enter(_)) => Some(0),
            None => None,
        }
    }

    #[test]
    fn run_after_install_is_answered_no_other_questions_yes() {
        // BLUEGALE's setup.exe (ミセス・ジャンキー)
        assert_eq!(box_answer("インストール完了しました。\n実行しますか。", 4), Some(7));
        assert_eq!(box_answer("Setup is complete. Launch the game now?", 4), Some(7));
        assert_eq!(box_answer("デスクトップにショートカットを\n作成しますか。", 4), None);
        assert_eq!(box_answer("フォルダが存在しないため、インストール時に作成します。\n続行しますか？", 1), None);
        // (an OK box: nothing to choose)
        assert_eq!(box_answer("実行しますか", 0), None);
    }

    #[test]
    fn texts_are_compared_without_mnemonics_and_arrows() {
        assert_eq!(norm("次へ(&N) >"), "次へ");
        assert_eq!(norm("&Next >"), "next");
        assert_eq!(norm("< 戻る(&B)"), "戻る");
        assert_eq!(norm("参照(&R)..."), "参照");
        assert_eq!(norm("Save && Exit"), "save & exit");
        assert_eq!(norm("  OK  "), "ok");
    }

    #[test]
    fn a_wizard_page_goes_next_not_back_or_cancel() {
        let w = top(1, vec![b(10, Kind::Push, "< 戻る(&B)"), b(11, Kind::DefPush, "次へ(&N) >"), b(12, Kind::Push, "キャンセル")]);
        assert_eq!(clicked(decide(&[w])), Some(11));
    }

    #[test]
    fn install_wins_over_exit_and_ok_over_close() {
        // CatSystem2's INSTALL.exe: インストール / 終了
        let w = top(1, vec![b(10, Kind::Push, "終了"), b(11, Kind::Push, "インストール")]);
        assert_eq!(clicked(decide(&[w])), Some(11));
        let w = top(1, vec![b(10, Kind::Push, "閉じる"), b(11, Kind::Push, "OK")]);
        assert_eq!(clicked(decide(&[w])), Some(11));
    }

    #[test]
    fn changing_the_folder_is_not_installing() {
        // CatSystem2's INSTALL.exe: 「インストール先の変更」 beside 「次へ」
        let w = top(1, vec![b(10, Kind::Push, "インストール先の変更"), b(11, Kind::Push, "次へ(&N) >"), b(12, Kind::Push, "キャンセル")]);
        assert_eq!(clicked(decide(&[w])), Some(11));
    }

    #[test]
    fn uninstall_and_no_are_never_pressed() {
        let w = top(1, vec![b(10, Kind::DefPush, "アンインストール"), b(11, Kind::Push, "No")]);
        assert_eq!(clicked(decide(&[w])), None);
        // ("Now" is not "No"; "Installieren" starts with "install")
        let w = top(1, vec![b(10, Kind::Push, "Uninstall"), b(11, Kind::Push, "Install now")]);
        assert_eq!(clicked(decide(&[w])), Some(11));
    }

    #[test]
    fn a_licence_is_accepted_before_next() {
        let mut no = b(20, Kind::Radio, "同意しない(&D)");
        no.checked = true;
        let w = top(1, vec![b(10, Kind::Radio, "同意する(&A)"), no, b(11, Kind::DefPush, "次へ(&N) >")]);
        assert_eq!(clicked(decide(&[w.clone()])), Some(10));
        let mut w2 = w;
        w2.ctrls[0].checked = true;
        w2.ctrls[1].checked = false;
        assert_eq!(clicked(decide(&[w2])), Some(11));
    }

    #[test]
    fn launching_the_game_afterwards_is_unchecked() {
        let mut run = b(10, Kind::Check, "ゲームを起動する");
        run.checked = true;
        let w = top(1, vec![run, b(11, Kind::DefPush, "完了")]);
        assert_eq!(clicked(decide(&[w])), Some(10));
    }

    #[test]
    fn disabled_buttons_and_windows_are_left() {
        let mut next = b(11, Kind::DefPush, "次へ");
        next.enabled = false;
        let w = top(1, vec![next, b(12, Kind::Push, "キャンセル")]);
        assert_eq!(clicked(decide(&[w])), None);
        // a modal dialog disables its owner: the dialog is the front
        let mut owner = top(1, vec![b(10, Kind::Push, "インストール")]);
        owner.enabled = false;
        let dlg = top(2, vec![b(20, Kind::Push, "OK")]);
        assert_eq!(clicked(decide(&[owner, dlg])), Some(20));
    }

    #[test]
    fn a_dialog_without_buttons_gets_enter_a_plain_window_nothing() {
        assert_eq!(decide(&[top(1, vec![])]), Some(Act::Enter(1)));
        let mut w = top(1, vec![b(10, Kind::Other, "")]);
        w.dialog = false;
        assert_eq!(decide(&[w]), None);
    }

    #[test]
    fn the_pilot_waits_for_windows_to_settle_and_gives_up_repeating() {
        let w = vec![top(1, vec![b(10, Kind::Push, "OK")])];
        let mut p = Pilot::new(2);
        assert_eq!(p.step(&w), None, "first look: not settled");
        assert!(p.step(&w).is_some());
        assert!(p.step(&w).is_some());
        assert_eq!(p.step(&w), None, "same action on the same windows twice already");
        let w2 = vec![top(1, vec![b(10, Kind::Push, "OK"), b(11, Kind::Push, "キャンセル")])];
        assert_eq!(p.step(&w2), None, "changed: not settled");
        assert!(p.step(&w2).is_some());
    }

    fn word() -> impl Strategy<Value = String> {
        let all: Vec<&'static str> = GO.iter().chain(STOP.iter()).copied().chain(["参照", "Next >", "&OK", "x", "", "設定"]).collect();
        (proptest::sample::select(all), 0..3u8).prop_map(|(w, deco)| match deco {
            0 => w.to_string(),
            1 => format!("&{w}"),
            _ => format!("{w}(&Z)"),
        })
    }
    fn ctrl() -> impl Strategy<Value = Ctrl> {
        (0u32..1, 0..5u8, word(), any::<bool>(), any::<bool>()).prop_map(|(h, k, text, checked, enabled)| Ctrl {
            h,
            kind: [Kind::Push, Kind::DefPush, Kind::Check, Kind::Radio, Kind::Other][k as usize],
            text,
            checked,
            enabled,
            at: (h as i32, 0),
        })
    }
    fn tops() -> impl Strategy<Value = Vec<Top>> {
        proptest::collection::vec(
            (1u32..6, any::<bool>(), any::<bool>(), any::<bool>(), proptest::collection::vec(ctrl(), 0..8))
                .prop_map(|(h, dialog, enabled, active, mut ctrls)| {
                    // (handles are unique, as windows' are)
                    for (i, c) in ctrls.iter_mut().enumerate() {
                        c.h = 100 * h + i as u32;
                    }
                    Top { h, text: String::new(), dialog, enabled, active, ctrls }
                }),
            0..4,
        )
    }

    proptest! {
        /// Whatever the windows: a click is on an enabled control of the front window, never on
        /// a STOP button, never unchecks an accepted licence, and Enter only goes to a dialog
        /// with no live push buttons.
        #[test]
        fn decide_never_presses_what_a_person_would_not(ts in tops()) {
            let Some(a) = decide(&ts) else { return Ok(()) };
            let w = front(&ts).expect("an action needs a front window");
            prop_assert!(w.enabled);
            match a {
                Act::Click(h, ..) => {
                    let c = w.ctrls.iter().find(|c| c.h == h).expect("a control of the front window");
                    prop_assert!(c.enabled);
                    let t = norm(&c.text);
                    match c.kind {
                        Kind::Push | Kind::DefPush => prop_assert!(!any_of(&t, STOP), "pressed {:?}", c.text),
                        Kind::Radio => prop_assert!(!c.checked),
                        Kind::Check => prop_assert!(c.checked),
                        Kind::Other => prop_assert!(false, "clicked a non-button"),
                    }
                }
                Act::Enter(h) => {
                    prop_assert_eq!(h, w.h);
                    prop_assert!(w.dialog);
                    prop_assert!(!w.ctrls.iter().any(|c| c.enabled && matches!(c.kind, Kind::Push | Kind::DefPush)));
                }
            }
        }

        /// The pilot never acts on windows it has not seen twice in a row, and at most `repeat`
        /// times on the same unchanged windows.
        #[test]
        fn the_pilot_is_bounded(ts in tops(), n in 1u32..5, looks in 2usize..20) {
            let mut p = Pilot::new(n);
            let acts = (0..looks).filter(|_| p.step(&ts).is_some()).count() as u32;
            prop_assert!(acts <= n);
            prop_assert!(acts as usize <= looks - 1);
            if decide(&ts).is_some() { prop_assert_eq!(acts, n.min(looks as u32 - 1)); }
        }
    }
}
