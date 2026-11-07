//! The pipe model against Windows' answers (tests/winref/golden/pipes-32, the same steps at the
//! kernel's level) and against properties any pipe keeps (proptest).

use super::*;
use proptest::prelude::*;

const PID: u32 = 7;

fn named(k: &mut Kern, name: &str, access: u32, msg: bool, max: u32) -> Result<u32, u32> {
    let p = NewPipe {
        access,
        msg_type: msg,
        msg_read: msg,
        nonblock: false,
        max,
        outsize: 512,
        insize: 512,
        timeout: 0,
        first: false,
    };
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
    let (r, w) = k.create_anon(0, PID);
    assert_eq!(k.write(w, b"abc"), Ok(3));
    assert_eq!(
        k.peek(r, 4),
        Ok(Peek {
            data: b"abc".to_vec(),
            avail: 3,
            msg_len: 0
        })
    );
    assert_eq!(rd(&mut k, r, 2), ok("ab", false));
    assert_eq!(rd(&mut k, r, 10), ok("c", false));
    assert_eq!(k.write(w, b""), Ok(0));
    assert_eq!(k.peek(r, 4), Ok(Peek { data: vec![], avail: 0, msg_len: 0 }));
    assert_eq!(k.read(r, 4), Err(PENDING));
    k.release(w, PID);
    assert_eq!(k.read(r, 4), Err(PIPE_BROKEN));
    assert_eq!(k.peek(r, 4), Err(PIPE_BROKEN));
    k.release(r, PID);
    assert!(k.ends.is_empty() && k.pipes.is_empty());
    let (r, w) = k.create_anon(0, PID);
    k.release(r, PID);
    assert_eq!(k.write(w, b"q"), Err(PIPE_CLOSING));
    assert_eq!(win32(PIPE_CLOSING), 232);
    // what the reader has not read yet stays after the writer closed
    let (r, w) = k.create_anon(0, PID);
    k.write(w, b"left").unwrap();
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
    k.write(c, b"hello").unwrap();
    k.write(c, b"world").unwrap();
    let p = k.peek(s, 4).unwrap();
    assert_eq!((p.data.as_slice(), p.avail, p.msg_len), (&b"hell"[..], 10, 0));
    assert_eq!(rd(&mut k, s, 3), ok("hel", false));
    assert_eq!(rd(&mut k, s, 100), ok("loworld", false));
    k.write(s, b"reply").unwrap();
    assert_eq!(rd(&mut k, c, 100), ok("reply", false));
    let i = k.info(s).unwrap();
    assert_eq!((i.server, i.msg_type, i.outsize, i.insize, i.max, i.instances), (true, false, 512, 512, 1, Some(1)));
    assert!(!k.info(c).unwrap().server);
    assert_eq!(k.set_mode(c, Some(true), None), INVALID_PARAMETER);
    k.release(c, PID);
    assert_eq!(k.read(s, 10), Err(PIPE_BROKEN));
    assert_eq!(k.peek(s, 4), Err(PIPE_BROKEN));
    assert_eq!(k.write(s, b"late"), Err(PIPE_CLOSING));
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
    k.write(s, b"lost").unwrap();
    assert_eq!(k.disconnect(s), SUCCESS);
    assert_eq!(k.read(c, 10), Err(PIPE_DISCONNECTED));
    assert_eq!(k.write(c, b"x"), Err(PIPE_DISCONNECTED));
    assert_eq!(k.peek(c, 4), Err(PIPE_DISCONNECTED));
    assert_eq!(k.info(c).unwrap().instances, None);
    // a new client of the same instance is not the old one
    assert_eq!(k.listen(s), PENDING);
    let c2 = k.open("d", true, true, PID).unwrap();
    k.write(s, b"new").unwrap();
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
    k.write(c1, b"first message").unwrap();
    k.write(c1, b"second").unwrap();
    k.write(c2, b"to two").unwrap();
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
    k.write(m1, b"ab").unwrap();
    k.write(m1, b"cd").unwrap();
    assert_eq!(rd(&mut k, c1, 1), ok("a", true));
    assert_eq!(rd(&mut k, c1, 10), ok("b", false));
    assert_eq!(rd(&mut k, c1, 10), ok("cd", false));
    assert_eq!(k.set_mode(m2, Some(false), None), SUCCESS);
    k.write(c2, b"xy").unwrap();
    k.write(c2, b"zw").unwrap();
    assert_eq!(rd(&mut k, m2, 10), ok("xyzw", false));
    k.write(m1, b"pending").unwrap();
    assert_eq!(k.transact(c1, b"q"), PIPE_BUSY);
    assert_eq!(rd(&mut k, c1, 100), ok("pending", false));
    assert_eq!(k.transact(c1, b"q"), SUCCESS);
    assert_eq!(rd(&mut k, m1, 100), ok("q", false));
    assert_eq!(k.transact(m2, b"q"), INVALID_READ_MODE);
    // an empty message is a message
    k.write(m1, b"").unwrap();
    assert_eq!(rd(&mut k, c1, 100), ok("", false));
    assert_eq!(k.read(c1, 100), Err(PENDING));
    k.release(c1, PID);
    k.release(c2, PID);
    assert_eq!(k.read(m1, 10), Err(PIPE_BROKEN));
    assert_eq!(k.write(m1, b"x"), Err(PIPE_CLOSING));
}

#[test]
fn one_way() {
    let mut k = Kern::default();
    let s = named(&mut k, "in", 1, false, 1).unwrap();
    assert_eq!(k.open("in", true, true, PID), Err(ACCESS_DENIED));
    let c = k.open("in", false, true, PID).unwrap();
    k.write(c, b"up").unwrap();
    assert_eq!(rd(&mut k, s, 10), ok("up", false));
    let s = named(&mut k, "out", 2, false, 1).unwrap();
    assert_eq!(k.open("out", false, true, PID), Err(ACCESS_DENIED));
    assert!(k.open("out", true, false, PID).is_ok());
    let _ = s;
    // a second instance must share the same way
    assert_eq!(named(&mut k, "out", 3, false, 2), Err(INSTANCE_NOT_AVAILABLE));
    let p = NewPipe {
        access: 3,
        msg_type: false,
        msg_read: false,
        nonblock: false,
        max: 2,
        outsize: 0,
        insize: 0,
        timeout: 0,
        first: false,
    };
    k.create("two", &p, PID).unwrap();
    assert_eq!(k.create("two", &NewPipe { access: 1, ..p }, PID), Err(ACCESS_DENIED));
    assert_eq!(k.create("two", &NewPipe { first: true, ..p }, PID), Err(ACCESS_DENIED));
}

#[test]
fn nonblocking() {
    let mut k = Kern::default();
    let p = NewPipe {
        access: 3,
        msg_type: false,
        msg_read: false,
        nonblock: true,
        max: 1,
        outsize: 0,
        insize: 0,
        timeout: 0,
        first: false,
    };
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
    let (r, w) = k.create_anon(0, 1);
    // the write end inherited by process 2, then closed by process 1
    k.add_ref(w, 2);
    k.release(w, 1);
    k.write(w, b"from 2").unwrap();
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
    prop::collection::vec(
        prop_oneof![
            prop::collection::vec(any::<u8>(), 0..20).prop_map(Op::Write),
            (0u32..30).prop_map(Op::Read),
            (0u32..30).prop_map(Op::Peek),
        ],
        0..60,
    )
}

proptest! {
    /// A byte pipe is a stream: what is read is what was written, in order, whatever the
    /// writes' and reads' sizes; peeking changes nothing; avail is what is left.
    #[test]
    fn byte_stream(ops in ops()) {
        let mut k = Kern::default();
        let (r, w) = k.create_anon(0, PID);
        let (mut written, mut read) = (Vec::new(), Vec::new());
        for op in ops {
            match op {
                Op::Write(d) => { prop_assert_eq!(k.write(w, &d), Ok(d.len() as u32)); written.extend(d); }
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
                Op::Write(d) => { k.write(c, &d).unwrap(); sent.push_back(d); }
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
