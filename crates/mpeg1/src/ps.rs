//! MPEG-1 system streams (ISO/IEC 11172-1): the packets of the video (0xE0..0xEF) and audio
//! (0xC0..0xDF) streams, with their presentation times, fed bytes as they come. As pl_mpeg's
//! plm_demux: pack and system headers are passed over by looking for the next start code.

use crate::bits::Bits;

#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    /// the stream id (0xE0.. video, 0xC0.. audio, 0xBD private)
    pub id: u8,
    /// presentation time, seconds
    pub pts: Option<f64>,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct Demux {
    buf: Vec<u8>,
    at: usize,
    ended: bool,
}

pub fn is_video(id: u8) -> bool {
    (0xe0..=0xef).contains(&id)
}
pub fn is_audio(id: u8) -> bool {
    (0xc0..=0xdf).contains(&id)
}

/// Whether `d` starts as a system stream (a pack header, 00 00 01 BA).
pub fn is_system(d: &[u8]) -> bool {
    d.len() >= 4 && d[..4] == [0, 0, 1, 0xba]
}

/// Whether `d` starts as a bare video stream (a sequence header, 00 00 01 B3).
pub fn is_video_stream(d: &[u8]) -> bool {
    d.len() >= 4 && d[..4] == [0, 0, 1, 0xb3]
}

fn time(b: &mut Bits) -> f64 {
    let mut clock = (b.read(3) as u64) << 30;
    b.skip(1);
    clock |= (b.read(15) as u64) << 15;
    b.skip(1);
    clock |= b.read(15) as u64;
    b.skip(1);
    clock as f64 / 90000.0
}

impl Demux {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, d: &[u8]) {
        if self.at > 1 << 20 && self.at * 2 > self.buf.len() {
            self.buf.drain(..self.at);
            self.at = 0;
        }
        self.buf.extend_from_slice(d);
    }

    pub fn end(&mut self) {
        self.ended = true;
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.at = 0;
        self.ended = false;
    }

    /// The next whole packet of a video, audio or private stream.
    pub fn next(&mut self) -> Option<Packet> {
        loop {
            let d = &self.buf;
            let mut i = self.at;
            let found = loop {
                if i + 4 > d.len() {
                    break None;
                }
                if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
                    break Some(d[i + 3]);
                }
                i += 1;
            };
            let Some(id) = found else {
                // (keep the last 3 bytes: a start code may be cut there)
                self.at = d.len().saturating_sub(3).max(self.at);
                return None;
            };
            if !(is_video(id) || is_audio(id) || id == 0xbd) {
                self.at = i + 4;
                continue;
            }
            // the packet header: length, stuffing, STD buffer, time stamps
            if i + 6 > d.len() {
                self.at = i;
                return None;
            }
            let len = u16::from_be_bytes([d[i + 4], d[i + 5]]) as usize;
            let start = i + 6;
            if start + len > d.len() {
                if !self.ended {
                    self.at = i;
                    return None;
                }
                // (a cut last packet: what there is)
            }
            let end = (start + len).min(d.len());
            let body = &d[start..end];
            let mut b = Bits::new(body);
            b.skip_bytes(0xff);
            if b.read(2) == 1 {
                b.skip(14);
            } else {
                b.pos -= 2.min(b.pos);
            }
            let pts = match b.read(4) {
                0b0011 => {
                    let t = time(&mut b);
                    b.skip(40);
                    Some(t)
                }
                0b0010 => Some(time(&mut b)),
                0b0000 => {
                    b.skip(4); // 1111
                    None
                }
                _ => {
                    self.at = i + 4;
                    continue;
                }
            };
            let data = body[(b.pos >> 3).min(body.len())..].to_vec();
            self.at = end;
            return Some(Packet { id, pts, data });
        }
    }
}

/// The video and audio packets of a whole system stream (or a bare video stream as one
/// packet), for tools and tests.
pub fn packets(d: &[u8]) -> Vec<Packet> {
    if is_video_stream(d) {
        return vec![Packet { id: 0xe0, pts: None, data: d.to_vec() }];
    }
    let mut m = Demux::new();
    m.push(d);
    m.end();
    std::iter::from_fn(|| m.next()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pes(id: u8, pts: Option<u64>, data: &[u8]) -> Vec<u8> {
        let mut h = vec![];
        match pts {
            Some(t) => {
                h.push(0x21 | ((t >> 29) as u8 & 0x0e));
                h.extend_from_slice(&(((t >> 14) as u16 & 0xfffe) | 1).to_be_bytes());
                h.extend_from_slice(&(((t << 1) as u16 & 0xfffe) | 1).to_be_bytes());
            }
            None => h.push(0x0f),
        }
        let mut v = vec![0, 0, 1, id];
        v.extend_from_slice(&((h.len() + data.len()) as u16).to_be_bytes());
        v.extend(h);
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn packets_with_and_without_times() {
        let mut s = vec![0, 0, 1, 0xba, 0x21, 0, 1, 0, 1, 0x80, 0, 1];
        s.extend(pes(0xe0, Some(90000 * 3 / 2), b"video"));
        s.extend(pes(0xc0, None, b"audio!"));
        s.extend(pes(0xbe, None, &[0xff; 5])); // padding: left out
        s.extend([0, 0, 1, 0xb9]);
        let p = packets(&s);
        assert_eq!(p, [Packet { id: 0xe0, pts: Some(1.5), data: b"video".to_vec() }, Packet { id: 0xc0, pts: None, data: b"audio!".to_vec() }]);
    }

    #[test]
    fn bytes_one_at_a_time_give_the_same_packets() {
        let mut s = vec![0, 0, 1, 0xba, 0x21, 0, 1, 0, 1, 0x80, 0, 1];
        for k in 0..5u8 {
            s.extend(pes(0xe0, Some(3003 * k as u64), &vec![k; 100 + k as usize]));
            s.extend(pes(0xc0, Some(2000 * k as u64), &vec![k + 9; 50]));
        }
        let all = packets(&s);
        let mut m = Demux::new();
        let mut got = vec![];
        for b in &s {
            m.push(&[*b]);
            while let Some(p) = m.next() {
                got.push(p);
            }
        }
        m.end();
        while let Some(p) = m.next() {
            got.push(p);
        }
        assert_eq!(got.len(), 10);
        assert_eq!(got, all);
    }
}
