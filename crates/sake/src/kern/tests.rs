//! The pipe model against Windows' answers (tests/winref/golden/pipes-32, the same steps at the
//! kernel's level) and against properties any pipe keeps (proptest).

use super::*;
use proptest::prelude::*;

const PID: u32 = 7;

impl Kern {
    /// One WriteFile asked once (PENDING: it waits for the reader)
    fn write1(&mut self, e: u32, data: &[u8]) -> Result<u32, u32> {
        self.write(e, data, &mut Write::default())
    }
}

fn named(k: &mut Kern, name: &str, access: u32, msg: bool, max: u32) -> Result<u32, u32> {
    let p = NewPipe { access, msg_type: msg, msg_read: msg, nonblock: false, max, outsize: 512, insize: 512, timeout: 0, first: false };
    k.create(name, &p, PID)
}
fn rd(k: &mut Kern, e: u32, n: u32) -> Result<(String, bool), u32> {
    k.read(e, n).map(|(d, m)| (String::from_utf8(d).unwrap(), m))
}
fn ok(s: &str, more: bool) -> Result<(String, bool), u32> {
    Ok((s.to_string(), more))
}

#[test]
fn names() {
    assert_eq!(pipe_name(r"\\.\pipe\Foo"), Some("foo".into()));
    assert_eq!(pipe_name(r"\\.\PIPE\"), Some("".into()));
    assert_eq!(pipe_name("//./pipe/x"), Some("x".into()));
    assert_eq!(pipe_name(r"\\?\pipe\a\b"), Some(r"a\b".into()));
    assert_eq!(pipe_name(r"C:\notapipe"), None);
    assert_eq!(pipe_name(r"\\.\pipex"), None);
}

#[test]
fn anonymous() {
    let mut k = Kern::default();
    let (r, w) = k.create_anon(0, PID).unwrap();
    assert_eq!(k.write1(w, b"abc"), Ok(3));
    assert_eq!(k.peek(r, 4), Ok(Peek { data: b"abc".to_vec(), avail: 3, msg_len: 0 }));
    assert_eq!(rd(&mut k, r, 2), ok("ab", false));
    assert_eq!(rd(&mut k, r, 10), ok("c", false));
    assert_eq!(k.write1(w, b""), Ok(0));
    assert_eq!(k.peek(r, 4), Ok(Peek { data: vec![], avail: 0, msg_len: 0 }));
    assert_eq!(k.read(r, 4), Err(PENDING));
    k.release(w, PID);
    assert_eq!(k.read(r, 4), Err(PIPE_BROKEN));
    assert_eq!(k.peek(r, 4), Err(PIPE_BROKEN));
    k.release(r, PID);
    assert!(k.ends.is_empty() && k.pipes.is_empty());
    let (r, w) = k.create_anon(0, PID).unwrap();
    k.release(r, PID);
    assert_eq!(k.write1(w, b"q"), Err(PIPE_CLOSING));
    assert_eq!(win32(PIPE_CLOSING), 232);
    // what the reader has not read yet stays after the writer closed
    let (r, w) = k.create_anon(0, PID).unwrap();
    k.write1(w, b"left").unwrap();
    k.release(w, PID);
    assert_eq!(rd(&mut k, r, 10), ok("left", false));
    assert_eq!(k.read(r, 10), Err(PIPE_BROKEN));
}

#[test]
fn create_errors() {
    let mut k = Kern::default();
    assert_eq!(named(&mut k, "", 3, false, 1), Err(OBJECT_NAME_INVALID));
    assert_eq!(named(&mut k, "x", 3, false, 0), Err(INVALID_PARAMETER));
    assert_eq!(named(&mut k, "x", 0, false, 1), Err(INVALID_PARAMETER));
    assert_eq!(named(&mut k, "x", 3, false, 256), Err(INVALID_PARAMETER));
    assert_eq!(k.open("none", true, true, PID), Err(OBJECT_NAME_NOT_FOUND));
    assert_eq!(k.wait_named("none"), Err(OBJECT_NAME_NOT_FOUND));
}

#[test]
fn byte_mode() {
    let mut k = Kern::default();
    let s = named(&mut k, "byte", 3, false, 1).unwrap();
    assert_eq!(named(&mut k, "byte", 3, false, 1), Err(INSTANCE_NOT_AVAILABLE));
    assert_eq!(k.wait_named("byte"), Ok(None));
    assert_eq!(k.peek(s, 4), Err(INVALID_PIPE_STATE));
    assert_eq!(win32(INVALID_PIPE_STATE), 230);
    let c = k.open("byte", true, true, PID).unwrap();
    assert_eq!(k.open("byte", true, true, PID), Err(PIPE_NOT_AVAILABLE));
    assert_eq!(k.wait_named("byte"), Ok(Some(50)));
    assert_eq!(k.listen(s), PIPE_CONNECTED);
    k.write1(c, b"hello").unwrap();
    k.write1(c, b"world").unwrap();
    let p = k.peek(s, 4).unwrap();
    assert_eq!((p.data.as_slice(), p.avail, p.msg_len), (&b"hell"[..], 10, 0));
    assert_eq!(rd(&mut k, s, 3), ok("hel", false));
    assert_eq!(rd(&mut k, s, 100), ok("loworld", false));
    k.write1(s, b"reply").unwrap();
    assert_eq!(rd(&mut k, c, 100), ok("reply", false));
    let i = k.info(s).unwrap();
    assert_eq!((i.server, i.msg_type, i.outsize, i.insize, i.max, i.instances), (true, false, 512, 512, 1, Some(1)));
    assert!(!k.info(c).unwrap().server);
    assert_eq!(k.set_mode(c, Some(true), None), INVALID_PARAMETER);
    k.release(c, PID);
    assert_eq!(k.read(s, 10), Err(PIPE_BROKEN));
    assert_eq!(k.peek(s, 4), Err(PIPE_BROKEN));
    assert_eq!(k.write1(s, b"late"), Err(PIPE_CLOSING));
    assert_eq!(k.listen(s), PIPE_CLOSING);
    assert_eq!(k.disconnect(s), SUCCESS);
    assert_eq!(k.disconnect(s), PIPE_DISCONNECTED);
    assert_eq!(k.read(s, 10), Err(PIPE_DISCONNECTED));
    // disconnected is not listening: no client gets in before the next ConnectNamedPipe
    assert_eq!(k.wait_named("byte"), Ok(Some(50)));
    assert_eq!(k.open("byte", true, true, PID), Err(PIPE_NOT_AVAILABLE));
    assert_eq!(k.listen(s), PENDING);
    assert_eq!(k.connected(s), PENDING);
    let c2 = k.open("byte", true, true, PID).unwrap();
    assert_eq!(k.connected(s), SUCCESS);
    k.release(c2, PID);
    k.release(s, PID);
    assert_eq!(k.open("byte", true, true, PID), Err(OBJECT_NAME_NOT_FOUND));
}

#[test]
fn disconnect_cuts_the_client_off() {
    let mut k = Kern::default();
    let s = named(&mut k, "d", 3, false, 1).unwrap();
    let c = k.open("d", true, true, PID).unwrap();
    k.write1(s, b"lost").unwrap();
    assert_eq!(k.disconnect(s), SUCCESS);
    assert_eq!(k.read(c, 10), Err(PIPE_DISCONNECTED));
    assert_eq!(k.write1(c, b"x"), Err(PIPE_DISCONNECTED));
    assert_eq!(k.peek(c, 4), Err(PIPE_DISCONNECTED));
    assert_eq!(k.info(c).unwrap().instances, None);
    // a new client of the same instance is not the old one
    assert_eq!(k.listen(s), PENDING);
    let c2 = k.open("d", true, true, PID).unwrap();
    k.write1(s, b"new").unwrap();
    assert_eq!(rd(&mut k, c2, 10), ok("new", false));
    assert_eq!(k.read(c, 10), Err(PIPE_DISCONNECTED));
    assert_eq!(k.listen(s), PIPE_CONNECTED);
    assert_eq!(k.disconnect(c2), ILLEGAL_FUNCTION);
}

#[test]
fn message_mode() {
    let mut k = Kern::default();
    let m1 = named(&mut k, "msg", 3, true, 2).unwrap();
    let m2 = named(&mut k, "msg", 3, true, 2).unwrap();
    assert_eq!(named(&mut k, "msg", 3, true, 2), Err(INSTANCE_NOT_AVAILABLE));
    assert_eq!(named(&mut k, "msg", 3, false, UNLIMITED), Err(INSTANCE_NOT_AVAILABLE));
    let c1 = k.open("msg", true, true, PID).unwrap();
    let c2 = k.open("msg", true, true, PID).unwrap();
    assert_eq!(k.open("msg", true, true, PID), Err(PIPE_NOT_AVAILABLE));
    k.write1(c1, b"first message").unwrap();
    k.write1(c1, b"second").unwrap();
    k.write1(c2, b"to two").unwrap();
    let p = k.peek(m1, 4).unwrap();
    assert_eq!((p.data.as_slice(), p.avail, p.msg_len), (&b"firs"[..], 19, 13));
    assert_eq!(rd(&mut k, m1, 5), ok("first", true));
    let p = k.peek(m1, 4).unwrap();
    assert_eq!((p.data.as_slice(), p.avail, p.msg_len), (&b" mes"[..], 14, 8));
    assert_eq!(rd(&mut k, m1, 100), ok(" message", false));
    assert_eq!(rd(&mut k, m1, 100), ok("second", false));
    assert_eq!(rd(&mut k, m2, 100), ok("to two", false));
    assert_eq!(k.peek(c1, 4), Ok(Peek { data: vec![], avail: 0, msg_len: 0 }));
    // clients read bytes until they ask for messages
    let i = k.info(c1).unwrap();
    assert_eq!((i.msg_read, i.instances), (false, Some(2)));
    assert_eq!(k.set_mode(c1, Some(true), None), SUCCESS);
    k.write1(m1, b"ab").unwrap();
    k.write1(m1, b"cd").unwrap();
    assert_eq!(rd(&mut k, c1, 1), ok("a", true));
    assert_eq!(rd(&mut k, c1, 10), ok("b", false));
    assert_eq!(rd(&mut k, c1, 10), ok("cd", false));
    assert_eq!(k.set_mode(m2, Some(false), None), SUCCESS);
    k.write1(c2, b"xy").unwrap();
    k.write1(c2, b"zw").unwrap();
    assert_eq!(rd(&mut k, m2, 10), ok("xyzw", false));
    k.write1(m1, b"pending").unwrap();
    assert_eq!(k.transact(c1, b"q"), PIPE_BUSY);
    assert_eq!(rd(&mut k, c1, 100), ok("pending", false));
    assert_eq!(k.transact(c1, b"q"), SUCCESS);
    assert_eq!(rd(&mut k, m1, 100), ok("q", false));
    assert_eq!(k.transact(m2, b"q"), INVALID_READ_MODE);
    // an empty message is a message
    k.write1(m1, b"").unwrap();
    assert_eq!(rd(&mut k, c1, 100), ok("", false));
    assert_eq!(k.read(c1, 100), Err(PENDING));
    k.release(c1, PID);
    k.release(c2, PID);
    assert_eq!(k.read(m1, 10), Err(PIPE_BROKEN));
    assert_eq!(k.write1(m1, b"x"), Err(PIPE_CLOSING));
}

#[test]
fn one_way() {
    let mut k = Kern::default();
    let s = named(&mut k, "in", 1, false, 1).unwrap();
    assert_eq!(k.open("in", true, true, PID), Err(ACCESS_DENIED));
    let c = k.open("in", false, true, PID).unwrap();
    k.write1(c, b"up").unwrap();
    assert_eq!(rd(&mut k, s, 10), ok("up", false));
    let s = named(&mut k, "out", 2, false, 1).unwrap();
    assert_eq!(k.open("out", false, true, PID), Err(ACCESS_DENIED));
    assert!(k.open("out", true, false, PID).is_ok());
    let _ = s;
    // a second instance must share the same way
    assert_eq!(named(&mut k, "out", 3, false, 2), Err(INSTANCE_NOT_AVAILABLE));
    let p = NewPipe { access: 3, msg_type: false, msg_read: false, nonblock: false, max: 2, outsize: 0, insize: 0, timeout: 0, first: false };
    k.create("two", &p, PID).unwrap();
    assert_eq!(k.create("two", &NewPipe { access: 1, ..p }, PID), Err(ACCESS_DENIED));
    assert_eq!(k.create("two", &NewPipe { first: true, ..p }, PID), Err(ACCESS_DENIED));
}

#[test]
fn nonblocking() {
    let mut k = Kern::default();
    let p = NewPipe { access: 3, msg_type: false, msg_read: false, nonblock: true, max: 1, outsize: 0, insize: 0, timeout: 0, first: false };
    let s = k.create("nb", &p, PID).unwrap();
    assert_eq!(k.listen(s), PIPE_LISTENING);
    let c = k.open("nb", true, true, PID).unwrap();
    assert_eq!(k.read(s, 1), Err(PIPE_EMPTY));
    assert_eq!(k.read(c, 1), Err(PENDING));
    assert_eq!(k.set_mode(c, None, Some(true)), SUCCESS);
    assert_eq!(k.read(c, 1), Err(PIPE_EMPTY));
}

#[test]
fn processes() {
    let mut k = Kern::default();
    let (r, w) = k.create_anon(0, 1).unwrap();
    // the write end inherited by process 2, then closed by process 1
    k.add_ref(w, 2);
    k.release(w, 1);
    k.write1(w, b"from 2").unwrap();
    assert_eq!(rd(&mut k, r, 10), ok("from 2", false));
    k.release_process(2);
    assert_eq!(k.read(r, 10), Err(PIPE_BROKEN));
    k.release_process(1);
    assert!(k.ends.is_empty() && k.pipes.is_empty());
}

#[derive(Debug, Clone)]
enum Op {
    Write(Vec<u8>),
    Read(u32),
    Peek(u32),
}
fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(prop_oneof![prop::collection::vec(any::<u8>(), 0..20).prop_map(Op::Write), (0u32..30).prop_map(Op::Read), (0u32..30).prop_map(Op::Peek),], 0..60)
}

proptest! {
    /// A byte pipe is a stream: what is read is what was written, in order, whatever the
    /// writes' and reads' sizes; peeking changes nothing; avail is what is left.
    #[test]
    fn byte_stream(ops in ops()) {
        let mut k = Kern::default();
        let (r, w) = k.create_anon(0, PID).unwrap();
        let (mut written, mut read) = (Vec::new(), Vec::new());
        for op in ops {
            match op {
                Op::Write(d) => { prop_assert_eq!(k.write1(w, &d), Ok(d.len() as u32)); written.extend(d); }
                Op::Read(n) => match k.read(r, n) {
                    Ok((d, more)) => { prop_assert!(!more); prop_assert!(d.len() <= n as usize); prop_assert!(!d.is_empty() || n == 0); read.extend(d); }
                    Err(e) => { prop_assert_eq!(e, PENDING); prop_assert_eq!(read.len(), written.len()); }
                },
                Op::Peek(n) => {
                    let p = k.peek(r, n).unwrap();
                    let left = &written[read.len()..];
                    prop_assert_eq!(p.avail as usize, left.len());
                    prop_assert_eq!(&p.data[..], &left[..left.len().min(n as usize)]);
                    prop_assert_eq!(p.msg_len, 0);
                }
            }
            prop_assert_eq!(&written[..read.len()], &read[..]);
            prop_assert_eq!(k.avail(r) as usize, written.len() - read.len());
        }
    }

    /// A message pipe read by messages keeps them apart: each message comes back whole (in
    /// pieces with "more" on all but the last when the reads are short), in order.
    #[test]
    fn messages(ops in ops()) {
        let mut k = Kern::default();
        let s = named(&mut k, "m", 3, true, 1).unwrap();
        let c = k.open("m", true, true, PID).unwrap();
        let mut sent: VecDeque<Vec<u8>> = VecDeque::new();
        let mut part = Vec::new();
        for op in ops {
            match op {
                Op::Write(d) => {
                    // (done at once while the messages waiting fit the server's 512 bytes, as on
                    // Windows; the message is in the queue either way)
                    let waiting: usize = sent.iter().map(Vec::len).sum::<usize>() - part.len() + d.len();
                    let want = if waiting <= 512 || d.is_empty() { Ok(d.len() as u32) } else { Err(PENDING) };
                    prop_assert_eq!(k.write1(c, &d), want);
                    sent.push_back(d);
                }
                Op::Read(n) => match k.read(s, n) {
                    Ok((d, more)) => {
                        part.extend(d);
                        let want = sent.front().unwrap();
                        if more {
                            prop_assert!(part.len() < want.len());
                            prop_assert_eq!(&want[..part.len()], &part[..]);
                        } else {
                            prop_assert_eq!(&part, want);
                            sent.pop_front();
                            part.clear();
                        }
                    }
                    Err(e) => { prop_assert_eq!(e, PENDING); prop_assert!(sent.is_empty()); }
                },
                Op::Peek(n) => {
                    let p = k.peek(s, n).unwrap();
                    let total: usize = sent.iter().map(Vec::len).sum::<usize>() - part.len();
                    prop_assert_eq!(p.avail as usize, total);
                    let first = sent.front().map_or(0, |m| m.len() - part.len());
                    prop_assert_eq!(p.msg_len as usize, if total == 0 { 0 } else { first });
                    prop_assert_eq!(p.data.len(), first.min(n as usize));
                }
            }
        }
    }
}

#[test]
fn named_objects() {
    let mut k = Kern::default();
    let (m, existed) = k.create_sync("m", Sync::Mutex { owner: 0x20, pid: 1, count: 1, abandoned: false }, 1).unwrap();
    assert!(!existed);
    assert_eq!(k.create_sync("m", Sync::Event { manual: false, set: false }, 2), Err(2));
    assert_eq!(k.create_sync("m", Sync::Mutex { owner: 0, pid: 2, count: 0, abandoned: false }, 2), Ok((m, true)));
    assert_eq!(k.open_sync("m", 1, 2), Err(6));
    assert_eq!(k.open_sync("none", 2, 2), Err(2));
    // held by process 1's thread: process 2's thread waits
    assert_eq!(k.try_take(m, 2, 0x10020, true), Some((false, false)));
    assert_eq!(k.release_mutex(m, 0x10020), Err(288));
    assert_eq!(k.release_mutex(m, 0x20), Ok(()));
    assert_eq!(k.try_take(m, 2, 0x10020, true), Some((true, false)));
    // process 2 ends holding it: abandoned, and the next wait says so once
    k.exited(2, 0);
    assert_eq!(k.try_take(m, 1, 0x20, true), Some((true, true)));
    assert_eq!(k.try_take(m, 1, 0x20, true), Some((true, false)));
    assert_eq!(k.release_mutex(m, 0x20), Ok(()));
    assert_eq!(k.release_mutex(m, 0x20), Ok(()));
    assert_eq!(k.release_mutex(m, 0x20), Err(288));
    // a thread's end abandons too
    k.try_take(m, 1, 0x24, true);
    k.abandon(0x24);
    assert_eq!(k.try_take(m, 1, 0x20, false), Some((true, true)));
    // the name goes with the last handle
    k.release_sync(m, 1);
    assert_eq!(k.sync_kind("m"), None);

    let (e, _) = k.create_sync("e", Sync::Event { manual: false, set: false }, 1).unwrap();
    let e2 = k.open_sync("e", 1, 3).unwrap();
    assert_eq!(e, e2);
    assert!(k.set_event(e, true));
    assert_eq!(k.try_take(e, 3, 0x30020, true), Some((true, false)));
    assert_eq!(k.try_take(e, 1, 0x20, true), Some((false, false)));
    let (s, _) = k.create_sync("s", Sync::Semaphore { count: 1, max: 2 }, 1).unwrap();
    assert_eq!(k.release_semaphore(s, 1), Ok(1));
    assert_eq!(k.release_semaphore(s, 1), Err(298));
    assert!(!k.set_event(s, true));
}

#[test]
fn process_records() {
    let mut k = Kern::default();
    let (r, w) = k.create_anon(0, ROOT_PID).unwrap();
    let p = Proc { parent: ROOT_PID, suspend: 1, inherit: vec![(0x104, KRef::Pipe { end: w, read: false, write: true, ovl: false })], ..Proc::default() };
    let pid = k.new_process(p);
    assert_eq!(pid, ROOT_PID + PID_STEP);
    assert!(k.known(pid) && k.known(ROOT_PID) && !k.known(0xfffffff0));
    assert_eq!(k.exit_code(pid), None);
    assert_eq!(k.resume(pid), 1);
    assert_eq!(k.resume(pid), 0);
    // the parent's write end closes; the child's (inherited) keeps the pipe open
    k.release(w, ROOT_PID);
    k.write1(w, b"x").unwrap();
    assert!(k.terminate(pid, 9));
    k.exited(pid, 0);
    assert_eq!(k.exit_code(pid), Some(9));
    k.exited(pid, 5);
    assert_eq!(k.exit_code(pid), Some(9));
    assert_eq!(rd(&mut k, r, 10), ok("x", false));
    assert_eq!(k.read(r, 10), Err(PIPE_BROKEN));
}

#[test]
fn windows_of_other_processes() {
    let mut k = Kern::default();
    let child = k.new_process(Proc { parent: ROOT_PID, ..Proc::default() });
    let w = KWin { hwnd: 0x10020, pid: ROOT_PID, tid: 0x20, class: "masu_procs_class".into(), title: "masu procs".into() };
    k.set_wins(ROOT_PID, vec![w.clone()]);
    // (the same list again changes nothing)
    let g = k.gen;
    k.set_wins(ROOT_PID, vec![w.clone()]);
    assert_eq!(k.gen, g);
    // FindWindow: not among its own windows; class and title match whole
    assert_eq!(k.find_win(child, Some("masu_procs_class"), Some("masu procs")), Some(&w));
    assert_eq!(k.find_win(child, None, Some("masu procs")), Some(&w));
    assert_eq!(k.find_win(child, Some("masu_procs"), None), None);
    assert_eq!(k.find_win(ROOT_PID, Some("masu_procs_class"), None), None);
    // sent and posted, in order; the sender gets the result
    let s1 = k.put_msg(XMsg { to: ROOT_PID, hwnd: w.hwnd, msg: 0x4a, data: Some((7, b"copy".to_vec())), send: true, ..XMsg::default() });
    k.put_msg(XMsg { to: ROOT_PID, hwnd: w.hwnd, msg: 0x402, w: 5, l: 6, ..XMsg::default() });
    assert_eq!(k.take_reply(s1), Err(PENDING));
    assert!(k.take_msgs(child).is_empty());
    let got = k.take_msgs(ROOT_PID);
    assert_eq!(got.iter().map(|x| (x.msg, x.send)).collect::<Vec<_>>(), [(0x4a, true), (0x402, false)]);
    assert_eq!(got[0].data, Some((7, b"copy".to_vec())));
    assert!(k.take_msgs(ROOT_PID).is_empty());
    k.reply(s1, 99);
    assert_eq!(k.take_reply(s1), Ok(99));
    assert_eq!(k.take_reply(s1), Err(PENDING));
    // a process that ends: its windows go, what was sent to it is answered 0
    let s2 = k.put_msg(XMsg { to: ROOT_PID, hwnd: w.hwnd, msg: 0x401, send: true, ..XMsg::default() });
    k.exited(ROOT_PID, 0);
    assert_eq!(k.take_reply(s2), Ok(0));
    assert_eq!(k.win(w.hwnd), None);
    let s3 = k.put_msg(XMsg { to: ROOT_PID, hwnd: w.hwnd, msg: 0x401, send: true, ..XMsg::default() });
    assert_eq!(k.take_reply(s3), Ok(0));
    // (and all of it survives the trip through bytes)
    let mut k2 = Kern::default();
    k2.set_wins(child, vec![KWin { pid: child, ..w.clone() }]);
    k2.put_msg(XMsg { to: child, hwnd: 1, data: Some((u64::MAX, vec![1, 2])), send: true, ..XMsg::default() });
    let b = bytes(&k2);
    assert_eq!(bytes(&Kern::get(&mut &b[..]).unwrap()), b);
}

fn bytes(k: &Kern) -> Vec<u8> {
    let mut v = Vec::new();
    k.put(&mut v);
    v
}

proptest! {
    /// The state comes back the same from its bytes (what the processes of a host share),
    /// whatever was done to it; and cut bytes never decode.
    #[test]
    fn state_bytes(ops in ops(), cut in any::<prop::sample::Index>()) {
        let mut k = Kern::default();
        let (r, w) = k.create_anon(0, PID).unwrap();
        let s = named(&mut k, "m", 3, true, 2).unwrap();
        let c = k.open("m", true, true, PID).unwrap();
        let (ev, _) = k.create_sync("ev", Sync::Event { manual: true, set: false }, PID).unwrap();
        k.create_sync("mx", Sync::Mutex { owner: 0x20, pid: PID, count: 1, abandoned: false }, PID).unwrap();
        k.new_process(Proc { parent: PID, image: "C:\\a.exe".into(), cmdline: "a b".into(), cwd: "C:\\".into(), std: Some((1, 2, 3)), env: Some(vec![("=C:".into(), "C:\\x".into()), ("PATH".into(), "C:\\w".into())]), inherit: vec![(8, KRef::Sync { id: ev }), (12, KRef::Process { pid: 5, access: 1 })], ..Proc::default() });
        for op in ops {
            match op {
                Op::Write(d) => { let _ = k.write1(w, &d); let _ = k.write1(c, &d); }
                Op::Read(n) => { let _ = k.read(r, n); let _ = k.read(s, n); }
                Op::Peek(n) => { k.set_event(ev, n % 2 == 0); }
            }
            let b = bytes(&k);
            let back = Kern::get(&mut b.as_slice()).unwrap();
            prop_assert_eq!(bytes(&back), b.clone());
            let n = cut.index(b.len());
            prop_assert!(Kern::get(&mut &b[..n]).is_none());
        }
    }
}

proptest! {
    /// The C runtime's _pipe and _dup (crate::pipe::crt_dup): every duplicate of the write
    /// end is a reference of its own, so the reader sees the end of the pipe (PIPE_BROKEN) only
    /// once the last one is closed and what was written is read; until then an empty pipe waits
    /// (PENDING). Writes through any duplicate arrive in order.
    #[test]
    fn dup_writers(ops in prop::collection::vec((0u8..3, prop::collection::vec(any::<u8>(), 0..8)), 0..40)) {
        let mut k = Kern::default();
        let (r, w) = k.create_anon(0, PID).unwrap();
        let (mut refs, mut written, mut read) = (1u32, Vec::new(), Vec::new());
        for (op, d) in ops {
            match op {
                // _dup
                0 if refs > 0 => { k.add_ref(w, PID); refs += 1; }
                // _close of one of the write descriptors
                1 if refs > 0 => { k.release(w, PID); refs -= 1; }
                // _write through any of them
                2 if refs > 0 => { prop_assert_eq!(k.write1(w, &d), Ok(d.len() as u32)); written.extend(d); }
                // _read
                _ => match k.read(r, 4) {
                    Ok((got, _)) => { prop_assert!(!got.is_empty()); read.extend(got); }
                    Err(e) => {
                        prop_assert_eq!(read.len(), written.len());
                        prop_assert_eq!(e, if refs == 0 { PIPE_BROKEN } else { PENDING });
                    }
                },
            }
            prop_assert_eq!(&written[..read.len()], &read[..]);
        }
        // draining after every writer closed always ends in PIPE_BROKEN
        while refs > 0 { k.release(w, PID); refs -= 1; }
        loop {
            match k.read(r, 4) {
                Ok((got, _)) => read.extend(got),
                Err(e) => { prop_assert_eq!(e, PIPE_BROKEN); break; }
            }
        }
        prop_assert_eq!(read, written);
    }
}

// ---------------------------------------------------------------------------------------------
// another process's memory (crate::xmem): the request queue

#[test]
fn vm_requests_reach_their_process_in_order_and_ended_ones_answer_gone() {
    let mut k = Kern::default();
    let a = k.new_process(Proc::default());
    let b = k.new_process(Proc::default());
    let r1 = k.put_vm(VmReq { to: a, op: 1, args: vec![0, 0x1000, 0x3000, 4], ..Default::default() });
    let r2 = k.put_vm(VmReq { to: b, op: 5, ..Default::default() });
    let r3 = k.put_vm(VmReq { to: a, op: 7, ..Default::default() });
    assert_eq!(k.take_vm_reply(r1), Err(PENDING));
    let got: Vec<u32> = k.take_vm(a).iter().map(|r| r.id).collect();
    assert_eq!(got, vec![r1, r3]);
    assert!(k.take_vm(a).is_empty());
    k.vm_reply(r1, VmReply { status: 0, vals: vec![0x5000], data: vec![] });
    assert_eq!(k.take_vm_reply(r1).unwrap().vals, vec![0x5000]);
    assert_eq!(k.take_vm_reply(r1), Err(PENDING), "an answer is taken once");
    // b ends with r2 still waiting: it answers VM_GONE, and so does anything asked after
    k.exited(b, 0);
    assert_eq!(k.take_vm_reply(r2).unwrap().status, VM_GONE);
    let r4 = k.put_vm(VmReq { to: b, ..Default::default() });
    assert_eq!(k.take_vm_reply(r4).unwrap().status, VM_GONE);
    let r5 = k.put_vm(VmReq { to: 0x7777_0000, ..Default::default() });
    assert_eq!(k.take_vm_reply(r5).unwrap().status, VM_GONE, "no such process");
}

proptest! {
    /// Through the shared buffer (as bytes) the requests and answers come back the same.
    #[test]
    fn vm_queue_survives_encoding(ops in proptest::collection::vec((0u32..3, 0u32..8, proptest::collection::vec(any::<u32>(), 0..5), proptest::collection::vec(any::<u8>(), 0..40)), 0..12)) {
        use crate::kern::enc::Enc;
        let mut k = Kern::default();
        let pids: Vec<u32> = (0..3).map(|_| k.new_process(Proc::default())).collect();
        let mut ids = vec![];
        for (who, op, args, data) in ops {
            ids.push(k.put_vm(VmReq { id: 0, to: pids[who as usize], op, args: args.clone(), data: data.clone(), ..Default::default() }));
            if op % 2 == 0 {
                k.vm_reply(*ids.last().unwrap(), VmReply { status: op, vals: args, data });
            }
        }
        let mut bytes = vec![];
        k.put(&mut bytes);
        let mut r = &bytes[..];
        let mut back = Kern::get(&mut r).unwrap();
        prop_assert!(r.is_empty());
        for p in &pids {
            prop_assert_eq!(back.take_vm(*p), k.take_vm(*p));
        }
        for id in ids {
            prop_assert_eq!(back.take_vm_reply(id), k.take_vm_reply(id));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// bounded pipes: a writer waits for the reader to make room (review P1-4)

fn anon(k: &mut Kern, size: u32) -> (u32, u32) {
    k.create_anon(size, PID).unwrap()
}

#[test]
fn a_write_past_the_buffer_waits_until_it_fits() {
    let mut k = Kern::default();
    let (r, w) = anon(&mut k, 16);
    let mut st = Write::default();
    // (100 bytes go in whole, PIPE_PIECE at a time, and the write waits while more than 16 wait)
    assert_eq!(k.write(w, &[7; 100], &mut st), Err(PENDING));
    assert_eq!(k.avail(r), 100);
    assert_eq!(k.write(w, &[7; 100], &mut st), Err(PENDING));
    assert_eq!(k.read(r, 83).map(|d| d.0.len()), Ok(83));
    assert_eq!(k.write(w, &[7; 100], &mut st), Err(PENDING));
    assert_eq!(k.read(r, 1).map(|d| d.0.len()), Ok(1));
    assert_eq!(k.write(w, &[7; 100], &mut st), Ok(100));
    // what fits is done at once
    assert_eq!(k.write1(w, &[1; 10]), Err(PENDING));
    assert_eq!(k.read(r, 100).map(|d| d.0.len()), Ok(26));
    assert_eq!(k.write1(w, &[1; 16]), Ok(16));
}

#[test]
fn a_long_write_goes_through_a_buffer_at_a_time() {
    let mut k = Kern::default();
    let (r, w) = anon(&mut k, 0);
    let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 + i / 251) as u8).collect();
    let mut st = Write::default();
    let mut got = Vec::new();
    let mut done = false;
    while !done {
        match k.write(w, &data, &mut st) {
            Ok(n) => { assert_eq!(n as usize, data.len()); done = true; }
            Err(e) => assert_eq!(e, PENDING),
        }
        // (never more than the buffer's worth waits)
        assert!(k.avail(r) <= 4096, "{} waiting", k.avail(r));
        if let Ok((d, _)) = k.read(r, 1000) { got.extend(d); }
    }
    while let Ok((d, _)) = k.read(r, 5000) { got.extend(d); }
    assert_eq!(got, data);
}

#[test]
fn a_waiting_write_ends_with_the_reader() {
    let mut k = Kern::default();
    let (r, w) = anon(&mut k, 8);
    let mut st = Write::default();
    assert_eq!(k.write(w, &[1; 20], &mut st), Err(PENDING));
    k.release(r, PID);
    assert_eq!(k.write(w, &[1; 20], &mut st), Err(PIPE_BROKEN));
    // read to the end before the reader closes: done
    let (r, w) = anon(&mut k, 8);
    let mut st = Write::default();
    assert_eq!(k.write(w, &[1; 20], &mut st), Err(PENDING));
    assert_eq!(k.read(r, 20).map(|d| d.0.len()), Ok(20));
    k.release(r, PID);
    assert_eq!(k.write(w, &[1; 20], &mut st), Ok(20));
}

#[test]
fn messages_wait_whole_and_nonblocking_ends_do_not() {
    let mut k = Kern::default();
    let s = named(&mut k, "q", 3, true, 1).unwrap();
    let c = k.open("q", true, true, PID).unwrap();
    let mut st = Write::default();
    assert_eq!(k.write(c, &[2; 600], &mut st), Err(PENDING));
    assert_eq!(k.avail(s), 600);
    // (an empty message is done at once)
    assert_eq!(k.write1(c, b""), Ok(0));
    assert_eq!(k.read(s, 100).map(|d| (d.0.len(), d.1)), Ok((100, true)));
    assert_eq!(k.write(c, &[2; 600], &mut st), Ok(600));
    assert_eq!(k.set_mode(c, None, Some(true)), SUCCESS);
    assert_eq!(k.write1(c, &[3; 2000]), Ok(2000));
}

#[test]
fn what_waits_in_pipes_is_bounded() {
    let mut k = Kern::default();
    let s = named(&mut k, "big", 3, true, 1).unwrap();
    let c = k.open("big", true, true, PID).unwrap();
    // a writer that never waits (overlapped writes left pending) fills only up to the quota
    let mut n = 0;
    loop {
        match k.write1(c, &[0; 1 << 20]) {
            Err(PENDING) => n += 1,
            Err(e) => { assert_eq!(e, QUOTA_EXCEEDED); break; }
            Ok(_) => unreachable!(),
        }
    }
    assert_eq!(n, PIPE_QUOTA >> 20);
    assert_eq!(win32(QUOTA_EXCEEDED), 1816);
    // the kernel's state fits the processes' shared buffer (host/pre.js MASU_KERNEL_BYTES)
    assert!(bytes(&k).len() < 32 << 20);
    // reading makes room again
    assert!(k.read(s, 1 << 20).is_ok());
    assert_eq!(k.write1(c, &[0; 1 << 20]), Err(PENDING));
}

proptest! {
    /// A writer and a reader taking turns at random, any buffer size: the bytes come out in
    /// order and whole, at most the buffer's worth (or PIPE_PIECE) waits, a write is done only
    /// once what waits fits the buffer, and every write ends once the reader keeps reading.
    #[test]
    fn bounded_byte_stream(size in prop_oneof![Just(0u32), 1u32..64, Just(4096), 5000u32..20000], writes in prop::collection::vec(0usize..30000, 1..8), reads in prop::collection::vec(1u32..9000, 1..40), seed in any::<u64>()) {
        let mut k = Kern::default();
        let (r, w) = anon(&mut k, size);
        // (CreatePipe's 0 is 4096)
        let room = if size == 0 { 4096 } else { size as usize };
        let mut rng = seed;
        let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
        let (mut written, mut got) = (Vec::new(), Vec::new());
        let mut ri = 0;
        for (i, len) in writes.iter().enumerate() {
            let data: Vec<u8> = (0..*len).map(|j| (i * 31 + j) as u8).collect();
            let mut st = Write::default();
            let mut steps = 0;
            loop {
                steps += 1;
                prop_assert!(steps < 100_000);
                let tries = (next() % 3) as usize;
                let mut res = Err(PENDING);
                for _ in 0..=tries {
                    res = k.write(w, &data, &mut st);
                    if res != Err(PENDING) { break; }
                }
                prop_assert!(k.avail(r) as usize <= room.max(4096));
                match res {
                    Ok(n) => {
                        prop_assert_eq!(n as usize, data.len());
                        prop_assert!(k.avail(r) as usize <= room || data.is_empty());
                        break;
                    }
                    Err(e) => prop_assert_eq!(e, PENDING),
                }
                let n = reads[ri % reads.len()]; ri += 1;
                if let Ok((d, _)) = k.read(r, n) { got.extend(d); }
            }
            written.extend(data);
        }
        while let Ok((d, _)) = k.read(r, 7000) { got.extend(d); }
        prop_assert_eq!(got, written);
        // and through the state's bytes (what another process reads back) mid-write
        let mut st = Write::default();
        let _ = k.write(w, &[9; 9000], &mut st);
        let mut b = bytes(&k);
        let mut back = Kern::get(&mut b.as_slice()).unwrap();
        let mut out = Vec::new();
        loop {
            match back.write(w, &[9; 9000], &mut st) {
                Ok(_) => break,
                Err(e) => prop_assert_eq!(e, PENDING),
            }
            if let Ok((d, _)) = back.read(r, 3000) { out.extend(d); }
        }
        while let Ok((d, _)) = back.read(r, 9000) { out.extend(d); }
        prop_assert_eq!(out, vec![9; 9000]);
        b.clear();
    }
}

/// The real thing between two threads (kern::with / kern::block and their waits): 4 MiB through
/// a 4 KiB pipe arrives whole, and the writer never gets more than the buffer ahead.
#[test]
fn threads_stream_through_a_small_pipe() {
    let (r, w) = with(|k| k.create_anon(4096, PID)).0.unwrap();
    let data: std::sync::Arc<Vec<u8>> = std::sync::Arc::new((0..4u32 << 20).map(|i| (i ^ (i >> 9)) as u8).collect());
    let d = data.clone();
    let writer = std::thread::spawn(move || {
        for part in d.chunks(100_000) {
            let mut st = Write::default();
            assert_eq!(block(None, &|| false, |k| k.write(w, part, &mut st)), Ok(part.len() as u32));
        }
        with(|k| k.release(w, PID));
    });
    let mut got = Vec::new();
    loop {
        match block(None, &|| false, |k| { let a = k.avail(r); k.read(r, 3000).map(|x| (x.0, a)) }) {
            Ok((d, avail)) => { assert!(avail <= 4096); got.extend(d); }
            Err(e) => { assert_eq!(e, PIPE_BROKEN); break; }
        }
    }
    writer.join().unwrap();
    with(|k| k.release(r, PID));
    assert!(got == *data);
}

/// Review b07: answers nobody takes are not kept: a posted message to an ended process, the
/// answers of a process that ended, a reply to an asker that went.
#[test]
fn answers_nobody_takes_are_dropped() {
    let mut k = Kern::default();
    let (a, b) = (ROOT_PID, ROOT_PID + PID_STEP);
    k.procs.entry(a).or_default();
    k.procs.entry(b).or_default();
    k.exited(b, 0);
    // posted to an ended process: no answer kept; sent: 0, taken once
    k.put_msg(XMsg { to: b, msg: 0x400, from: a, ..XMsg::default() });
    assert!(k.replies.is_empty() && k.askers.is_empty());
    let s = k.put_msg(XMsg { to: b, msg: 0x400, send: true, from: a, ..XMsg::default() });
    assert_eq!(k.take_reply(s), Ok(0));
    assert!(k.replies.is_empty() && k.askers.is_empty());
    // an asker that ends before its answer comes: the answer is not kept
    let c = ROOT_PID + 2 * PID_STEP;
    k.procs.entry(c).or_default();
    let s = k.put_msg(XMsg { to: a, msg: 0x401, send: true, from: c, ..XMsg::default() });
    let v = k.put_vm(VmReq { to: a, op: 1, from: c, ..Default::default() });
    k.exited(c, 0);
    assert!(k.xmsgs.is_empty() && k.vm.is_empty(), "its questions are taken back");
    k.reply(s, 5);
    k.vm_reply(v, VmReply::default());
    assert!(k.replies.is_empty() && k.vm_replies.is_empty() && k.askers.is_empty());
    // an answer already there when its asker ends
    let d = ROOT_PID + 3 * PID_STEP;
    k.procs.entry(d).or_default();
    let s = k.put_msg(XMsg { to: a, msg: 0x401, send: true, from: d, ..XMsg::default() });
    k.take_msgs(a);
    k.reply(s, 9);
    assert_eq!(k.replies.len(), 1);
    k.exited(d, 0);
    assert!(k.replies.is_empty() && k.askers.is_empty());
}

#[test]
fn squatted_anonymous_pipe_names_fail_the_call() {
    let mut k = Kern::default();
    let p = NewPipe { access: 3, msg_type: false, msg_read: false, nonblock: false, max: 1, outsize: 64, insize: 64, timeout: 0, first: true };
    // a program that made pipes of the names CreatePipe takes next
    for i in 1..=3 {
        k.create(&format!("win32pipes.{PID:08x}.{:08x}", k.next + i), &p, PID).unwrap();
    }
    let (r, w) = k.create_anon(0, PID).expect("a later free name");
    assert_eq!(k.write1(w, b"x"), Ok(1));
    assert_eq!(rd(&mut k, r, 1), ok("x", false));
    // every name it would try taken: CreatePipe fails, nothing aborts
    let mut k = Kern::default();
    let base = k.next;
    for i in 1..=64 {
        k.create(&format!("win32pipes.{PID:08x}.{:08x}", base + i), &p, PID).unwrap();
    }
    k.next = base;
    assert_eq!(k.create_anon(0, PID), None);
}

#[test]
fn semaphores_take_only_positive_counts() {
    let mut k = Kern::default();
    let (s, _) = k.create_sync("s", Sync::Semaphore { count: 0, max: 5 }, 1).unwrap();
    for n in [0, -1, i32::MIN] {
        assert_eq!(k.release_semaphore(s, n), Err(87), "{n}");
    }
    assert_eq!(k.release_semaphore(s, 5), Ok(0));
    assert_eq!(k.release_semaphore(s, i32::MAX), Err(298));
}

proptest! {
    /// Whatever processes ask each other and in whatever order they end, once all have ended
    /// no answer, question or asker is left (the shared state no longer grows with them), and
    /// while they run every answer is taken exactly once by its asker.
    #[test]
    fn ended_processes_leave_no_answers(ops in proptest::collection::vec((0u8..6, 0u8..3, 0u8..3, any::<u32>()), 1..120)) {
        let mut k = Kern::default();
        let pids: Vec<u32> = (0..3).map(|i| ROOT_PID + i * PID_STEP).collect();
        for &p in &pids {
            k.procs.entry(p).or_default();
        }
        let mut open: Vec<(u32, u32, bool)> = vec![];
        for (op, from, to, v) in ops {
            let (from, to) = (pids[from as usize], pids[to as usize]);
            let alive = |k: &Kern, p: u32| k.exit_code(p).is_none();
            match op {
                0 if alive(&k, from) => {
                    let id = k.put_msg(XMsg { to, msg: 0x400, send: true, from, ..XMsg::default() });
                    open.push((id, from, false));
                }
                1 if alive(&k, from) => {
                    k.put_msg(XMsg { to, msg: 0x401, from, ..XMsg::default() });
                }
                2 if alive(&k, from) => {
                    let id = k.put_vm(VmReq { to, op: 1, from, ..Default::default() });
                    open.push((id, from, true));
                }
                3 => {
                    // `to` answers what came for it
                    for m in k.take_msgs(to) {
                        if m.send {
                            k.reply(m.id, v);
                        }
                    }
                    for r in k.take_vm(to) {
                        k.vm_reply(r.id, VmReply { status: v, ..Default::default() });
                    }
                }
                4 => {
                    for &(id, asker, vm) in &open {
                        if k.exit_code(asker).is_none() {
                            let got = if vm { k.take_vm_reply(id).map(|_| 0) } else { k.take_reply(id) };
                            if got.is_ok() {
                                prop_assert_eq!(if vm { k.take_vm_reply(id).map(|_| 0) } else { k.take_reply(id) }, Err(PENDING));
                            }
                        }
                    }
                }
                5 => k.exited(from, 0),
                _ => {}
            }
        }
        for &p in &pids {
            k.exited(p, 0);
        }
        prop_assert!(k.replies.is_empty(), "{:?}", k.replies);
        prop_assert!(k.vm_replies.is_empty());
        prop_assert!(k.askers.is_empty());
        prop_assert!(k.xmsgs.is_empty() && k.vm.is_empty());
    }
}
