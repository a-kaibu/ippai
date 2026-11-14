//! The parts of Media Foundation kept free of the guest: the attribute store, the engine's state
//! (the events in order, as the HTML media element's), the frame's letterboxing. The COM side
//! and Windows' answers: tests/winref/probes/mfengine.c.

use super::*;
use proptest::prelude::*;

fn key(n: u8) -> Guid {
    let mut g = [0u8; 16];
    g[0] = n;
    g
}

#[test]
fn store_keeps_the_order_and_replaces_in_place() {
    let mut s = Store::default();
    assert_eq!(s.set(key(1), Val::U32(1)), None);
    assert_eq!(s.set(key(2), Val::Str(vec![104, 105])), None);
    assert_eq!(s.set(key(1), Val::U64(7)), Some(Val::U32(1)));
    assert_eq!(s.items.iter().map(|i| i.0[0]).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(s.get(&key(1)), Some(&Val::U64(7)));
    assert_eq!(s.delete(&key(1)), Some(Val::U64(7)));
    assert_eq!(s.delete(&key(1)), None);
    assert_eq!(s.items.len(), 1);
}

/// mferror.h's codes, as Windows answers them (tests/winref/golden/mfengine-*): a getter of the
/// wrong type, a missing attribute (no callback for CreateInstance), a bad media type
#[test]
fn error_codes_as_windows() {
    assert_eq!(MF_E_INVALIDTYPE, 0xc00d_36bd);
    assert_eq!(MF_E_ATTRIBUTENOTFOUND, 0xc00d_36e6);
    assert_eq!(MF_E_INVALIDMEDIATYPE, 0xc00d_36b4);
    assert_eq!(MF_E_INVALIDREQUEST, 0xc00d_36b2);
    assert_ne!(NO_FRAME_TO_TRANSFER, MF_E_INVALIDREQUEST);
}

#[test]
fn value_types_are_mf_attribute_types() {
    // MF_ATTRIBUTE_UINT32 .. MF_ATTRIBUTE_IUNKNOWN
    let vs = [Val::U32(0), Val::U64(0), Val::F64(0.0), Val::Guid([0; 16]), Val::Str(vec![]), Val::Blob(vec![]), Val::Unk(0)];
    assert_eq!(vs.iter().map(Val::vt).collect::<Vec<_>>(), [19, 21, 5, 72, 31, 0x1011, 13]);
}

#[test]
fn compare_match_types() {
    let mut a = Store::default();
    a.set(key(1), Val::U32(1));
    let mut b = a.clone();
    b.set(key(2), Val::U32(2));
    // a's items are all in b; not the other way round
    assert_eq!(a.matches(&b, 0), Some(true));
    assert_eq!(a.matches(&b, 1), Some(false));
    assert_eq!(a.matches(&b, 2), Some(false));
    assert_eq!(a.matches(&b, 3), Some(true));
    assert_eq!(a.matches(&b, 4), Some(true));
    assert_eq!(b.matches(&a, 4), Some(true));
    assert_eq!(a.matches(&b, 5), None);
    b.set(key(1), Val::U32(9));
    assert_eq!(a.matches(&b, 3), Some(false));
    assert_eq!(a.matches(&a.clone(), 2), Some(true));
}

/// Values of a few keys and kinds.
fn arb_val() -> impl Strategy<Value = Val> {
    prop_oneof![
        any::<u32>().prop_map(Val::U32),
        any::<u64>().prop_map(Val::U64),
        (-1e9f64..1e9).prop_map(Val::F64),
        any::<[u8; 16]>().prop_map(Val::Guid),
        prop::collection::vec(any::<u16>(), 0..6).prop_map(Val::Str),
        prop::collection::vec(any::<u8>(), 0..6).prop_map(Val::Blob),
        any::<u32>().prop_map(Val::Unk),
    ]
}

#[derive(Clone, Debug)]
enum Op {
    Set(u8, Val),
    Delete(u8),
}

fn arb_ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(prop_oneof![(0u8..6, arb_val()).prop_map(|(k, v)| Op::Set(k, v)), (0u8..6).prop_map(Op::Delete)], 0..40)
}

proptest! {
    /// The store against a model: a list of (key, value) in the order keys came in.
    #[test]
    fn store_as_a_model(ops in arb_ops()) {
        let mut s = Store::default();
        let mut m: Vec<(u8, Val)> = Vec::new();
        for op in ops {
            match op {
                Op::Set(k, v) => {
                    let old = m.iter().position(|e| e.0 == k).map(|i| std::mem::replace(&mut m[i].1, v.clone()));
                    if old.is_none() {
                        m.push((k, v.clone()));
                    }
                    prop_assert_eq!(s.set(key(k), v), old);
                }
                Op::Delete(k) => {
                    let old = m.iter().position(|e| e.0 == k).map(|i| m.remove(i).1);
                    prop_assert_eq!(s.delete(&key(k)), old);
                }
            }
            prop_assert_eq!(s.items.len(), m.len());
            for (i, (k, v)) in m.iter().enumerate() {
                prop_assert_eq!(&s.items[i].0, &key(*k));
                prop_assert_eq!(&s.items[i].1, v);
            }
        }
    }

    /// Compare's types as set relations: all items = both ways, the intersection = no key
    /// with two values, the smaller = the smaller one's way.
    #[test]
    fn compare_as_relations(a in arb_ops(), b in arb_ops()) {
        let build = |ops: &[Op]| {
            let mut s = Store::default();
            for op in ops {
                match op {
                    Op::Set(k, v) => { s.set(key(*k), v.clone()); }
                    Op::Delete(k) => { s.delete(&key(*k)); }
                }
            }
            s
        };
        let (x, y) = (build(&a), build(&b));
        let ours = x.matches(&y, 0).unwrap();
        let theirs = x.matches(&y, 1).unwrap();
        prop_assert_eq!(theirs, y.matches(&x, 0).unwrap());
        prop_assert_eq!(x.matches(&y, 2).unwrap(), ours && theirs);
        let clash = x.items.iter().any(|(k, v)| y.get(k).is_some_and(|w| w != v));
        prop_assert_eq!(x.matches(&y, 3).unwrap(), !clash);
        let smaller = if x.items.len() <= y.items.len() { ours } else { theirs };
        prop_assert_eq!(x.matches(&y, 4).unwrap(), smaller);
        prop_assert!(x.matches(&x, 2).unwrap());
    }
}

fn names(st: &mut State) -> Vec<u32> {
    st.events.drain(..).map(|e| e.0).collect()
}

#[test]
fn load_events_in_order() {
    let mut st = State::default();
    st.reset();
    assert!(!st.loaded(2.0, Some((64, 48)), false));
    use ev::*;
    assert_eq!(names(&mut st), [LOADSTART, DURATIONCHANGE, LOADEDMETADATA, LOADEDDATA, FIRSTFRAMEREADY, CANPLAY, CANPLAYTHROUGH]);
    assert_eq!((st.ready, st.network, st.paused, st.ended), (4, 1, true, false));
    assert_eq!(st.duration, 2.0);
    // a second source: emptied first
    st.reset();
    assert_eq!(names(&mut st), [EMPTIED, LOADSTART]);
    assert!(st.duration.is_nan());
    assert_eq!(st.ready, 0);
}

#[test]
fn a_source_that_fails() {
    let mut st = State::default();
    st.reset();
    st.failed(0x8007_0002);
    assert_eq!(st.events.iter().copied().collect::<Vec<_>>(), [(ev::LOADSTART, 0, 0), (ev::ERROR, 4, 0x8007_0002)]);
    assert_eq!((st.error, st.network, st.ready), ((4, 0x8007_0002), 3, 0));
    // nothing to seek in or play
    assert!(!st.seek());
    st.events.clear();
    assert_eq!(st.play(), (false, false));
    assert_eq!(names(&mut st), [ev::PLAY]);
    // a good source clears the error
    st.reset();
    assert_eq!(st.error, (0, 0));
}

#[test]
fn autoplay_and_play_before_load() {
    use ev::*;
    let mut st = State { autoplay: true, ..State::default() };
    st.reset();
    assert!(st.loaded(1.0, Some((2, 2)), true));
    assert_eq!(names(&mut st), [LOADSTART, DURATIONCHANGE, LOADEDMETADATA, LOADEDDATA, FIRSTFRAMEREADY, CANPLAY, PLAY, PLAYING, CANPLAYTHROUGH]);
    let mut st = State::default();
    st.reset();
    st.play();
    assert!(st.loaded(1.0, None, true));
    assert_eq!(names(&mut st), [LOADSTART, PLAY, DURATIONCHANGE, LOADEDMETADATA, LOADEDDATA, CANPLAY, PLAYING, CANPLAYTHROUGH]);
}

#[test]
fn play_to_the_end_and_again() {
    use ev::*;
    let mut st = State::default();
    st.reset();
    st.loaded(1.25, Some((64, 48)), false);
    st.events.clear();
    assert!(st.seek());
    assert_eq!(names(&mut st), [SEEKING, TIMEUPDATE, SEEKED]);
    assert_eq!(st.play(), (false, true));
    assert_eq!(names(&mut st), [PLAY, PLAYING]);
    assert!(!st.end());
    assert_eq!(names(&mut st), [TIMEUPDATE, PAUSE, ENDED]);
    assert!(st.paused && st.ended);
    // from the end: back to the start
    assert_eq!(st.play(), (true, true));
    assert_eq!(names(&mut st), [SEEKING, TIMEUPDATE, SEEKED, PLAY, PLAYING]);
    assert!(!st.ended);
    st.pause();
    st.pause();
    assert_eq!(names(&mut st), [TIMEUPDATE, PAUSE]);
    // looping: no end
    st.looping = true;
    st.play();
    st.events.clear();
    assert!(st.end());
    assert_eq!(names(&mut st), [SEEKING, TIMEUPDATE, SEEKED]);
    assert!(!st.paused && !st.ended);
}

#[derive(Clone, Debug)]
enum Do {
    Reset,
    Fail,
    Load(bool),
    Play,
    Pause,
    Seek,
    End,
    Loop(bool),
    Autoplay(bool),
}

fn arb_do() -> impl Strategy<Value = Do> {
    prop_oneof![
        Just(Do::Reset),
        Just(Do::Fail),
        any::<bool>().prop_map(Do::Load),
        Just(Do::Play),
        Just(Do::Pause),
        Just(Do::Seek),
        Just(Do::End),
        any::<bool>().prop_map(Do::Loop),
        any::<bool>().prop_map(Do::Autoplay),
    ]
}

proptest! {
    /// Any calls: PLAY goes out exactly when it stops being paused (and PAUSE when it becomes
    /// paused, but for the load's reset), PLAYING only with data, ENDED only not looping (paused
    /// and ended then), seeking never left on, the ready state 0 or 4.
    #[test]
    fn state_events_match_its_changes(ops in prop::collection::vec(arb_do(), 0..60)) {
        let mut st = State::default();
        for op in ops {
            let (paused, ready, looping) = (st.paused, st.ready, st.looping);
            // (a load or failure only follows a reset, as load() does)
            match op {
                Do::Reset => st.reset(),
                Do::Fail => { st.reset(); st.failed(MF_E_UNSUPPORTED_BYTESTREAM_TYPE); }
                Do::Load(video) => { st.reset(); st.loaded(1.0, video.then_some((4, 4)), false); }
                Do::Play => { st.play(); }
                Do::Pause => st.pause(),
                Do::Seek => { prop_assert_eq!(st.seek(), ready > 0); }
                Do::End => { if ready > 0 { st.end(); } }
                Do::Loop(v) => st.looping = v,
                Do::Autoplay(v) => st.autoplay = v,
            }
            let e = names(&mut st);
            let count = |x: u32| e.iter().filter(|&&y| y == x).count();
            let reset = matches!(op, Do::Reset | Do::Fail | Do::Load(_));
            prop_assert!(count(ev::PLAY) <= 1 && count(ev::PAUSE) <= 1);
            if !reset {
                prop_assert_eq!(count(ev::PLAY) == 1, paused && !st.paused, "{:?} {:?}", op, e);
                prop_assert_eq!(count(ev::PAUSE) == 1, !paused && st.paused, "{:?} {:?}", op, e);
            }
            if count(ev::PLAYING) > 0 {
                prop_assert!(st.ready >= 3 && !st.paused);
            }
            if count(ev::ENDED) > 0 {
                prop_assert!(!looping && st.paused && st.ended);
                prop_assert_eq!(e.last(), Some(&ev::ENDED));
            }
            if count(ev::ERROR) > 0 {
                prop_assert_eq!(st.error.0, 4);
                prop_assert_eq!(st.network, 3);
            }
            prop_assert!(!st.seeking);
            prop_assert!(st.ready == 0 || st.ready == 4);
            prop_assert_eq!(st.duration.is_nan(), st.ready == 0);
            if st.ended {
                prop_assert!(st.paused);
            }
            // seeking and seeked come in pairs, seeking first
            let s: Vec<_> = e.iter().filter(|&&x| x == ev::SEEKING || x == ev::SEEKED).collect();
            prop_assert!(s.chunks(2).all(|p| p == [&ev::SEEKING, &ev::SEEKED]));
        }
    }
}

#[test]
fn volume_as_centibels() {
    assert_eq!(centibels(1.0, false), 0);
    assert_eq!(centibels(0.1, false), -2000);
    assert_eq!(centibels(0.5, false), -602);
    assert_eq!(centibels(0.0, false), -10000);
    assert_eq!(centibels(1e-9, false), -10000);
    assert_eq!(centibels(1.0, true), -10000);
    assert_eq!(centibels(f64::NAN, false), -10000);
    assert_eq!(centibels(2.0, false), 0);
}

#[test]
fn source_urls_as_paths() {
    let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
    assert_eq!(source_path(&w("C:\\game\\op.mp4")), "C:\\game\\op.mp4");
    assert_eq!(source_path(&w("file:///C:/game/my%20op.mp4")), "C:\\game\\my op.mp4");
    assert_eq!(source_path(&w("movie/op.wmv")), "movie/op.wmv");
}

#[test]
fn frame_letterboxed_with_the_border() {
    // a 2x1 picture onto 4x4: rows 1-2 the picture (each pixel twice), the rest the border
    let f = [0x0011_2233, 0x0044_5566];
    let out = compose(&f, 2, 1, (0, 0, 2, 1), (4, 4), 0x8001_0203, false);
    let b = 0x8001_0203;
    let (p, q) = (0xff11_2233, 0xff44_5566);
    assert_eq!(out, [b, b, b, b, p, p, q, q, p, p, q, q, b, b, b, b]);
    // R8G8B8A8: red and blue swapped
    let out = compose(&f, 2, 1, (0, 0, 2, 1), (2, 1), 0, true);
    assert_eq!(out, [0xff33_2211, 0xff66_5544]);
    // nothing of the picture: all border
    let out = compose(&f, 2, 1, (0, 0, 0, 0), (2, 2), 7, false);
    assert_eq!(out, [7; 4]);
}

proptest! {
    /// Any picture onto any rectangle: the letterbox has the picture's pixels (alpha set),
    /// everything else the border.
    #[test]
    fn compose_fills_all(fw in 1u32..9, fh in 1u32..9, dw in 1u32..17, dh in 1u32..17, border in any::<u32>()) {
        let f: Vec<u32> = (0..fw * fh).map(|i| 0x0100_0000 + i).collect();
        let out = compose(&f, fw, fh, (0, 0, fw as i32, fh as i32), (dw, dh), border, false);
        prop_assert_eq!(out.len(), (dw * dh) as usize);
        let lb = letterbox((0, 0, dw as i32, dh as i32), fw as i32, fh as i32);
        for y in 0..dh as i32 {
            for x in 0..dw as i32 {
                let v = out[(y * dw as i32 + x) as usize];
                let inside = x >= lb.0 && x < lb.0 + lb.2 && y >= lb.1 && y < lb.1 + lb.3;
                if inside {
                    prop_assert_eq!(v >> 24, 0xff);
                    prop_assert!(f.contains(&(v & 0x00ff_ffff | 0x0100_0000)));
                } else {
                    prop_assert_eq!(v, border);
                }
            }
        }
    }
}

/// TransferVideoFrame's letterboxing as it was before the frames were kept (a fresh Vec per
/// call): the model the kept buffers and the straight copy must match.
fn compose_model(frame: &[u32], fw: u32, fh: u32, src: play::R, dst: (u32, u32), border: u32, rgba: bool) -> Vec<u32> {
    let (dw, dh) = (dst.0 as i32, dst.1 as i32);
    let mut out = vec![border; (dw * dh) as usize];
    let lb = letterbox((0, 0, dw, dh), src.2, src.3);
    if lb.2 > 0 && lb.3 > 0 {
        let mut part = vec![0u32; (lb.2 * lb.3) as usize];
        play::blit(frame, fw as i32, fh as i32, src, &mut part, lb, lb);
        for y in 0..lb.3 {
            let o = ((lb.1 + y) * dw + lb.0) as usize;
            out[o..o + lb.2 as usize].copy_from_slice(&part[(y * lb.2) as usize..((y + 1) * lb.2) as usize]);
        }
    }
    if rgba {
        for p in &mut out {
            *p = (*p & 0xff00_ff00) | (*p & 0xff) << 16 | (*p >> 16) & 0xff;
        }
    }
    out
}

#[test]
fn picture_sizes_past_any_texture_are_refused() {
    assert_eq!(picture_pixels(1920, 1080), Some(1920 * 1080));
    assert_eq!(picture_pixels(16384, 16384), Some(16384 * 16384));
    // (65536 x 65536 is 0 in u32: the old vec![0; w * h] was empty and the decoder wrote past it)
    assert_eq!(picture_pixels(65536, 65536), None);
    assert_eq!(picture_pixels(0x1_0001, 0xffff), None);
    assert_eq!(picture_pixels(0, 480), None);
    assert_eq!(picture_pixels(16385, 16384), None);
}

#[test]
fn whole_picture_at_its_size_straight_across() {
    let f: Vec<u32> = (0..6).map(|i| 0x0010_2030 * i).collect();
    for rgba in [false, true] {
        assert_eq!(compose(&f, 3, 2, (0, 0, 3, 2), (3, 2), 0x1234, rgba), compose_model(&f, 3, 2, (0, 0, 3, 2), (3, 2), 0x1234, rgba));
    }
    // one column wider than the picture: not the straight copy (the letterbox starts at 0 there)
    assert_eq!(compose(&f, 3, 2, (0, 0, 3, 2), (4, 2), 9, false), compose_model(&f, 3, 2, (0, 0, 3, 2), (4, 2), 9, false));
}

fn arb_transfer() -> impl Strategy<Value = (u32, u32, (i32, i32, i32, i32), (u32, u32), u32, bool)> {
    (1u32..9, 1u32..9).prop_flat_map(|(fw, fh)| {
        let full = Just((0, 0, fw as i32, fh as i32));
        let part = (0..fw as i32, 0..fh as i32, 0..=fw as i32, 0..=fh as i32);
        (Just(fw), Just(fh), prop_oneof![full, part], (1u32..13, 1u32..13), any::<u32>(), any::<bool>())
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]
    /// One engine's kept buffers across any sequence of transfers (pictures, rectangles and
    /// formats changing between calls, the buffers left as the last call had them): every frame
    /// as the model makes it fresh, the straight copy included.
    #[test]
    fn kept_frames_match_fresh_ones(calls in prop::collection::vec(arb_transfer(), 1..12)) {
        let mut kept = Frames::default();
        for (fw, fh, src, dst, border, rgba) in calls {
            let f: Vec<u32> = (0..fw * fh).map(|i| i.wrapping_mul(0x9e37_79b9)).collect();
            kept.compose(&f, fw, fh, src, dst, border, rgba);
            prop_assert_eq!(&kept.out, &compose_model(&f, fw, fh, src, dst, border, rgba));
        }
    }
}

/// An engine of no movie for the lifetime tests: its block, held once, its events thread
/// running or not.
fn test_engine(thread: bool) -> u32 {
    let b = object(0, 1);
    // (a block let go by an earlier test may come back)
    RETIRED.lock().unwrap().retain(|&x| x != b);
    let e = Engine { refs: 1, st: State::default(), notify: 0, shut: false, src: None, movie: false, ticked: None, category: 0, role: 0, thread, frames: Frames::default() };
    ENGINES.lock().unwrap().get_or_insert_with(HashMap::new).insert(b, e);
    b
}
fn retired(b: u32) -> usize {
    RETIRED.lock().unwrap().iter().filter(|&&x| x == b).count()
}
fn live(b: u32) -> bool {
    ENGINES.lock().unwrap().get_or_insert_with(HashMap::new).contains_key(&b)
}

#[test]
fn an_engine_goes_with_its_last_release() {
    let _g = crate::test_guest::enter();
    // no events thread: at once
    let b = test_engine(false);
    assert_eq!(crate::test_guest::call(en_add_ref, &[b]), 2);
    assert_eq!(crate::test_guest::call(en_release, &[b]), 1);
    assert!(live(b));
    assert_eq!(crate::test_guest::call(en_release, &[b]), 0);
    assert!(!live(b));
    assert_eq!(retired(b), 1);
    // a stray Release after that touches nothing
    assert_eq!(crate::test_guest::call(en_release, &[b]), 0);
    assert_eq!(retired(b), 1);
}

#[derive(Debug, Clone, Copy)]
enum Life {
    AddRef,
    Release,
    Shutdown,
    /// the events thread sees the shutdown and ends (only after one)
    ThreadEnds,
}

proptest! {
    /// Any order of AddRef, Release, Shutdown and the events thread's end (formal/tla/
    /// MfEngineLife.tla checks the same with the thread racing): the block is let go exactly
    /// once, only when the game holds it no more and its thread is gone, and always then.
    #[test]
    fn engines_go_once_and_only_when_unheld(ops in prop::collection::vec(prop_oneof![Just(Life::AddRef), Just(Life::Release), Just(Life::Shutdown), Just(Life::ThreadEnds)], 0..24)) {
        let _g = crate::test_guest::enter();
        let b = test_engine(true);
        let (mut refs, mut thread, mut shut) = (1u32, true, false);
        for op in ops {
            match op {
                Life::AddRef if refs > 0 => {
                    refs += 1;
                    prop_assert_eq!(crate::test_guest::call(en_add_ref, &[b]), refs);
                }
                Life::Release if refs > 0 => {
                    refs -= 1;
                    shut |= refs == 0;
                    prop_assert_eq!(crate::test_guest::call(en_release, &[b]), refs);
                }
                Life::Shutdown if refs > 0 => {
                    shut = true;
                    crate::test_guest::call(en_shutdown, &[b]);
                }
                Life::ThreadEnds if thread && shut => {
                    // (what events_thread does once next_event says Err)
                    prop_assert_eq!(next_event(b, std::time::Duration::ZERO), Some(Err(())));
                    engine(b, |e| e.thread = false);
                    retire(b);
                    thread = false;
                }
                _ => {}
            }
            let gone = refs == 0 && !thread;
            prop_assert_eq!(live(b), !gone);
            prop_assert_eq!(retired(b), gone as usize);
        }
        // (an engine left held: let it go for the next case)
        if live(b) {
            engine(b, |e| { e.refs = 0; e.thread = false; e.shut = true; });
            retire(b);
        }
        RETIRED.lock().unwrap().retain(|&x| x != b);
    }
}

#[test]
fn the_events_thread_sleeps_until_an_event() {
    let _g = crate::test_guest::enter();
    let b = test_engine(true);
    // nothing to tell: it waits the whole wait
    let t = std::time::Instant::now();
    assert_eq!(next_event(b, std::time::Duration::from_millis(60)), Some(Ok((0, None))));
    assert!(t.elapsed() >= std::time::Duration::from_millis(55));
    // an event from another thread wakes it at once (not at the end of a 10 s wait)
    let poster = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        engine(b, |e| e.st.play());
    });
    let t = std::time::Instant::now();
    let got = next_event(b, std::time::Duration::from_secs(10));
    assert!(matches!(got, Some(Ok((0, Some(_))))), "{got:?}");
    assert!(t.elapsed() < std::time::Duration::from_secs(5));
    poster.join().unwrap();
    // and so does the shutdown
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        engine(b, |e| {
            e.shut = true;
            e.refs = 0;
        });
    });
    let t = std::time::Instant::now();
    loop {
        match next_event(b, std::time::Duration::from_secs(10)) {
            Some(Err(())) => break,
            Some(Ok((_, Some(_)))) => continue,
            other => panic!("{other:?}"),
        }
    }
    assert!(t.elapsed() < std::time::Duration::from_secs(5));
    closer.join().unwrap();
    engine(b, |e| e.thread = false);
    assert!(retire(b));
}

#[test]
fn blobs_past_the_limit_are_out_of_memory() {
    let _g = crate::test_guest::enter();
    assert_eq!(blob(0x1000, MAX_BLOB + 1), Err(E_OUTOFMEMORY));
    assert_eq!(blob(0, 4), Err(E_POINTER));
    assert_eq!(blob(0, 0), Ok(Vec::new()));
    let p = mem::heap_alloc(4, true);
    mem::write(p, &[1, 2, 3, 4]);
    assert_eq!(blob(p, 4), Ok(vec![1, 2, 3, 4]));
    mem::heap_free(p);
}
