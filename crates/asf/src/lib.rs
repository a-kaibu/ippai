//! Advanced Systems Format (ASF: `.wmv`, `.wma`, `.asf`) demuxing, as the Windows Media
//! Format SDK's readers (wmvcore.dll's IWMSyncReader / IWMReader) see a file: its header (the
//! streams, their formats, the length, the attributes) and its media objects (a stream's
//! samples, in the order the file holds them, with their presentation time).
//!
//! The format is Microsoft's "Advanced Systems Format (ASF) Specification" (revision 01.20.05):
//! a Header Object (sub-objects: File Properties, Stream Properties per stream, Header
//! Extension, Content Description, Extended Content Description ...), then a Data Object of
//! fixed-size packets. A packet carries payloads (pieces of media objects of any stream, a
//! media object split over packets is put back together by offset) or, with replicated data of
//! length 1, several whole small objects at once ("compressed payloads"). An audio stream with
//! the Audio Spread error correction has its objects interleaved in a span (`descramble`).
//!
//! Nothing here trusts the file: lengths are checked, a damaged packet gives what it holds up
//! to the damage, and objects never grow past their stated size (fuzz/fuzz_targets/asf.rs).

use std::collections::{HashMap, VecDeque};

pub type Guid = [u8; 16];

/// A GUID in the byte order files hold it (the first three fields little-endian).
pub const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

pub const HEADER: Guid = guid(0x75b2_2630, 0x668e, 0x11cf, [0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c]);
pub const DATA: Guid = guid(0x75b2_2636, 0x668e, 0x11cf, [0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c]);
pub const FILE_PROPERTIES: Guid = guid(0x8cab_dca1, 0xa947, 0x11cf, [0x8e, 0xe4, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65]);
pub const STREAM_PROPERTIES: Guid = guid(0xb7dc_0791, 0xa9b7, 0x11cf, [0x8e, 0xe6, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65]);
pub const HEADER_EXTENSION: Guid = guid(0x5fbf_03b5, 0xa92e, 0x11cf, [0x8e, 0xe3, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65]);
pub const CONTENT_DESCRIPTION: Guid = guid(0x75b2_2633, 0x668e, 0x11cf, [0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c]);
pub const EXTENDED_CONTENT_DESCRIPTION: Guid = guid(0xd2d0_a440, 0xe307, 0x11d2, [0x97, 0xf0, 0x00, 0xa0, 0xc9, 0x5e, 0xa8, 0x50]);
pub const EXTENDED_STREAM_PROPERTIES: Guid = guid(0x14e6_a5cb, 0xc672, 0x4332, [0x83, 0x99, 0xa9, 0x69, 0x52, 0x06, 0x5b, 0x5a]);
pub const METADATA: Guid = guid(0xc5f8_cbea, 0x5baf, 0x4877, [0x84, 0x67, 0xaa, 0x8c, 0x44, 0xfa, 0x4c, 0xca]);
pub const STREAM_BITRATE_PROPERTIES: Guid = guid(0x7bf8_75ce, 0x468d, 0x11d1, [0x8d, 0x82, 0x00, 0x60, 0x97, 0xc9, 0xa2, 0xb2]);

pub const AUDIO_MEDIA: Guid = guid(0xf869_9e40, 0x5b4d, 0x11cf, [0xa8, 0xfd, 0x00, 0x80, 0x5f, 0x5c, 0x44, 0x2b]);
pub const VIDEO_MEDIA: Guid = guid(0xbc19_efc0, 0x5b4d, 0x11cf, [0xa8, 0xfd, 0x00, 0x80, 0x5f, 0x5c, 0x44, 0x2b]);
pub const AUDIO_SPREAD: Guid = guid(0xbfc3_cd50, 0x618f, 0x11cf, [0x8b, 0xb2, 0x00, 0xaa, 0x00, 0xb4, 0xe2, 0x20]);
pub const NO_ERROR_CORRECTION: Guid = guid(0x20fb_5700, 0x5b55, 0x11cf, [0xa8, 0xfd, 0x00, 0x80, 0x5f, 0x5c, 0x44, 0x2b]);

/// 100 ns units a millisecond (ASF's times are in ms in packets, in 100 ns in the header)
const MS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// not an ASF file (no Header Object first)
    NotAsf,
    /// the header or a packet ends before its stated length
    Truncated,
    /// a field the format leaves no room for (a packet size of 0, stream numbers wider than a byte)
    Bad(&'static str),
}

/// What kind of media a stream holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// type-specific data: a WAVEFORMATEX (with its codec data)
    Audio,
    /// a BITMAPINFOHEADER (with its codec data), the picture `width` x `height`
    Video { width: u32, height: u32 },
    Other(Guid),
}

/// The Audio Spread error correction: a span of `span` packets of `packet_size` bytes is
/// written as chunks of `chunk_size`, column by column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub span: u8,
    pub packet_size: u16,
    pub chunk_size: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub number: u8,
    pub kind: Kind,
    /// WAVEFORMATEX (audio) or BITMAPINFOHEADER (video), as the file holds it
    pub format: Vec<u8>,
    pub encrypted: bool,
    /// 100 ns
    pub time_offset: u64,
    pub spread: Option<Spread>,
    /// from the Extended Stream Properties (0: not given): 100 ns per frame, bits per second
    pub avg_time_per_frame: u64,
    pub bitrate: u32,
}

impl Stream {
    /// The codec: WAVEFORMATEX.wFormatTag (audio) or BITMAPINFOHEADER.biCompression (video).
    pub fn codec(&self) -> u32 {
        match self.kind {
            Kind::Audio => le16(&self.format, 0).unwrap_or(0) as u32,
            Kind::Video { .. } => le32(&self.format, 16).unwrap_or(0),
            Kind::Other(_) => 0,
        }
    }
}

/// An attribute's value (Extended Content Description / Metadata types 0..6).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Bytes(Vec<u8>),
    Bool(bool),
    U32(u32),
    U64(u64),
    U16(u16),
    Guid(Guid),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// 0: the file's, else the stream's
    pub stream: u16,
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Header {
    pub streams: Vec<Stream>,
    /// the play duration less the preroll (100 ns)
    pub duration: u64,
    /// ms the presentation times are ahead by
    pub preroll: u64,
    /// File Properties flags: 1 broadcast, 2 seekable
    pub flags: u32,
    pub packet_size: u32,
    pub max_bitrate: u32,
    /// the Data Object's packets: where the first is, and how many (0: unknown, broadcast)
    pub data_offset: u64,
    pub packets: u64,
    pub title: String,
    pub author: String,
    pub copyright: String,
    pub description: String,
    pub attributes: Vec<Attribute>,
}

impl Header {
    pub fn stream(&self, number: u8) -> Option<&Stream> {
        self.streams.iter().find(|s| s.number == number)
    }
    /// A file attribute (stream 0) by name.
    pub fn attribute(&self, stream: u16, name: &str) -> Option<&Value> {
        self.attributes.iter().find(|a| a.stream == stream && a.name == name).map(|a| &a.value)
    }
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}
fn guid_at(b: &[u8], at: usize) -> Option<Guid> {
    b.get(at..at.checked_add(16)?)?.try_into().ok()
}
fn utf16(b: &[u8]) -> String {
    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// The objects in `b` (GUID, then its whole length including the 24-byte head): (guid, body).
fn objects(b: &[u8]) -> impl Iterator<Item = (Guid, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let g = guid_at(b, at)?;
        let size = le64(b, at + 16)?;
        if size < 24 {
            return None;
        }
        let end = at.checked_add(usize::try_from(size).ok()?)?.min(b.len());
        let body = &b[at + 24..end];
        at = end;
        Some((g, body))
    })
}

/// How many bytes the start of a file must hold for `header` to parse: its Header Object and
/// the head of the Data Object after it (`None`: not ASF, or too short to tell yet: 30 bytes).
pub fn header_size(start: &[u8]) -> Option<u64> {
    if guid_at(start, 0)? != HEADER {
        return None;
    }
    le64(start, 16)?.checked_add(50)
}

/// Parses the Header Object at the start of `b` (and the Data Object's head after it: `b` holds
/// `header_size` bytes).
pub fn header(b: &[u8]) -> Result<Header, Error> {
    if guid_at(b, 0).ok_or(Error::Truncated)? != HEADER {
        return Err(Error::NotAsf);
    }
    let size = le64(b, 16).ok_or(Error::Truncated)?;
    let size = usize::try_from(size).map_err(|_| Error::Bad("header size"))?;
    if size < 30 || size > b.len() {
        return Err(Error::Truncated);
    }
    let mut h = Header::default();
    let mut ext: Vec<(u8, u64, u32)> = Vec::new();
    let mut file_props = false;
    for (g, body) in objects(&b[30..size]) {
        match g {
            FILE_PROPERTIES => {
                let f = |at| le64(body, at).ok_or(Error::Truncated);
                let play = f(40)?;
                h.preroll = f(56)?;
                h.flags = le32(body, 64).ok_or(Error::Truncated)?;
                h.packet_size = le32(body, 68).ok_or(Error::Truncated)?;
                h.max_bitrate = le32(body, 76).unwrap_or(0);
                h.duration = play.saturating_sub(h.preroll.saturating_mul(MS));
                h.packets = f(32)?;
                file_props = true;
            }
            STREAM_PROPERTIES => {
                if let Some(s) = stream_properties(body) {
                    if !h.streams.iter().any(|t| t.number == s.number) {
                        h.streams.push(s);
                    }
                }
            }
            HEADER_EXTENSION => {
                let n = le32(body, 18).unwrap_or(0) as usize;
                let inner = body.get(22..22usize.saturating_add(n).min(body.len())).unwrap_or(&[]);
                for (g, o) in objects(inner) {
                    match g {
                        EXTENDED_STREAM_PROPERTIES => {
                            // start time, end time, bitrate, buffer size, initial fullness,
                            // alt bitrate, alt buffer, alt fullness, max object size, flags,
                            // stream number, language, avg time per frame, ...
                            if let (Some(rate), Some(n), Some(t)) = (le32(o, 16), le16(o, 48), le64(o, 52)) {
                                ext.push(((n & 0x7f) as u8, t, rate));
                            }
                            // (it may hold the stream's Stream Properties Object, after its
                            // names and payload extension systems)
                            if let (Some(names), Some(systems)) = (le16(o, 60), le16(o, 62)) {
                                let mut at = 64usize;
                                for _ in 0..names {
                                    let len = le16(o, at + 2).unwrap_or(0) as usize;
                                    at = at.saturating_add(4 + len);
                                }
                                for _ in 0..systems {
                                    let len = le32(o, at + 18).unwrap_or(0) as usize;
                                    at = at.saturating_add(22 + len);
                                }
                                if let Some(rest) = o.get(at..) {
                                    for (g, sp) in objects(rest) {
                                        if g == STREAM_PROPERTIES {
                                            if let Some(s) = stream_properties(sp) {
                                                if !h.streams.iter().any(|t| t.number == s.number) {
                                                    h.streams.push(s);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        METADATA => h.attributes.extend(metadata(o)),
                        _ => {}
                    }
                }
            }
            CONTENT_DESCRIPTION => {
                let lens: Vec<usize> = (0..5).map(|k| le16(body, k * 2).unwrap_or(0) as usize).collect();
                let mut at = 10usize;
                let mut strs = Vec::new();
                for l in lens {
                    strs.push(utf16(body.get(at..at.saturating_add(l).min(body.len())).unwrap_or(&[])));
                    at = at.saturating_add(l);
                }
                h.title = strs[0].clone();
                h.author = strs[1].clone();
                h.copyright = strs[2].clone();
                h.description = strs[3].clone();
            }
            EXTENDED_CONTENT_DESCRIPTION => h.attributes.extend(extended_content(body)),
            _ => {}
        }
    }
    if !file_props {
        return Err(Error::Bad("no file properties"));
    }
    for (n, t, rate) in ext {
        if let Some(s) = h.streams.iter_mut().find(|s| s.number == n) {
            s.avg_time_per_frame = t;
            s.bitrate = rate;
        }
    }
    // the Data Object: its GUID, size, file id, total packets, reserved
    if guid_at(b, size) == Some(DATA) {
        h.data_offset = size as u64 + 50;
        if let Some(n) = le64(b, size + 40) {
            if h.packets == 0 {
                h.packets = n;
            }
        }
    } else {
        return Err(Error::Bad("no data object after the header"));
    }
    if h.packet_size == 0 {
        return Err(Error::Bad("packet size 0"));
    }
    Ok(h)
}

fn stream_properties(b: &[u8]) -> Option<Stream> {
    let ty = guid_at(b, 0)?;
    let ec = guid_at(b, 16)?;
    let time_offset = le64(b, 32)?;
    let tlen = le32(b, 40)? as usize;
    let elen = le32(b, 44)? as usize;
    let flags = le16(b, 48)?;
    let ts = b.get(54..54usize.checked_add(tlen)?)?;
    let es = b.get(54 + tlen..(54 + tlen).checked_add(elen)?).unwrap_or(&[]);
    let (kind, format) = match ty {
        AUDIO_MEDIA => (Kind::Audio, ts.to_vec()),
        VIDEO_MEDIA => {
            let width = le32(ts, 0)?;
            let height = le32(ts, 4)?;
            let n = le16(ts, 9)? as usize;
            (Kind::Video { width, height }, ts.get(11..11 + n.min(ts.len().saturating_sub(11)))?.to_vec())
        }
        g => (Kind::Other(g), ts.to_vec()),
    };
    let spread = (ec == AUDIO_SPREAD)
        .then(|| Some(Spread { span: *es.first()?, packet_size: le16(es, 1)?, chunk_size: le16(es, 3)? }))
        .flatten()
        .filter(|s| s.span > 1 && s.chunk_size > 0 && s.packet_size % s.chunk_size == 0);
    Some(Stream { number: (flags & 0x7f) as u8, kind, format, encrypted: flags & 0x8000 != 0, time_offset, spread, avg_time_per_frame: 0, bitrate: 0 })
}

fn value(ty: u16, v: &[u8], wide_bool: bool) -> Value {
    match ty {
        0 => Value::Str(utf16(v)),
        2 => Value::Bool(if wide_bool { le32(v, 0).unwrap_or(0) != 0 } else { le16(v, 0).unwrap_or(0) != 0 }),
        3 => Value::U32(le32(v, 0).unwrap_or(0)),
        4 => Value::U64(le64(v, 0).unwrap_or(0)),
        5 => Value::U16(le16(v, 0).unwrap_or(0)),
        6 => Value::Guid(guid_at(v, 0).unwrap_or([0; 16])),
        _ => Value::Bytes(v.to_vec()),
    }
}

fn extended_content(b: &[u8]) -> Vec<Attribute> {
    let mut out = Vec::new();
    let n = le16(b, 0).unwrap_or(0);
    let mut at = 2usize;
    for _ in 0..n {
        let Some(nl) = le16(b, at) else { break };
        let Some(name) = b.get(at + 2..at + 2 + nl as usize) else { break };
        at += 2 + nl as usize;
        let (Some(ty), Some(vl)) = (le16(b, at), le16(b, at + 2)) else { break };
        let Some(v) = b.get(at + 4..at + 4 + vl as usize) else { break };
        at += 4 + vl as usize;
        out.push(Attribute { stream: 0, name: utf16(name), value: value(ty, v, true) });
    }
    out
}

fn metadata(b: &[u8]) -> Vec<Attribute> {
    let mut out = Vec::new();
    let n = le16(b, 0).unwrap_or(0);
    let mut at = 2usize;
    for _ in 0..n {
        let (Some(stream), Some(nl), Some(ty), Some(vl)) = (le16(b, at + 2), le16(b, at + 4), le16(b, at + 6), le32(b, at + 8)) else { break };
        let ns = at + 12;
        let Some(name) = b.get(ns..ns + nl as usize) else { break };
        let vs = ns + nl as usize;
        let Some(v) = vs.checked_add(vl as usize).and_then(|e| b.get(vs..e)) else { break };
        at = vs + vl as usize;
        out.push(Attribute { stream, name: utf16(name), value: value(ty, v, false) });
    }
    out
}

// ---------------------------------------------------------------------------------------------
// packets

/// A piece of a media object in a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub stream: u8,
    pub key: bool,
    pub object: u32,
    pub offset: u32,
    /// the whole object's size (a compressed payload's object: its own length)
    pub object_size: u32,
    /// presentation time in ms (preroll included)
    pub time: u32,
    pub data: Vec<u8>,
}

/// A packet's send time (ms) and payloads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    pub send_time: u32,
    pub duration: u16,
    pub payloads: Vec<Payload>,
}

/// A field of length type `t` (0 none, 1 byte, 2 word, 3 dword) at `*at`.
fn field(b: &[u8], at: &mut usize, t: u8) -> Option<u32> {
    let v = match t & 3 {
        0 => return Some(0),
        1 => *b.get(*at)? as u32,
        2 => le16(b, *at)? as u32,
        _ => le32(b, *at)?,
    };
    *at += [0, 1, 2, 4][(t & 3) as usize];
    Some(v)
}

/// Parses one packet (`b`: the packet's `packet_size` bytes). What a damaged packet holds before
/// its damage is kept.
pub fn packet(b: &[u8]) -> Result<Packet, Error> {
    let mut at = 0usize;
    let mut flags = *b.first().ok_or(Error::Truncated)?;
    if flags & 0x80 != 0 {
        // error correction data: its length in the low nibble (unless "opaque" is set)
        if flags & 0x10 != 0 {
            return Err(Error::Bad("opaque error correction"));
        }
        at = 1 + (flags & 0x0f) as usize;
        flags = *b.get(at).ok_or(Error::Truncated)?;
    }
    let props = *b.get(at + 1).ok_or(Error::Truncated)?;
    at += 2;
    let multiple = flags & 1 != 0;
    let len = field(b, &mut at, flags >> 5).ok_or(Error::Truncated)? as usize;
    let _sequence = field(b, &mut at, flags >> 1).ok_or(Error::Truncated)?;
    let mut padding = field(b, &mut at, flags >> 3).ok_or(Error::Truncated)? as usize;
    let send_time = le32(b, at).ok_or(Error::Truncated)?;
    let duration = le16(b, at + 4).ok_or(Error::Truncated)?;
    at += 6;
    if len != 0 && len < b.len() {
        // (a shorter packet: the rest is padding)
        padding += b.len() - len;
    }
    let end = b.len().saturating_sub(padding).max(at);
    let mut p = Packet { send_time, duration, payloads: Vec::new() };
    let (count, ptype) = if multiple {
        let f = *b.get(at).ok_or(Error::Truncated)?;
        at += 1;
        ((f & 0x3f) as usize, f >> 6)
    } else {
        (1, 0)
    };
    if props >> 6 != 1 {
        return Err(Error::Bad("stream number field not a byte"));
    }
    for _ in 0..count {
        let sn = *b.get(at).ok_or(Error::Truncated)?;
        at += 1;
        let object = field(b, &mut at, props >> 4).ok_or(Error::Truncated)?;
        let offset = field(b, &mut at, props >> 2).ok_or(Error::Truncated)?;
        let rlen = field(b, &mut at, props).ok_or(Error::Truncated)? as usize;
        let (stream, key) = (sn & 0x7f, sn & 0x80 != 0);
        if rlen == 1 {
            // compressed payload: `offset` is the presentation time, then a time delta, then
            // sub-payloads (a length byte, its object)
            let delta = *b.get(at).ok_or(Error::Truncated)? as u32;
            at += 1;
            let plen = if multiple { field(b, &mut at, ptype).ok_or(Error::Truncated)? as usize } else { end.saturating_sub(at) };
            let stop = at.checked_add(plen).filter(|&e| e <= end).ok_or(Error::Truncated)?;
            let mut t = offset;
            let mut obj = object;
            while at < stop {
                let n = b[at] as usize;
                at += 1;
                let d = b.get(at..at + n).filter(|_| at + n <= stop).ok_or(Error::Truncated)?;
                at += n;
                p.payloads.push(Payload { stream, key, object: obj, offset: 0, object_size: n as u32, time: t, data: d.to_vec() });
                obj = obj.wrapping_add(1);
                t = t.wrapping_add(delta);
            }
            continue;
        }
        let rep = b.get(at..at + rlen).ok_or(Error::Truncated)?;
        at += rlen;
        let (object_size, time) = if rlen >= 8 { (le32(rep, 0).unwrap_or(0), le32(rep, 4).unwrap_or(0)) } else { (0, 0) };
        let plen = if multiple { field(b, &mut at, ptype).ok_or(Error::Truncated)? as usize } else { end.saturating_sub(at) };
        let d = at.checked_add(plen).filter(|&e| e <= end).and_then(|e| b.get(at..e)).ok_or(Error::Truncated)?;
        at += plen;
        // (no replicated data: the payload is the whole object)
        let object_size = if rlen >= 8 { object_size } else { plen as u32 };
        p.payloads.push(Payload { stream, key, object, offset, object_size, time, data: d.to_vec() });
    }
    Ok(p)
}

// ---------------------------------------------------------------------------------------------
// media objects

/// A stream's media object, whole: a sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub stream: u8,
    pub key: bool,
    /// presentation time, 100 ns (preroll taken off)
    pub time: u64,
    pub data: Vec<u8>,
}

struct Partial {
    object: u32,
    key: bool,
    time: u32,
    data: Vec<u8>,
    got: usize,
}

/// Puts media objects back together from payloads.
#[derive(Default)]
pub struct Assembler {
    parts: HashMap<u8, Partial>,
    spread: HashMap<u8, Spread>,
    preroll: u64,
}

/// Objects are never larger than this (a damaged size must not allocate gigabytes).
pub const MAX_OBJECT: u32 = 64 << 20;

impl Assembler {
    pub fn new(h: &Header) -> Assembler {
        Assembler { parts: HashMap::new(), spread: h.streams.iter().filter_map(|s| Some((s.number, s.spread?))).collect(), preroll: h.preroll }
    }
    /// Drops the objects begun (after a seek).
    pub fn reset(&mut self) {
        self.parts.clear();
    }
    /// Takes a payload; a media object it completes comes out.
    pub fn push(&mut self, p: Payload) -> Option<Sample> {
        let size = p.object_size.min(MAX_OBJECT) as usize;
        let off = p.offset as usize;
        let fresh = !matches!(self.parts.get(&p.stream), Some(q) if q.object == p.object);
        if fresh {
            // (a new object drops an unfinished one: lost packets)
            if off != 0 {
                self.parts.remove(&p.stream);
                return None;
            }
            self.parts.insert(p.stream, Partial { object: p.object, key: p.key, time: p.time, data: vec![0; size], got: 0 });
        }
        let q = self.parts.get_mut(&p.stream)?;
        let end = off.saturating_add(p.data.len()).min(q.data.len());
        if off < end {
            q.data[off..end].copy_from_slice(&p.data[..end - off]);
            q.got += end - off;
        }
        if q.got < q.data.len() {
            return None;
        }
        let q = self.parts.remove(&p.stream)?;
        let mut data = q.data;
        if let Some(s) = self.spread.get(&p.stream) {
            data = descramble(&data, *s);
        }
        let time = (q.time as u64).saturating_sub(self.preroll).saturating_mul(MS);
        Some(Sample { stream: p.stream, key: q.key, time, data })
    }
}

/// Undoes the Audio Spread interleaving of one span (`d`: `span` x `packet_size` bytes; any
/// other size is left as it is).
pub fn descramble(d: &[u8], s: Spread) -> Vec<u8> {
    let (span, ps, cs) = (s.span as usize, s.packet_size as usize, s.chunk_size as usize);
    if span <= 1 || cs == 0 || ps % cs != 0 || d.len() != ps * span {
        return d.to_vec();
    }
    let mut out = vec![0u8; d.len()];
    let mut offset = 0;
    while offset < d.len() {
        let off = offset / cs;
        let (row, col) = (off / span, off % span);
        let idx = row + col * (ps / cs);
        out[offset..offset + cs].copy_from_slice(&d[idx * cs..idx * cs + cs]);
        offset += cs;
    }
    out
}

/// The interleaving `descramble` undoes (for tests, and the writer of test files).
pub fn scramble(d: &[u8], s: Spread) -> Vec<u8> {
    let (span, ps, cs) = (s.span as usize, s.packet_size as usize, s.chunk_size as usize);
    if span <= 1 || cs == 0 || ps % cs != 0 || d.len() != ps * span {
        return d.to_vec();
    }
    let mut out = vec![0u8; d.len()];
    let mut offset = 0;
    while offset < d.len() {
        let off = offset / cs;
        let (row, col) = (off / span, off % span);
        let idx = row + col * (ps / cs);
        out[idx * cs..idx * cs + cs].copy_from_slice(&d[offset..offset + cs]);
        offset += cs;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// reading a file

/// Where a file's bytes come from (a file, or a game's IStream).
pub trait Source {
    /// Reads up to `buf.len()` bytes at `off`; how many it read (0 at the end).
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize;
}

impl Source for &[u8] {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        let Ok(o) = usize::try_from(off) else { return 0 };
        let Some(s) = self.get(o..) else { return 0 };
        let n = s.len().min(buf.len());
        buf[..n].copy_from_slice(&s[..n]);
        n
    }
}

impl<S: Source + ?Sized> Source for Box<S> {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        (**self).read_at(off, buf)
    }
}

impl Source for Vec<u8> {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        self.as_slice().read_at(off, buf)
    }
}

fn read_full<S: Source + ?Sized>(s: &mut S, off: u64, buf: &mut [u8]) -> usize {
    let mut got = 0;
    while got < buf.len() {
        let n = s.read_at(off + got as u64, &mut buf[got..]);
        if n == 0 {
            break;
        }
        got += n;
    }
    got
}

/// Reads the header of the file in `s`.
pub fn read_header<S: Source + ?Sized>(s: &mut S) -> Result<Header, Error> {
    let mut head = [0u8; 30];
    if read_full(s, 0, &mut head) < 30 {
        return Err(Error::Truncated);
    }
    let n = header_size(&head).ok_or(Error::NotAsf)?;
    if n > 16 << 20 {
        return Err(Error::Bad("header over 16 MiB"));
    }
    let mut b = vec![0u8; n as usize];
    let got = read_full(s, 0, &mut b);
    b.truncate(got);
    header(&b)
}

/// The samples of a file, in file order.
pub struct Reader<S: Source> {
    pub src: S,
    pub header: Header,
    asm: Assembler,
    next: u64,
    queue: VecDeque<Sample>,
    /// samples of streams not in this set are dropped (None: all)
    pub only: Option<Vec<u8>>,
}

impl<S: Source> Reader<S> {
    pub fn new(mut src: S) -> Result<Reader<S>, Error> {
        let header = read_header(&mut src)?;
        let asm = Assembler::new(&header);
        Ok(Reader { src, header, asm, next: 0, queue: VecDeque::new(), only: None })
    }

    fn packet_at(&mut self, k: u64) -> Option<Packet> {
        if self.header.packets != 0 && k >= self.header.packets {
            return None;
        }
        let size = self.header.packet_size as usize;
        let off = self.header.data_offset.checked_add(k.checked_mul(size as u64)?)?;
        let mut b = vec![0u8; size];
        if read_full(&mut self.src, off, &mut b) < size {
            return None;
        }
        // (a damaged packet is skipped)
        Some(packet(&b).unwrap_or_default())
    }

    /// The next sample (of the streams wanted), or None at the end.
    pub fn next_sample(&mut self) -> Option<Sample> {
        loop {
            if let Some(s) = self.queue.pop_front() {
                return Some(s);
            }
            let p = self.packet_at(self.next)?;
            self.next += 1;
            for pl in p.payloads {
                if self.only.as_ref().is_some_and(|o| !o.contains(&pl.stream)) {
                    continue;
                }
                if let Some(s) = self.asm.push(pl) {
                    self.queue.push_back(s);
                }
            }
        }
    }

    /// Goes back to the start.
    pub fn rewind(&mut self) {
        self.next = 0;
        self.queue.clear();
        self.asm.reset();
    }

    /// Goes to time `t` (100 ns): the samples after come from the last key frame of the video
    /// streams at or before `t` (audio from about `t`). What comes before `t` the caller drops
    /// or decodes without showing.
    pub fn seek(&mut self, t: u64) {
        self.rewind();
        if t == 0 {
            return;
        }
        let want = t / MS + self.header.preroll;
        // the last packet sent at or before the time wanted (send times grow)
        let (mut lo, mut hi) = (0u64, self.header.packets.max(1));
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            match self.packet_at(mid) {
                Some(p) if (p.send_time as u64) <= want => lo = mid,
                _ => hi = mid,
            }
        }
        let video: Vec<u8> = self.header.streams.iter().filter(|s| matches!(s.kind, Kind::Video { .. })).map(|s| s.number).collect();
        // back to a packet where a key frame of the video (at or before t) begins
        let mut k = lo;
        loop {
            let p = self.packet_at(k).unwrap_or_default();
            let key = video.is_empty()
                || p.payloads.iter().any(|pl| video.contains(&pl.stream) && pl.key && pl.offset == 0 && (pl.time as u64) <= want);
            if key || k == 0 {
                break;
            }
            k -= 1;
        }
        self.next = k;
    }
}

pub mod write;

#[cfg(test)]
mod tests;
