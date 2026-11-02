//! The DXBC container: its chunks and the input/output signatures. Shared by the shader front
//! ends: tpf.rs reads SHDR/SHEX (shader model 4/5); a DXIL front end (shader model 6, the same
//! container with a DXIL chunk and ISG1/OSG1 signatures) would read the rest of it here too.

/// A signature element (ISGN/OSGN, ISG1/OSG1, OSG5).
#[derive(Clone, Debug, PartialEq)]
pub struct SigElement {
    pub name: String,
    pub index: u32,
    /// D3D_NAME: 0 none, 1 position, 6 vertex id, 8 instance id, 9 is front face, 10 sample index, 64+ targets...
    pub sysval: u32,
    /// 1 uint, 2 sint, 3 float
    pub component_type: u32,
    pub reg: u32,
    pub mask: u32,
    pub stream: u32,
}

impl SigElement {
    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct Container<'a> {
    chunks: Vec<([u8; 4], &'a [u8])>,
}

fn rd(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl<'a> Container<'a> {
    pub fn parse(code: &'a [u8]) -> Result<Self, String> {
        if code.get(..4) != Some(b"DXBC") {
            return Err("not DXBC".into());
        }
        let n = rd(code, 28).ok_or("short DXBC")? as usize;
        let mut chunks = Vec::new();
        for i in 0..n.min(64) {
            let off = rd(code, 32 + 4 * i).ok_or("short DXBC")? as usize;
            let tag = code.get(off..off + 4).ok_or("bad DXBC chunk")?;
            let size = rd(code, off + 4).ok_or("bad DXBC chunk")? as usize;
            let data = code.get(off + 8..off + 8 + size).ok_or("bad DXBC chunk size")?;
            chunks.push(([tag[0], tag[1], tag[2], tag[3]], data));
        }
        Ok(Container { chunks })
    }

    pub fn chunk(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        self.chunks.iter().find(|c| &c.0 == tag).map(|c| c.1)
    }

    /// The input (`output` false) or output signature.
    pub fn signature(&self, output: bool) -> Vec<SigElement> {
        let tags: &[(&[u8; 4], usize)] =
            if output { &[(b"OSGN", 24), (b"OSG1", 32), (b"OSG5", 28)] } else { &[(b"ISGN", 24), (b"ISG1", 32)] };
        for &(tag, size) in tags {
            if let Some(c) = self.chunk(tag) {
                return parse_signature(c, size);
            }
        }
        Vec::new()
    }
}

/// Elements of `size` bytes: ISGN/OSGN 24 (name, index, sysval, type, reg, mask), OSG5 28 (a
/// stream first), ISG1/OSG1 32 (stream first, min precision last).
fn parse_signature(c: &[u8], size: usize) -> Vec<SigElement> {
    let n = rd(c, 0).unwrap_or(0) as usize;
    let mut v = Vec::new();
    for i in 0..n.min(128) {
        let at = 8 + i * size;
        let base = if size == 24 { at } else { at + 4 };
        let (Some(name_off), Some(index), Some(sysval), Some(ty), Some(reg), Some(mask)) =
            (rd(c, base), rd(c, base + 4), rd(c, base + 8), rd(c, base + 12), rd(c, base + 16), c.get(base + 20))
        else {
            break;
        };
        let name_off = name_off as usize;
        let name = c.get(name_off..).map_or(String::new(), |s| {
            let e = s.iter().position(|&b| b == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..e]).into_owned()
        });
        let stream = if size == 24 { 0 } else { rd(c, at).unwrap_or(0) };
        v.push(SigElement { name, index, sysval, component_type: ty, reg, mask: *mask as u32 & 0xf, stream });
    }
    v
}
