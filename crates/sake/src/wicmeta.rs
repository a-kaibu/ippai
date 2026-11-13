//! What a WIC frame's metadata query reader (IWICBitmapFrameDecode::GetMetadataQueryReader)
//! answers, worked out from the file's bytes: for PNG the chunks Windows' PNG decoder exposes by
//! query name (`/sRGB/RenderingIntent`, `/gAMA/ImageGamma`, `/tEXt/{str=Keyword}`). DirectXTex's
//! loader (linked into games) asks for the reader and fails the whole load unless it gets one
//! or WINCODEC_ERR_UNSUPPORTEDOPERATION (BMP's answer), then looks for the sRGB chunk.
//! Pure: no guest memory, so tests, proptest and the fuzzer run it natively.

/// A metadata value as WIC hands it out in a PROPVARIANT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// VT_UI1
    U1(u8),
    /// VT_UI4
    U4(u32),
    /// VT_LPSTR (the bytes, no terminator)
    Str(Vec<u8>),
}

impl Value {
    pub fn vt(&self) -> u16 {
        match self {
            Value::U1(_) => 17,
            Value::U4(_) => 19,
            Value::Str(_) => 30,
        }
    }
}

/// One frame's metadata: the top-level blocks in file order (what the reader's enumerator
/// lists, e.g. "/sRGB") and the leaf items by query name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub blocks: Vec<String>,
    pub items: Vec<(String, Value)>,
}

impl Meta {
    /// GetMetadataByName: query names compare without case (WIC's names are case-insensitive);
    /// the first of repeated items wins.
    pub fn get(&self, name: &str) -> Option<&Value> {
        let k = canon(name)?;
        self.items.iter().find(|(n, _)| canon(n).as_deref() == Some(k.as_str())).map(|(_, v)| v)
    }
}

/// A query name in one spelling: lower case, a block's "/" separators kept. Not a query
/// (empty, no leading "/"): None.
fn canon(name: &str) -> Option<String> {
    if !name.starts_with('/') || name.len() < 2 {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// A PNG's frame metadata (None: not a PNG). Chunks are read up to IEND or the end of the data;
/// a chunk running past the end stops the walk (what was read stays), as do bad lengths.
pub fn png(data: &[u8]) -> Option<Meta> {
    if data.len() < 8 || &data[..8] != SIG {
        return None;
    }
    let mut m = Meta::default();
    let mut p = 8usize;
    while p + 8 <= data.len() {
        let len = u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        let ty = &data[p + 4..p + 8];
        // (a length over 2^31 - 1 is invalid PNG)
        if len > 0x7fff_ffff {
            break;
        }
        let Some(end) = (p + 8).checked_add(len) else { break };
        if end > data.len() {
            break;
        }
        let body = &data[p + 8..end];
        let mut block = |name: &str| {
            if !m.blocks.iter().any(|b| b == name) {
                m.blocks.push(name.to_string());
            }
        };
        match ty {
            b"sRGB" if len == 1 => {
                block("/sRGB");
                m.items.push(("/sRGB/RenderingIntent".into(), Value::U1(body[0])));
            }
            b"gAMA" if len == 4 => {
                block("/gAMA");
                m.items.push(("/gAMA/ImageGamma".into(), Value::U4(u32::from_be_bytes(body.try_into().unwrap()))));
            }
            b"tEXt" => {
                // keyword (1-79 Latin-1 bytes), NUL, text
                if let Some(z) = body.iter().position(|&b| b == 0) {
                    if (1..=79).contains(&z) {
                        block("/tEXt");
                        let key = String::from_utf8_lossy(&body[..z]);
                        m.items.push((format!("/tEXt/{{str={key}}}"), Value::Str(body[z + 1..].to_vec())));
                    }
                }
            }
            b"IEND" => break,
            _ => {}
        }
        // (CRC not checked: Windows' decoder doesn't refuse metadata over it either)
        p = end + 4;
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn chunk(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(body);
        v.extend_from_slice(&[0, 0, 0, 0]);
        v
    }

    fn file(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = SIG.to_vec();
        let ihdr = [0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0];
        v.extend(chunk(b"IHDR", &ihdr));
        for c in chunks {
            v.extend_from_slice(c);
        }
        v.extend(chunk(b"IEND", &[]));
        v
    }

    #[test]
    fn srgb_gamma_and_text() {
        let m = png(&file(&[chunk(b"sRGB", &[2]), chunk(b"gAMA", &45455u32.to_be_bytes()), chunk(b"tEXt", b"Software\0paint")])).unwrap();
        assert_eq!(m.get("/sRGB/RenderingIntent"), Some(&Value::U1(2)));
        assert_eq!(m.get("/srgb/renderingintent"), Some(&Value::U1(2)));
        assert_eq!(m.get("/gAMA/ImageGamma"), Some(&Value::U4(45455)));
        assert_eq!(m.get("/tEXt/{str=Software}"), Some(&Value::Str(b"paint".to_vec())));
        assert_eq!(m.get("/tEXt/{str=Title}"), None);
        assert_eq!(m.get("System.Image.ColorSpace"), None);
        assert_eq!(m.get(""), None);
        assert_eq!(m.blocks, ["/sRGB", "/gAMA", "/tEXt"]);
    }

    #[test]
    fn plain_png_has_no_items_and_others_are_not_png() {
        let m = png(&file(&[])).unwrap();
        assert!(m.items.is_empty() && m.blocks.is_empty());
        assert_eq!(png(b"BM\0\0\0\0"), None);
        assert_eq!(png(&[]), None);
    }

    #[test]
    fn wrong_sizes_and_after_iend_are_ignored() {
        let mut f = file(&[chunk(b"sRGB", &[0, 1]), chunk(b"gAMA", &[1, 2, 3]), chunk(b"tEXt", b"\0x"), chunk(b"tEXt", b"nonul")]);
        f.extend(chunk(b"sRGB", &[1]));
        let m = png(&f).unwrap();
        assert!(m.items.is_empty(), "{m:?}");
    }

    #[test]
    fn truncated_chunk_keeps_what_came_before() {
        let mut f = file(&[chunk(b"sRGB", &[3])]);
        f.truncate(f.len() - 12); // IEND cut
        f.extend_from_slice(&[0, 0, 1, 0, b'g', b'A', b'M', b'A', 1]);
        assert_eq!(png(&f).unwrap().get("/sRGB/RenderingIntent"), Some(&Value::U1(3)));
    }

    proptest! {
        /// Any bytes after the signature: no panic, every item belongs to a listed block.
        #[test]
        fn any_bytes(tail in proptest::collection::vec(any::<u8>(), 0..512)) {
            let mut f = SIG.to_vec();
            f.extend(tail);
            let m = png(&f).unwrap();
            for (n, _) in &m.items {
                let ok = m.blocks.iter().any(|b| n.starts_with(&format!("{b}/")));
                prop_assert!(ok, "{} outside {:?}", n, m.blocks);
            }
        }

        /// Generated chunk lists: the reader answers exactly the first sRGB / gAMA / per-keyword
        /// tEXt values written, in any case spelling of the name.
        #[test]
        fn model(items in proptest::collection::vec((0u8..4, any::<u32>(), "[A-Za-z]{1,12}", proptest::collection::vec(1u8..=255, 0..20)), 0..12),
                 upper in any::<bool>()) {
            let mut chunks = Vec::new();
            let (mut srgb, mut gama) = (None, None);
            let mut texts: Vec<(String, Vec<u8>)> = Vec::new();
            for (kind, n, key, text) in &items {
                match kind {
                    0 => { chunks.push(chunk(b"sRGB", &[*n as u8])); srgb.get_or_insert(*n as u8); }
                    1 => { chunks.push(chunk(b"gAMA", &n.to_be_bytes())); gama.get_or_insert(*n); }
                    2 => {
                        let mut b = key.as_bytes().to_vec(); b.push(0); b.extend(text);
                        chunks.push(chunk(b"tEXt", &b));
                        if !texts.iter().any(|(k, _)| k.eq_ignore_ascii_case(key)) { texts.push((key.clone(), text.clone())); }
                    }
                    _ => chunks.push(chunk(b"zTXt", &n.to_be_bytes())),
                }
            }
            let m = png(&file(&chunks)).unwrap();
            let q = |s: &str| if upper { s.to_ascii_uppercase() } else { s.to_string() };
            prop_assert_eq!(m.get(&q("/sRGB/RenderingIntent")).cloned(), srgb.map(Value::U1));
            prop_assert_eq!(m.get(&q("/gAMA/ImageGamma")).cloned(), gama.map(Value::U4));
            for (k, t) in &texts {
                prop_assert_eq!(m.get(&q(&format!("/tEXt/{{str={k}}}"))).cloned(), Some(Value::Str(t.clone())));
            }
        }
    }
}
