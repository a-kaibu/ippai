//! Sound files as Media Foundation's Source Reader opens them (crates/sake/src/mf_reader.rs):
//! the container read here, the packets decoded by the page's AudioDecoder
//! (crates/kura/src/adec.rs) or, for PCM, taken as they are.
//!
//! - WAVE: PCM (8/16/24/32-bit, 32-bit float) or MP3 data (WAVE_FORMAT_MPEGLAYER3)
//! - MP3: layer III frames (crates/sake/src/mp3frame.rs), an ID3v2 tag before them
//! - MP4 / M4A / 3GP: the first sound track's AAC (`mp4a`, its esds' AudioSpecificConfig as the
//!   decoder's description) or MP3 (`.mp3`, object type 0x6b)
//!
//! For Media Foundation only (`open_more`, #85), as Windows 10's own FLAC and ASF sources, and
//! Ogg as with the Web Media Extensions installed:
//! - FLAC: its frames, for the page's AudioDecoder ("flac", the STREAMINFO as description)
//! - Ogg Vorbis: its packets, decoded here (crates/vorbis)
//! - ASF (.wma): the first sound stream's media objects, decoded here (crates/wmcodec)
//!
//! Ogg and ASF packets are not where the file has them (pages, payloads): they are copied, one
//! after another, into a buffer of their own, which the track's offsets are into.
//!
//! ADTS is not here: it fails to open (MF_E_UNSUPPORTED_BYTESTREAM_TYPE).

/// How a track's packets become samples.
#[derive(Clone, Debug, PartialEq)]
pub enum Codec {
    /// little-endian PCM as it is: bits per sample (8 unsigned, 16/24/32 signed), or float
    Pcm { bits: u16, float: bool },
    /// for the page's AudioDecoder: WebCodecs' codec string and description
    Page { codec: String, desc: Vec<u8>, subtype: u32 },
    /// Vorbis, decoded here: the identification and setup headers
    Vorbis { ident: Vec<u8>, setup: Vec<u8> },
    /// a Windows Media sound stream (WMA 1 / 2), decoded here: its WAVEFORMATEX
    Wma { info: crate::wmdec::Info },
}

/// A file's sound: its packets (offset, bytes) and how many frames each holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub codec: Codec,
    pub rate: u32,
    pub channels: u32,
    pub packets: Vec<(u64, u32)>,
    /// frames per packet (per channel)
    pub frames: Vec<u32>,
    /// the file's bit rate, bytes a second (MF_MT_AUDIO_AVG_BYTES_PER_SECOND of the native type)
    pub avg: u32,
    /// the container's own length (100 ns) where it gives one: MP4's movie duration, which
    /// Windows' MP4 source has as MF_PD_DURATION (winref mfreader: 500 ms for a track whose
    /// samples, AAC's 1024 of priming among them, last 523 ms)
    pub duration: Option<u64>,
}

impl Track {
    pub fn total_frames(&self) -> u64 {
        self.frames.iter().map(|&f| f as u64).sum()
    }
}

/// WAVE_FORMAT_* of the native type's subtype (MFAudioFormat_* is it in Data1)
pub const PCM: u32 = 1;
pub const FLOAT: u32 = 3;
pub const MP3: u32 = 0x55;
pub const AAC: u32 = 0x1610;
/// MFAudioFormat_FLAC
pub const FLAC: u32 = 0xf1ac;

/// `open`, and for Media Foundation also FLAC, Ogg Vorbis and WMA; with the buffer the track's
/// packets are in when they are not in `b` (Ogg, ASF).
pub fn open_more(b: &[u8]) -> Option<(Track, Option<Vec<u8>>)> {
    if b.starts_with(b"fLaC") {
        return flac(b).map(|t| (t, None));
    }
    if b.starts_with(b"OggS") {
        return ogg_vorbis(b).map(|(t, d)| (t, Some(d)));
    }
    if asf::header_size(b).is_some() {
        return wma(b).map(|(t, d)| (t, Some(d)));
    }
    open(b).map(|t| (t, None))
}

/// Packets one after another: the buffer and each one's (offset, bytes).
fn repack<'a>(ps: impl Iterator<Item = &'a [u8]>) -> (Vec<u8>, Vec<(u64, u32)>) {
    let (mut d, mut at) = (Vec::new(), Vec::new());
    for p in ps {
        at.push((d.len() as u64, p.len() as u32));
        d.extend_from_slice(p);
    }
    (d, at)
}

/// Ogg Vorbis: every audio packet's frames from its block size and the one before (the first
/// gives none), the last cut to the stream's final granule position.
fn ogg_vorbis(b: &[u8]) -> Option<(Track, Vec<u8>)> {
    let (ps, granule) = vorbis::ogg::packets(b);
    if ps.len() < 3 {
        return None;
    }
    let ident = vorbis::Ident::read(&ps[0]).ok()?;
    let (rate, channels) = (ident.rate, ident.channels as u32);
    let setup = vorbis::Setup::read(vorbis::Ident::read(&ps[0]).ok()?, &ps[2]).ok()?;
    let audio = &ps[3..];
    let mut frames = Vec::with_capacity(audio.len());
    let mut prev = 0usize;
    let mut total = 0u64;
    for p in audio {
        let mut bits = vorbis::Bits::new(p);
        let n = match vorbis::decode::packet_header(&setup, &mut bits) {
            Some((mode, _, _)) => setup.ident.blocksize[setup.modes[mode].blockflag as usize],
            None => {
                frames.push(0);
                continue;
            }
        };
        let f = if prev > 0 { (prev / 4 + n / 4) as u64 } else { 0 };
        prev = n;
        // (the end: no further than the last granule position)
        let f = if granule > 0 { f.min(granule.saturating_sub(total)) } else { f };
        total += f;
        frames.push(f as u32);
    }
    if rate == 0 || channels == 0 || total == 0 {
        return None;
    }
    let (d, packets) = repack(audio.iter().map(|p| &p[..]));
    let avg = (d.len() as u128 * rate as u128 / total as u128) as u32;
    let codec = Codec::Vorbis { ident: ps[0].clone(), setup: ps[2].clone() };
    Some((Track { codec, rate, channels, packets, frames, avg, duration: None }, d))
}

/// CRC-8 (poly 7) of a FLAC frame header.
fn crc8(b: &[u8]) -> u8 {
    let mut c = 0u8;
    for &x in b {
        c ^= x;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { c << 1 ^ 7 } else { c << 1 };
        }
    }
    c
}

/// A FLAC frame header at `b[o..]`: its block size (frames) and number, if it is one (sync,
/// no reserved values, its CRC-8).
pub fn flac_frame(b: &[u8], o: usize) -> Option<(u32, u64)> {
    let h = b.get(o..)?;
    if h.len() < 6 || h[0] != 0xff || h[1] & 0xfe != 0xf8 {
        return None;
    }
    let (bs, sr) = (h[2] >> 4, h[2] & 15);
    let (ch, ss) = (h[3] >> 4, h[3] >> 1 & 7);
    if bs == 0 || sr == 15 || ch > 10 || ss == 3 || h[3] & 1 != 0 {
        return None;
    }
    // the frame (or sample) number, UTF-8 coded
    let lead = h[4].leading_ones() as usize;
    let len = match lead {
        0 => 1,
        2..=7 => lead,
        _ => return None,
    };
    let mut num = (h[4] & (0xff >> (lead + 1))) as u64;
    for i in 1..len {
        let x = *h.get(4 + i)?;
        if x & 0xc0 != 0x80 {
            return None;
        }
        num = num << 6 | (x & 0x3f) as u64;
    }
    let mut at = 4 + len;
    let size = match bs {
        1 => 192,
        2..=5 => 576 << (bs - 2),
        6 => {
            at += 1;
            *h.get(at - 1)? as u32 + 1
        }
        7 => {
            at += 2;
            u16::from_be_bytes([*h.get(at - 2)?, *h.get(at - 1)?]) as u32 + 1
        }
        _ => 256 << (bs - 8),
    };
    at += match sr {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    (crc8(h.get(..at)?) == *h.get(at)?).then_some((size, num))
}

/// FLAC: STREAMINFO, then the frames (each found by its header: the next valid header numbered
/// one on ends it).
fn flac(b: &[u8]) -> Option<Track> {
    let mut o = 4;
    let mut info = None;
    loop {
        let h = *b.get(o)?;
        let len = u32::from_be_bytes([0, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]) as usize;
        if h & 0x7f == 0 && len >= 34 {
            info = Some(o);
        }
        o = o.checked_add(4 + len)?;
        if h & 0x80 != 0 {
            break;
        }
    }
    let si = info?;
    let s = &b[si + 4..si + 4 + 34];
    let rate = (s[10] as u32) << 12 | (s[11] as u32) << 4 | (s[12] as u32) >> 4;
    let channels = (s[12] >> 1 & 7) as u32 + 1;
    let total = ((s[13] & 15) as u64) << 32 | u32::from_be_bytes([s[14], s[15], s[16], s[17]]) as u64;
    if rate == 0 {
        return None;
    }
    // "fLaC" and STREAMINFO alone, marked the last block (WebCodecs' description)
    let mut desc = b"fLaC".to_vec();
    desc.push(0x80);
    desc.extend_from_slice(&b[si + 1..si + 4 + 34]);
    let (mut packets, mut frames) = (Vec::new(), Vec::new());
    let mut cur = flac_frame(b, o).map(|f| (o, f));
    while let Some((at, (size, num))) = cur {
        let mut q = at + 6;
        cur = None;
        while q + 6 <= b.len() {
            if b[q] == 0xff {
                if let Some(f) = flac_frame(b, q).filter(|f| f.1 == num + 1 || f.1 == num + size as u64) {
                    cur = Some((q, f));
                    break;
                }
            }
            q += 1;
        }
        let end = cur.map_or(b.len(), |c| c.0);
        packets.push((at as u64, (end - at) as u32));
        frames.push(size);
    }
    // (the last frame is shorter than its block size says only through STREAMINFO's total)
    let sum: u64 = frames.iter().map(|&f| f as u64).sum();
    if total > 0 && sum > total {
        if let Some(l) = frames.last_mut() {
            *l = l.saturating_sub((sum - total).min(*l as u64) as u32);
        }
    }
    let n = total.max(1).min(sum.max(1));
    let bytes: u64 = packets.iter().map(|p| p.1 as u64).sum();
    if packets.is_empty() {
        return None;
    }
    let avg = (bytes as u128 * rate as u128 / n as u128) as u32;
    Some(Track { codec: Codec::Page { codec: "flac".into(), desc, subtype: FLAC }, rate, channels, packets, frames, avg, duration: None })
}

/// ASF: the first sound stream sake's Windows Media decoders take, its media objects; each
/// one's frames from the presentation times (the last to the file's play duration).
fn wma(b: &[u8]) -> Option<(Track, Vec<u8>)> {
    let mut r = asf::Reader::new(b.to_vec()).ok()?;
    let st = r.header.streams.iter().find(|s| matches!(s.kind, asf::Kind::Audio) && !s.encrypted && crate::wmdec::Info::from_wave(&s.format).is_some_and(|i| crate::wmdec::codec(&i).is_some()))?;
    let (number, info) = (st.number, crate::wmdec::Info::from_wave(&st.format)?);
    let crate::wmdec::Info::Audio { channels, rate, bytes_per_sec, .. } = info else { return None };
    let duration = r.header.duration;
    r.only = Some(vec![number]);
    let mut objs = Vec::new();
    while let Some(s) = r.next_sample() {
        objs.push(s);
    }
    if objs.is_empty() || rate == 0 {
        return None;
    }
    let at = |t: u64| (t as u128 * rate as u128 / 10_000_000) as u64;
    let end = at(duration.max(objs.last()?.time));
    let mut frames = Vec::with_capacity(objs.len());
    for (i, o) in objs.iter().enumerate() {
        let next = objs.get(i + 1).map_or(end, |n| at(n.time));
        frames.push(next.saturating_sub(at(o.time)) as u32);
    }
    let (d, packets) = repack(objs.iter().map(|o| &o.data[..]));
    Some((Track { codec: Codec::Wma { info }, rate, channels: channels as u32, packets, frames, avg: bytes_per_sec, duration: Some(duration) }, d))
}

/// Reads the file whose bytes are `b`; None when it is none of these.
pub fn open(b: &[u8]) -> Option<Track> {
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        return wave(b);
    }
    if b.len() >= 8 && &b[4..8] == b"ftyp" {
        return mp4(b);
    }
    mp3(b, 0, b.len())
}

/// The first 64 KiB of host file `p`.
pub(crate) fn head_of(p: impl AsRef<std::path::Path>) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut v = Vec::new();
    kura::fs::File::open(p).ok()?.take(HEAD).read_to_end(&mut v).ok()?;
    Some(v)
}
pub(crate) const HEAD: u64 = 64 << 10;

/// Whether a file starting with `head` (its first HEAD bytes) can be a sound file that
/// mci::sound_pcm takes: a WAVE, an MP4 (its sound track), an ID3 tag (an MP3 whose cover art
/// can fill the head), or MP3 frames already in the head. An AVI, ASF or MPEG movie is not, and
/// is not read whole just to find that out.
pub(crate) fn maybe_sound(head: &[u8]) -> bool {
    head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE"
        || head.len() >= 8 && &head[4..8] == b"ftyp"
        || head.starts_with(b"ID3")
        || open(head).is_some()
}

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u32be(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64be(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// PCM packets: a tenth of a second each (the last one shorter), as Windows' WAVE source hands
/// out its samples (winref mfreader: 0.3 s at 22050 Hz in 3 samples of 2205 frames)
fn pcm_frames(rate: u32) -> u64 {
    (rate as u64 / 10).max(1)
}

/// Where a RIFF chunk of `len` bytes (from the file) whose body starts at `body` ends inside a
/// file of `file` bytes, and where the next chunk starts (padded to even): in 64 bits, so that
/// on wasm32 a length of 0xFFFFFFFF (streamed WAVs: ffmpeg writes it for piped output) or
/// 0xFFFFFFF8 does not wrap `body + len` to before `body` (an end before the start, or a next
/// chunk at the same offset: an endless loop, review A-7). The next start is always past `body`.
pub(crate) fn riff_chunk(body: usize, len: u32, file: usize) -> (usize, usize) {
    let end = (body as u64 + len as u64).min(file as u64) as usize;
    let next = (body as u64 + len as u64 + (len & 1) as u64).min(usize::MAX as u64) as usize;
    (end.max(body.min(file)), next)
}

fn wave(b: &[u8]) -> Option<Track> {
    let mut o = 12usize;
    let mut fmt: Option<(u16, u16, u32, u32, u16, u16)> = None;
    while o < b.len() && b.len() - o >= 8 {
        let (id, len32) = (&b[o..o + 4], u32le(b, o + 4)?);
        let body = o + 8;
        let (end, next) = riff_chunk(body, len32, b.len());
        if id == b"fmt " {
            fmt = Some((
                u16le(b, body)?,
                u16le(b, body + 2)?,
                u32le(b, body + 4)?,
                u32le(b, body + 8)?,
                u16le(b, body + 12)?,
                u16le(b, body + 14)?,
            ));
            // WAVE_FORMAT_EXTENSIBLE: the subformat GUID's first two bytes are the tag
            if fmt?.0 == 0xfffe {
                let f = fmt?;
                fmt = Some((u16le(b, body + 24)?, f.1, f.2, f.3, f.4, f.5));
            }
        } else if id == b"data" {
            let (tag, ch, rate, avg, align, bits) = fmt?;
            if ch == 0 || rate == 0 {
                return None;
            }
            return match (tag as u32, bits) {
                (PCM, 8 | 16 | 24 | 32) | (FLOAT, 32) => {
                    let align = (ch as u32 * bits as u32 / 8).max(align as u32).max(1);
                    let total = ((end - body) as u32 / align) as u64;
                    let mut packets = Vec::new();
                    let mut frames = Vec::new();
                    let mut at = 0u64;
                    while at < total {
                        let n = (total - at).min(pcm_frames(rate)) as u32;
                        packets.push((body as u64 + at * align as u64, n * align));
                        frames.push(n);
                        at += n as u64;
                    }
                    Some(Track {
                        codec: Codec::Pcm { bits, float: tag as u32 == FLOAT },
                        rate,
                        channels: ch as u32,
                        packets,
                        frames,
                        avg,
                        duration: None,
                    })
                }
                (MP3, _) => mp3(b, body, end),
                _ => None,
            };
        }
        o = next;
    }
    None
}

/// MP3 frames in `b[from..to]` (an ID3v2 tag first is skipped): all of one rate and channel
/// count, the first frame's.
fn mp3(b: &[u8], from: usize, to: usize) -> Option<Track> {
    let to = to.min(b.len());
    let mut sp = crate::mp3frame::Splitter::default();
    let mut out = Vec::new();
    sp.push(&b[from..to], &mut out);
    sp.finish(&mut out);
    let first = out.first()?.0;
    // (every frame's offset: the splitter hands out bytes, found again in order)
    let (mut packets, mut frames) = (Vec::new(), Vec::new());
    let mut at = from;
    let mut bytes = 0u64;
    for (h, f) in &out {
        let k = b[at..to].windows(f.len()).position(|w| w == &f[..])?;
        at += k;
        if h.rate == first.rate {
            packets.push((at as u64, f.len() as u32));
            frames.push(h.samples);
            bytes += f.len() as u64;
        }
        at += f.len();
    }
    // a file of mostly junk with a few false frames is not an MP3
    if packets.len() < 2 && to - from > 4096 {
        return None;
    }
    let secs = frames.iter().map(|&f| f as f64).sum::<f64>() / first.rate as f64;
    let avg = if secs > 0.0 { (bytes as f64 / secs) as u32 } else { 0 };
    Some(Track {
        codec: Codec::Page {
            codec: "mp3".into(),
            desc: Vec::new(),
            subtype: MP3,
        },
        rate: first.rate,
        channels: first.channels,
        packets,
        frames,
        avg,
        duration: None,
    })
}

// ---------------------------------------------------------------------------------------------
// MP4 (ISO base media): boxes

/// The boxes in `b[from..to]`: (type, body start, body end).
pub fn boxes(b: &[u8], from: usize, to: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut o = from;
    while o + 8 <= to {
        let Some(size) = u32be(b, o) else { break };
        let kind: [u8; 4] = b[o + 4..o + 8].try_into().unwrap();
        let (body, end) = match size {
            0 => (o + 8, to),
            1 => match u64be(b, o + 8) {
                // (a size past the address space is past `to`: no `as usize` truncation, whose
                // low bits 0 made `end == o` and the loop push boxes for ever on wasm32)
                Some(s) if s >= 16 => (o + 16, usize::try_from(s).ok().and_then(|s| o.checked_add(s)).unwrap_or(usize::MAX)),
                _ => break,
            },
            s if s >= 8 => (o + 8, o.saturating_add(s as usize)),
            _ => break,
        };
        if end > to {
            break;
        }
        out.push((kind, body, end));
        o = end;
    }
    out
}

pub fn child(b: &[u8], from: usize, to: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    boxes(b, from, to).into_iter().find(|x| &x.0 == kind).map(|x| (x.1, x.2))
}

fn mp4(b: &[u8]) -> Option<Track> {
    let (ms, me) = child(b, 0, b.len(), b"moov")?;
    for (kind, ts, te) in boxes(b, ms, me) {
        if &kind != b"trak" {
            continue;
        }
        let (ds, de) = child(b, ts, te, b"mdia")?;
        let (hs, _) = child(b, ds, de, b"hdlr")?;
        if b.get(hs + 8..hs + 12)? != b"soun" {
            continue;
        }
        let (mds, _) = child(b, ds, de, b"mdhd")?;
        let timescale = if *b.get(mds)? == 1 { u32be(b, mds + 20)? } else { u32be(b, mds + 12)? };
        let (ns, ne) = child(b, ds, de, b"minf")?;
        let (ss, se) = child(b, ns, ne, b"stbl")?;
        let mut t = sound_track(b, ss, se, timescale, b.len() as u64)?;
        t.duration = movie_duration(b, ms, me);
        return Some(t);
    }
    None
}

/// mvhd's duration (100 ns): none when it gives none (0, all ones, no timescale)
pub fn movie_duration(b: &[u8], ms: usize, me: usize) -> Option<u64> {
    let (o, _) = child(b, ms, me, b"mvhd")?;
    let (scale, d) = if *b.get(o)? == 1 {
        (u32be(b, o + 20)?, (u32be(b, o + 24)? as u64) << 32 | u32be(b, o + 28)? as u64)
    } else {
        (u32be(b, o + 12)?, u32be(b, o + 16)? as u64)
    };
    (scale != 0 && d != 0 && d != u32::MAX as u64 && d != u64::MAX).then(|| (d as u128 * 10_000_000 / scale as u128) as u64)
}

/// The descriptor at `o` in an esds: (tag, body start, body end); sizes are 7 bits a byte.
fn descriptor(b: &[u8], o: usize, end: usize) -> Option<(u8, usize, usize)> {
    let tag = *b.get(o)?;
    let mut size = 0usize;
    let mut p = o + 1;
    for _ in 0..4 {
        let x = *b.get(p)?;
        p += 1;
        size = size << 7 | (x & 0x7f) as usize;
        if x & 0x80 == 0 {
            break;
        }
    }
    let e = p.checked_add(size)?;
    (e <= end).then_some((tag, p, e))
}

/// The sound track whose stbl is `ss..se` of `b` (a moov's bytes, or the whole file) in a file
/// of `file_len` bytes (its packets must lie within).
pub fn sound_track(b: &[u8], ss: usize, se: usize, timescale: u32, file_len: u64) -> Option<Track> {
    let (sds, sde) = child(b, ss, se, b"stsd")?;
    // stsd: version/flags, count, then sample entries
    let (kind, es, ee) = *boxes(b, sds + 8, sde).first()?;
    // AudioSampleEntry: 6 reserved, data reference (2), version (2), 6 more, channels (2), sample
    // size (2), 4 more, rate (16.16); then its boxes (version 1 / 2 entries add fields)
    let ver = u16::from_be_bytes(b.get(es + 8..es + 10)?.try_into().ok()?);
    let mut channels = u16::from_be_bytes(b.get(es + 16..es + 18)?.try_into().ok()?) as u32;
    let mut rate = u32be(b, es + 24)? >> 16;
    let inner = es
        + 28
        + match ver {
            1 => 16,
            2 => 36,
            _ => 0,
        };
    let codec = match &kind {
        b"mp4a" => {
            let (xs, xe) = child(b, inner, ee, b"esds")?;
            // version/flags, then ES_Descriptor (3): ES_ID, flags (+ optional fields), then
            // DecoderConfigDescriptor (4): object type, ... then DecoderSpecificInfo (5)
            let (t, s, e) = descriptor(b, xs + 4, xe)?;
            if t != 3 {
                return None;
            }
            let flags = *b.get(s + 2)?;
            let mut p = s + 3;
            if flags & 0x80 != 0 {
                p += 2;
            }
            if flags & 0x40 != 0 {
                p += 1 + *b.get(p)? as usize;
            }
            if flags & 0x20 != 0 {
                p += 2;
            }
            let (t, ds, de) = descriptor(b, p, e)?;
            if t != 4 {
                return None;
            }
            let object = *b.get(ds)?;
            match object {
                0x40 | 0x66 | 0x67 | 0x68 => {
                    let (t, is, ie) = descriptor(b, ds + 13, de)?;
                    if t != 5 || ie - is < 2 {
                        return None;
                    }
                    let asc = b[is..ie].to_vec();
                    let aot = asc[0] >> 3;
                    // (the config's own rate and channels, where the entry's are placeholders)
                    let fi = ((asc[0] & 7) << 1 | asc[1] >> 7) as usize;
                    const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
                    if let Some(&r) = RATES.get(fi) {
                        rate = r;
                    }
                    let cc = ((asc[1] >> 3) & 15) as u32;
                    if (1..=6).contains(&cc) {
                        channels = cc;
                    } else if cc == 7 {
                        channels = 8;
                    }
                    let profile = if object == 0x40 { aot } else { 2 };
                    Codec::Page {
                        codec: format!("mp4a.40.{profile}"),
                        desc: asc,
                        subtype: AAC,
                    }
                }
                0x69 | 0x6b => Codec::Page {
                    codec: "mp3".into(),
                    desc: Vec::new(),
                    subtype: MP3,
                },
                _ => return None,
            }
        }
        b".mp3" => Codec::Page {
            codec: "mp3".into(),
            desc: Vec::new(),
            subtype: MP3,
        },
        _ => return None,
    };
    if channels == 0 || rate == 0 {
        return None;
    }
    // the sample table: sizes (stsz), chunk offsets (stco / co64), samples per chunk (stsc),
    // durations (stts, in the track's timescale)
    let (zs, _) = child(b, ss, se, b"stsz")?;
    let (fixed, count) = (u32be(b, zs + 4)?, u32be(b, zs + 8)? as usize);
    if count > b.len() {
        return None;
    }
    let sizes: Vec<u32> = if fixed != 0 {
        vec![fixed; count]
    } else {
        (0..count).map(|i| u32be(b, zs + 12 + 4 * i)).collect::<Option<_>>()?
    };
    let chunks: Vec<u64> = if let Some((cs, _)) = child(b, ss, se, b"stco") {
        let n = u32be(b, cs + 4)? as usize;
        (0..n.min(b.len())).map(|i| u32be(b, cs + 8 + 4 * i).map(|v| v as u64)).collect::<Option<_>>()?
    } else {
        let (cs, _) = child(b, ss, se, b"co64")?;
        let n = u32be(b, cs + 4)? as usize;
        (0..n.min(b.len())).map(|i| u64be(b, cs + 8 + 8 * i)).collect::<Option<_>>()?
    };
    let (scs, _) = child(b, ss, se, b"stsc")?;
    let runs: Vec<(u32, u32)> = (0..(u32be(b, scs + 4)? as usize).min(b.len()))
        .map(|i| Some((u32be(b, scs + 8 + 12 * i)?, u32be(b, scs + 12 + 12 * i)?)))
        .collect::<Option<_>>()?;
    let mut packets = Vec::with_capacity(count);
    let mut k = 0usize;
    for (ci, &off) in chunks.iter().enumerate() {
        let per = runs.iter().rev().find(|r| r.0 as usize <= ci + 1).map_or(0, |r| r.1);
        let mut o = off;
        for _ in 0..per {
            let Some(&s) = sizes.get(k) else { break };
            packets.push((o, s));
            o = o.saturating_add(s as u64);
            k += 1;
        }
    }
    if packets.len() != count || packets.iter().any(|&(o, s)| o.checked_add(s as u64).is_none_or(|e| e > file_len)) {
        return None;
    }
    let (ts, _) = child(b, ss, se, b"stts")?;
    let mut frames = Vec::with_capacity(count);
    for i in 0..(u32be(b, ts + 4)? as usize).min(count) {
        let (n, d) = (u32be(b, ts + 8 + 8 * i)?, u32be(b, ts + 12 + 8 * i)?);
        // (durations in the track's timescale; frames at the sound's rate)
        let f = (d as u64 * rate as u64 / timescale.max(1) as u64) as u32;
        frames.extend(std::iter::repeat_n(f, (n as usize).min(count - frames.len())));
    }
    frames.resize(count, *frames.last().unwrap_or(&1024));
    let secs = frames.iter().map(|&f| f as f64).sum::<f64>() / rate as f64;
    let bytes: u64 = sizes.iter().map(|&s| s as u64).sum();
    let avg = if secs > 0.0 { (bytes as f64 / secs) as u32 } else { 0 };
    Some(Track {
        codec,
        rate,
        channels,
        packets,
        frames,
        avg,
        duration: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn wav(tag: u16, ch: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let align = ch * bits / 8;
        let mut f = b"RIFF\0\0\0\0WAVE".to_vec();
        f.extend(b"fmt \x10\0\0\0");
        for v in [tag, ch] {
            f.extend(v.to_le_bytes());
        }
        f.extend(rate.to_le_bytes());
        f.extend((rate * align as u32).to_le_bytes());
        f.extend(align.to_le_bytes());
        f.extend(bits.to_le_bytes());
        // a chunk games' tools add, with an odd length (padded)
        f.extend(b"LIST\x03\0\0\0abc\0");
        f.extend(b"data");
        f.extend((data.len() as u32).to_le_bytes());
        f.extend(data);
        f
    }

    #[test]
    fn wave_pcm_packets() {
        let data: Vec<u8> = (0..4 * 5000).map(|i| i as u8).collect();
        let t = open(&wav(1, 2, 44100, 16, &data)).unwrap();
        assert_eq!((t.codec.clone(), t.rate, t.channels, t.avg), (Codec::Pcm { bits: 16, float: false }, 44100, 2, 176400));
        assert_eq!(t.frames, [4410, 590]);
        let body = wav(1, 2, 44100, 16, &[]).len() as u64;
        assert_eq!(t.packets, [(body, 4410 * 4), (body + 4410 * 4, 590 * 4)]);
        // (a tenth of a second at any rate)
        assert_eq!(open(&wav(1, 1, 22050, 16, &vec![0; 2 * 6615])).unwrap().frames, [2205, 2205, 2205]);
        assert_eq!(open(&wav(3, 1, 48000, 32, &[0; 8])).unwrap().codec, Codec::Pcm { bits: 32, float: true });
        // ADPCM, or no fmt chunk: not opened here
        assert_eq!(open(&wav(2, 1, 22050, 4, &[0; 8])), None);
        assert_eq!(open(b"RIFF\0\0\0\0WAVEdata\x04\0\0\0abcd"), None);
    }

    /// MPEG-1 layer III 44.1 kHz 128 kbps stereo frames, padded or not
    fn frame(pad: bool, fill: u8) -> Vec<u8> {
        let mut f = vec![0xff, 0xfb, 0x90 | (pad as u8) << 1, 0];
        f.resize(417 + pad as usize, fill & 0x7f);
        f
    }

    #[test]
    fn mp3_files() {
        let mut f = b"ID3\x03\0\0\0\0\0\x05hello".to_vec();
        let start = f.len() as u64;
        for i in 0..10 {
            f.extend(frame(i % 3 == 0, i));
        }
        f.extend(b"TAG");
        f.extend([0u8; 125]);
        let t = open(&f).unwrap();
        assert_eq!((t.rate, t.channels, t.packets.len(), t.total_frames()), (44100, 2, 10, 11520));
        assert_eq!(t.packets[0], (start, 418));
        assert_eq!(t.packets[1], (start + 418, 417));
        assert!(matches!(t.codec, Codec::Page { ref codec, subtype: MP3, .. } if codec == "mp3"));
        // MP3 in a WAVE
        let w = wav(0x55, 2, 44100, 0, &f[start as usize..start as usize + 417 * 2 + 1]);
        let t = open(&w).unwrap();
        assert_eq!(t.packets.len(), 2);
        // junk is not an MP3
        assert_eq!(open(&[0x11; 10000]), None);
    }

    /// An MP4 box
    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend(kind);
        b.extend(body);
        b
    }
    fn full(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 4];
        v.extend(body);
        bx(kind, &v)
    }
    fn be(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }
    fn desc(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut d = vec![tag, 0x80, 0x80, 0x80, body.len() as u8];
        d.extend(body);
        d
    }

    /// An M4A as ffmpeg lays it out: ftyp, mdat (the packets), moov; `sizes` packets in chunks of
    /// `per`, 1024 frames each, timescale `ts`, AAC-LC with config `asc`.
    fn m4a(sizes: &[u32], per: usize, ts: u32, asc: &[u8], co64: bool) -> (Vec<u8>, Vec<u64>) {
        let mut f = bx(b"ftyp", b"M4A \0\0\x02\0isomiso2");
        let mdat_at = f.len() + 8;
        let body: Vec<u8> = sizes.iter().enumerate().flat_map(|(i, &s)| vec![i as u8; s as usize]).collect();
        f.extend(bx(b"mdat", &body));
        let mut offs = Vec::new();
        let mut o = mdat_at as u64;
        for &s in sizes {
            offs.push(o);
            o += s as u64;
        }
        let chunk_offs: Vec<u64> = offs.chunks(per).map(|c| c[0]).collect();
        let mut entry = vec![0u8; 6];
        entry.extend([0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 16, 0, 0, 0, 0]);
        entry.extend((44100u32 << 16).to_be_bytes());
        let dci = [&[0x40u8, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..], &desc(5, asc)].concat();
        let es = [&[0u8, 1, 0][..], &desc(4, &dci), &desc(6, &[2])].concat();
        entry.extend(full(b"esds", &desc(3, &es)));
        let stsd = full(b"stsd", &[&be(&[1])[..], &bx(b"mp4a", &entry)].concat());
        let stts = full(b"stts", &be(&[1, sizes.len() as u32, 1024 * ts / 44100]));
        let stsc = full(b"stsc", &be(&[1, 1, per as u32, 1]));
        let stsz = full(b"stsz", &[be(&[0, sizes.len() as u32]), be(sizes)].concat());
        let stco = if co64 {
            full(b"co64", &[be(&[chunk_offs.len() as u32]), chunk_offs.iter().flat_map(|v| v.to_be_bytes()).collect()].concat())
        } else {
            full(b"stco", &[be(&[chunk_offs.len() as u32]), be(&chunk_offs.iter().map(|&v| v as u32).collect::<Vec<_>>())].concat())
        };
        let stbl = bx(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
        let minf = bx(b"minf", &stbl);
        let hdlr = full(b"hdlr", &[&[0u8; 4][..], b"soun", &[0; 12], b"Sound\0"].concat());
        let mdhd = full(b"mdhd", &be(&[0, 0, ts, 0, 0]));
        let mdia = bx(b"mdia", &[mdhd, hdlr, minf].concat());
        // a video track first: skipped
        let vhdlr = full(b"hdlr", &[&[0u8; 4][..], b"vide", &[0; 12]].concat());
        let vtrak = bx(b"trak", &bx(b"mdia", &[full(b"mdhd", &be(&[0, 0, 600, 0, 0])), vhdlr].concat()));
        f.extend(bx(b"moov", &[full(b"mvhd", &[0; 96]), vtrak, bx(b"trak", &mdia)].concat()));
        (f, offs)
    }

    #[test]
    fn m4a_aac() {
        // AAC-LC (2), 44.1 kHz (index 4), stereo: 0x12 0x10
        let sizes = [300, 371, 5, 400, 280];
        let (f, offs) = m4a(&sizes, 2, 44100, &[0x12, 0x10], false);
        let t = open(&f).unwrap();
        assert_eq!(
            t.codec,
            Codec::Page {
                codec: "mp4a.40.2".into(),
                desc: vec![0x12, 0x10],
                subtype: AAC
            }
        );
        assert_eq!((t.rate, t.channels), (44100, 2));
        assert_eq!(t.packets, offs.iter().zip(sizes).map(|(&o, s)| (o, s)).collect::<Vec<_>>());
        assert_eq!(t.frames, [1024; 5]);
        // the config's rate wins over the entry's: HE-AAC's core at 22.05 kHz mono (aot 5)
        let (f, _) = m4a(&sizes, 3, 22050, &[0x2b, 0x88], true);
        let t = open(&f).unwrap();
        assert_eq!((t.rate, t.channels), (22050, 1));
        assert!(matches!(t.codec, Codec::Page { ref codec, .. } if codec == "mp4a.40.5"));
        // no movie duration (mvhd all zeros): none; one: it, as Windows' MP4 source has it
        assert_eq!(t.duration, None);
        let (mut f, _) = m4a(&sizes, 2, 44100, &[0x12, 0x10], false);
        let at = f.windows(4).position(|w| w == b"mvhd").unwrap() + 4;
        f[at + 12..at + 20].copy_from_slice(&[0, 0, 3, 0xe8, 0, 0, 1, 0xf4]);
        assert_eq!(open(&f).unwrap().duration, Some(5_000_000));
        // version 1: 64-bit times
        f[at] = 1;
        f[at + 20..at + 32].copy_from_slice(&[0, 0, 0x03, 0xe8, 0, 0, 0, 0, 0, 0, 0x01, 0xf4]);
        assert_eq!(open(&f).unwrap().duration, Some(5_000_000));
    }

    /// Review A-7: a streamed WAV (data length 0xFFFFFFFF, as ffmpeg writes for piped output)
    /// plays the bytes the file has; chunk lengths 0xFFFFFFF8.. (on wasm32 `body + len`
    /// wrapped to the same offset: an endless loop) end the walk; packets stay in the file.
    #[test]
    fn wave_huge_chunk_lengths() {
        let data: Vec<u8> = (0..4 * 3000).map(|i| i as u8).collect();
        let mut f = wav(1, 2, 44100, 16, &data);
        let n = f.len();
        f[n - data.len() - 4..n - data.len()].copy_from_slice(&u32::MAX.to_le_bytes());
        let t = open(&f).unwrap();
        assert_eq!(t.frames.iter().sum::<u32>(), 3000);
        assert!(t.packets.iter().all(|&(o, s)| o + s as u64 <= f.len() as u64));
        // an odd last byte past whole frames: not a frame
        f.push(7);
        assert_eq!(open(&f).unwrap().frames.iter().sum::<u32>(), 3000);
        for len in [0xffff_fff8u32, 0xffff_fff7, 0xffff_fffe, 0xffff_ffff, 0x8000_0000] {
            let mut g = b"RIFF\0\0\0\0WAVEjunk".to_vec();
            g.extend(len.to_le_bytes());
            g.extend(&wav(1, 2, 44100, 16, &data)[12..]);
            assert_eq!(open(&g), None, "{len:#x}");
        }
        // an MP3 data chunk of length 0xFFFFFFFF (tag 0x55): the frames up to the end
        let mut m = wav(0x55, 2, 44100, 0, &[]);
        let n = m.len();
        m[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        for i in 0..4 {
            m.extend(frame(i % 2 == 0, i as u8));
        }
        let t = open(&m).unwrap();
        assert_eq!(t.packets.len(), 4);
        assert!(t.packets.iter().all(|&(o, s)| o + s as u64 <= m.len() as u64));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// The chunk arithmetic over the whole wasm32 range (offsets and files up to 4 GB, any
        /// length): the end is inside [body, file] (or the file's end when the body is past it),
        /// the next chunk starts past the body: the walk always advances and never slices
        /// backwards.
        #[test]
        fn riff_chunk_in_32_bits(body in 8usize..=u32::MAX as usize, len: u32, file in 0usize..=u32::MAX as usize) {
            let (end, next) = riff_chunk(body, len, file);
            prop_assert!(end <= file.max(body.min(file)));
            prop_assert!(end >= body.min(file));
            prop_assert!(next > body || len == 0 && next == body);
            prop_assert_eq!(next as u64, body as u64 + len as u64 + (len & 1) as u64);
        }

        /// Box sizes near 2^32 and 2^64 (the 64-bit form too): the box walk ends, every box is
        /// inside what it was given.
        #[test]
        fn boxes_with_any_sizes(sizes in proptest::collection::vec(prop_oneof![Just(0u64), Just(1), 8u64..64, Just(0xffff_fff8), Just(0xffff_ffff), Just(1 << 32), Just(u64::MAX), Just(u64::MAX - 7)], 1..8)) {
            let mut b = Vec::new();
            for &s in &sizes {
                if s > u32::MAX as u64 {
                    b.extend(1u32.to_be_bytes());
                    b.extend(b"free");
                    b.extend(s.to_be_bytes());
                } else {
                    b.extend((s as u32).to_be_bytes());
                    b.extend(b"free");
                    b.resize(b.len() + (s as usize).saturating_sub(8).min(56), 0);
                }
            }
            for (_, body, end) in boxes(&b, 0, b.len()) {
                prop_assert!(body <= end && end <= b.len());
            }
        }

        /// Any packet sizes in any chunking, 32- or 64-bit offsets: every packet found where it is.
        #[test]
        fn m4a_tables(sizes in proptest::collection::vec(1u32..600, 1..60), per in 1usize..9, co64 in any::<bool>()) {
            let (f, offs) = m4a(&sizes, per, 48000, &[0x12, 0x10], co64);
            let t = open(&f).unwrap();
            prop_assert_eq!(t.packets.len(), sizes.len());
            for (i, (&(o, s), &want)) in t.packets.iter().zip(&sizes).enumerate() {
                prop_assert_eq!((o, s), (offs[i], want));
                prop_assert!(f[o as usize..(o + s as u64) as usize].iter().all(|&x| x == i as u8));
            }
        }

        /// Any bytes (as any of the three kinds): no panic; packets inside the file.
        #[test]
        fn any_bytes(mut b in proptest::collection::vec(any::<u8>(), 0..2000), kind in 0u8..3) {
            let head: &[u8] = match kind {
                0 => b"RIFF\0\0\0\0WAVE",
                1 => b"\0\0\0\x10ftypM4A \0\0\0\0",
                _ => b"",
            };
            b.splice(0..0, head.iter().copied());
            if let Some(t) = open(&b) {
                prop_assert_eq!(t.packets.len(), t.frames.len());
                for &(o, s) in &t.packets {
                    prop_assert!(o + s as u64 <= b.len() as u64);
                }
            }
        }

        /// A cut-off M4A is refused or gives packets inside what is left.
        #[test]
        fn m4a_cut(sizes in proptest::collection::vec(1u32..300, 1..20), cut in 0usize..10000) {
            let (f, _) = m4a(&sizes, 4, 44100, &[0x12, 0x10], false);
            let f = &f[..cut.min(f.len())];
            if let Some(t) = open(f) {
                for &(o, s) in &t.packets {
                    prop_assert!(o + s as u64 <= f.len() as u64);
                }
            }
        }
    }
}
