//! Writing ASF files: what the tests (here and sake's) read back. Packets of a fixed size with
//! multiple payloads (word lengths, 8 bytes of replicated data), objects split over packets,
//! small ones optionally as compressed payloads, Audio Spread scrambling where a stream has it.

use crate::*;
use std::collections::HashMap;

/// An ASF writer.
pub struct Mux {
    pub packet_size: usize,
    pub preroll: u64,
    pub streams: Vec<Stream>,
    pub attributes: Vec<(String, Value)>,
    pub compress_small: bool,
}

fn obj(g: &Guid, body: &[u8]) -> Vec<u8> {
    let mut o = g.to_vec();
    o.extend(((body.len() + 24) as u64).to_le_bytes());
    o.extend(body);
    o
}
fn w16(v: &mut Vec<u8>, x: u16) {
    v.extend(x.to_le_bytes());
}
fn w32(v: &mut Vec<u8>, x: u32) {
    v.extend(x.to_le_bytes());
}
fn w64(v: &mut Vec<u8>, x: u64) {
    v.extend(x.to_le_bytes());
}
fn wstr(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    v.extend([0, 0]);
    v
}

impl Mux {
    /// The file of `samples` (stream, key, time ms without preroll, data), in this order.
    pub fn write(&self, samples: &[(u8, bool, u32, Vec<u8>)], duration_ms: u64) -> Vec<u8> {
        // the packets first (their count goes in the header)
        let mut packets: Vec<Vec<u8>> = Vec::new();
        // per packet: payloads (stream byte, object, offset, replicated, data) or compressed
        enum P {
            Plain { sn: u8, object: u8, offset: u32, size: u32, time: u32, data: Vec<u8> },
            Comp { sn: u8, object: u8, time: u32, data: Vec<u8> },
        }
        let mut objects: HashMap<u8, u8> = HashMap::new();
        let mut cur: Vec<P> = Vec::new();
        let mut used = 0usize;
        // packet head: flags, props, word length, no sequence, word padding, send time, duration,
        // payload flags
        let head = 1 + 1 + 2 + 2 + 4 + 2 + 1;
        let flush = |cur: &mut Vec<P>, used: &mut usize, packets: &mut Vec<Vec<u8>>| {
            if cur.is_empty() {
                return;
            }
            let mut b = Vec::new();
            let send = cur.iter().map(|p| match p { P::Plain { time, .. } | P::Comp { time, .. } => *time }).min().unwrap_or(0);
            b.push(0x01 | (2 << 3) | (2 << 5)); // multiple payloads, word padding, word length
            b.push(0x01 | (3 << 2) | (1 << 4) | (1 << 6)); // replicated byte, offset dword, object byte, stream byte
            w16(&mut b, self.packet_size as u16);
            let pad = self.packet_size - head - *used;
            w16(&mut b, pad as u16);
            w32(&mut b, send.saturating_sub(100));
            w16(&mut b, 0);
            b.push(cur.len() as u8 | (2 << 6)); // word payload lengths
            for p in cur.drain(..) {
                match p {
                    P::Plain { sn, object, offset, size, time, data } => {
                        b.push(sn);
                        b.push(object);
                        w32(&mut b, offset);
                        b.push(8);
                        w32(&mut b, size);
                        w32(&mut b, time);
                        w16(&mut b, data.len() as u16);
                        b.extend(data);
                    }
                    P::Comp { sn, object, time, data } => {
                        b.push(sn);
                        b.push(object);
                        w32(&mut b, time);
                        b.push(1);
                        b.push(0); // delta
                        w16(&mut b, (data.len() + 1) as u16);
                        b.push(data.len() as u8);
                        b.extend(data);
                    }
                }
            }
            b.resize(self.packet_size, 0);
            packets.push(b);
            *used = 0;
        };
        for (stream, key, t, data) in samples {
            let o = objects.entry(*stream).or_insert(0);
            let object = *o;
            *o = o.wrapping_add(1);
            let sn = stream | if *key { 0x80 } else { 0 };
            let time = *t + self.preroll as u32;
            let spread = self.streams.iter().find(|s| s.number == *stream).and_then(|s| s.spread);
            let data = match spread {
                Some(s) => scramble(data, s),
                None => data.clone(),
            };
            if self.compress_small && data.len() < 200 && !data.is_empty() {
                let need = 1 + 1 + 4 + 1 + 1 + 2 + 1 + data.len();
                if used + need > self.packet_size - head {
                    flush(&mut cur, &mut used, &mut packets);
                }
                used += need;
                cur.push(P::Comp { sn, object, time, data });
                continue;
            }
            let mut off = 0usize;
            loop {
                // (an empty object is still one payload)
                let room = (self.packet_size - head).saturating_sub(used + 1 + 1 + 4 + 1 + 8 + 2);
                if room == 0 {
                    flush(&mut cur, &mut used, &mut packets);
                    continue;
                }
                let n = room.min(data.len() - off);
                used += 1 + 1 + 4 + 1 + 8 + 2 + n;
                cur.push(P::Plain { sn, object, offset: off as u32, size: data.len() as u32, time, data: data[off..off + n].to_vec() });
                off += n;
                if off >= data.len() {
                    break;
                }
                flush(&mut cur, &mut used, &mut packets);
            }
        }
        flush(&mut cur, &mut used, &mut packets);

        let mut hdr = Vec::new();
        let mut fp = vec![0u8; 16];
        w64(&mut fp, 0);
        w64(&mut fp, 0);
        w64(&mut fp, packets.len() as u64);
        w64(&mut fp, (duration_ms + self.preroll) * 10_000);
        w64(&mut fp, duration_ms * 10_000);
        w64(&mut fp, self.preroll);
        w32(&mut fp, 2);
        w32(&mut fp, self.packet_size as u32);
        w32(&mut fp, self.packet_size as u32);
        w32(&mut fp, 1_000_000);
        hdr.push(obj(&FILE_PROPERTIES, &fp));
        let mut ext_inner = Vec::new();
        for s in &self.streams {
            let (ty, ts) = match &s.kind {
                Kind::Audio => (AUDIO_MEDIA, s.format.clone()),
                Kind::Video { width, height } => {
                    let mut t = Vec::new();
                    w32(&mut t, *width);
                    w32(&mut t, *height);
                    t.push(2);
                    w16(&mut t, s.format.len() as u16);
                    t.extend(&s.format);
                    (VIDEO_MEDIA, t)
                }
                Kind::Other(g) => (*g, s.format.clone()),
            };
            let (ec, es) = match s.spread {
                Some(sp) => {
                    let mut e = vec![sp.span];
                    w16(&mut e, sp.packet_size);
                    w16(&mut e, sp.chunk_size);
                    w16(&mut e, 1);
                    e.push(0);
                    (AUDIO_SPREAD, e)
                }
                None => (NO_ERROR_CORRECTION, Vec::new()),
            };
            let mut sp = ty.to_vec();
            sp.extend(ec);
            w64(&mut sp, s.time_offset);
            w32(&mut sp, ts.len() as u32);
            w32(&mut sp, es.len() as u32);
            w16(&mut sp, s.number as u16 | if s.encrypted { 0x8000 } else { 0 });
            w32(&mut sp, 0);
            sp.extend(ts);
            sp.extend(es);
            hdr.push(obj(&STREAM_PROPERTIES, &sp));
            let mut x = Vec::new();
            w64(&mut x, 0);
            w64(&mut x, 0);
            w32(&mut x, s.bitrate);
            // buffer size, initial fullness, alternates (3), max object size, flags
            for _ in 0..7 {
                w32(&mut x, 0);
            }
            w16(&mut x, s.number as u16);
            w16(&mut x, 0);
            w64(&mut x, s.avg_time_per_frame);
            w16(&mut x, 0);
            w16(&mut x, 0);
            ext_inner.extend(obj(&EXTENDED_STREAM_PROPERTIES, &x));
        }
        let mut he = vec![0u8; 16];
        w16(&mut he, 6);
        w32(&mut he, ext_inner.len() as u32);
        he.extend(ext_inner);
        hdr.push(obj(&HEADER_EXTENSION, &he));
        if !self.attributes.is_empty() {
            let mut e = Vec::new();
            w16(&mut e, self.attributes.len() as u16);
            for (n, v) in &self.attributes {
                let name = wstr(n);
                w16(&mut e, name.len() as u16);
                e.extend(name);
                let (ty, b) = match v {
                    Value::Str(s) => (0, wstr(s)),
                    Value::Bytes(b) => (1, b.clone()),
                    Value::Bool(b) => (2, (*b as u32).to_le_bytes().to_vec()),
                    Value::U32(x) => (3, x.to_le_bytes().to_vec()),
                    Value::U64(x) => (4, x.to_le_bytes().to_vec()),
                    Value::U16(x) => (5, x.to_le_bytes().to_vec()),
                    Value::Guid(g) => (6, g.to_vec()),
                };
                w16(&mut e, ty);
                w16(&mut e, b.len() as u16);
                e.extend(b);
            }
            hdr.push(obj(&EXTENDED_CONTENT_DESCRIPTION, &e));
        }
        let body: Vec<u8> = hdr.concat();
        let mut f = HEADER.to_vec();
        w64(&mut f, (30 + body.len()) as u64);
        w32(&mut f, hdr.len() as u32);
        f.extend([1, 2]);
        f.extend(body);
        f.extend(DATA);
        w64(&mut f, (50 + packets.len() * self.packet_size) as u64);
        f.extend([0u8; 16]);
        w64(&mut f, packets.len() as u64);
        f.extend([1, 1]);
        for p in packets {
            f.extend(p);
        }
        f
    }
}

