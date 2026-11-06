//! Ogg pages to packets (one logical stream; CRCs are not checked).

/// The packets of the first logical stream in an Ogg file, and the stream's last granule
/// position.
pub fn packets(data: &[u8]) -> (Vec<Vec<u8>>, u64) {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut pos = 0;
    let mut serial = None;
    let mut granule = 0;
    while pos + 27 <= data.len() {
        if &data[pos..pos + 4] != b"OggS" {
            pos += 1;
            continue;
        }
        let nseg = data[pos + 26] as usize;
        if pos + 27 + nseg > data.len() {
            break;
        }
        let s = u32::from_le_bytes(data[pos + 14..pos + 18].try_into().unwrap());
        let g = u64::from_le_bytes(data[pos + 6..pos + 14].try_into().unwrap());
        let lacing = &data[pos + 27..pos + 27 + nseg];
        let mut body = pos + 27 + nseg;
        let mine = *serial.get_or_insert(s) == s;
        for &l in lacing {
            let end = (body + l as usize).min(data.len());
            if mine {
                cur.extend_from_slice(&data[body..end]);
                if l < 255 {
                    out.push(std::mem::take(&mut cur));
                }
            }
            body = end;
        }
        if mine && g != u64::MAX {
            granule = g;
        }
        pos = body;
    }
    (out, granule)
}
