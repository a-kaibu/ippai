//! Summary information: the "\x05SummaryInformation" property set (FMTID
//! F29F85E0-4FF9-1068-AB91-08002B27B3D9), properties 1 (codepage) to 19.

use crate::*;

pub const VT_EMPTY: u32 = 0;
pub const VT_I2: u32 = 2;
pub const VT_I4: u32 = 3;
pub const VT_LPSTR: u32 = 30;
pub const VT_FILETIME: u32 = 64;

pub const PID_CODEPAGE: u32 = 1;
pub const PID_TITLE: u32 = 2;
pub const PID_SUBJECT: u32 = 3;
pub const PID_AUTHOR: u32 = 4;
pub const PID_KEYWORDS: u32 = 5;
pub const PID_COMMENTS: u32 = 6;
pub const PID_TEMPLATE: u32 = 7;
pub const PID_LASTAUTHOR: u32 = 8;
pub const PID_REVNUMBER: u32 = 9;
pub const PID_EDITTIME: u32 = 10;
pub const PID_LASTPRINTED: u32 = 11;
pub const PID_CREATE_DTM: u32 = 12;
pub const PID_LASTSAVE_DTM: u32 = 13;
pub const PID_PAGECOUNT: u32 = 14;
pub const PID_WORDCOUNT: u32 = 15;
pub const PID_CHARCOUNT: u32 = 16;
pub const PID_THUMBNAIL: u32 = 17;
pub const PID_APPNAME: u32 = 18;
pub const PID_SECURITY: u32 = 19;
pub const MAX_PID: u32 = 19;

const FMTID: [u8; 16] = [0xe0, 0x85, 0x9f, 0xf2, 0xf9, 0x4f, 0x68, 0x10, 0xab, 0x91, 0x08, 0x00, 0x2b, 0x27, 0xb3, 0xd9];

#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    I2(i16),
    I4(i32),
    Str(String),
    Time(u64),
}

impl Prop {
    pub fn vt(&self) -> u32 {
        match self {
            Prop::I2(_) => VT_I2,
            Prop::I4(_) => VT_I4,
            Prop::Str(_) => VT_LPSTR,
            Prop::Time(_) => VT_FILETIME,
        }
    }
}

/// The type a property must have (MsiSummaryInfoSetProperty checks it).
pub fn type_of(pid: u32) -> Option<u32> {
    Some(match pid {
        PID_CODEPAGE => VT_I2,
        PID_TITLE..=PID_REVNUMBER | PID_APPNAME => VT_LPSTR,
        PID_EDITTIME..=PID_LASTSAVE_DTM => VT_FILETIME,
        PID_PAGECOUNT | PID_WORDCOUNT | PID_CHARCOUNT | PID_SECURITY => VT_I4,
        _ => return None,
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SummaryInfo {
    pub props: std::collections::BTreeMap<u32, Prop>,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

impl SummaryInfo {
    pub fn codepage(&self) -> u32 {
        match self.props.get(&PID_CODEPAGE) {
            Some(Prop::I2(c)) => *c as u16 as u32,
            _ => 0,
        }
    }

    pub fn parse(b: &[u8]) -> SummaryInfo {
        let mut s = SummaryInfo::default();
        let _ = s.parse_into(b);
        s
    }

    fn parse_into(&mut self, b: &[u8]) -> Option<()> {
        if u16_at(b, 0)? != 0xfffe {
            return None;
        }
        let nsec = u32_at(b, 24)?;
        if nsec < 1 {
            return None;
        }
        let off = u32_at(b, 44)? as usize;
        let n = u32_at(b, off + 4)?;
        // the codepage first: strings are in it
        let mut entries = Vec::new();
        for i in 0..n.min(1000) as usize {
            let pid = u32_at(b, off + 8 + 8 * i)?;
            let po = off + u32_at(b, off + 12 + 8 * i)? as usize;
            entries.push((pid, po));
        }
        let mut cp = 0;
        for &(pid, po) in &entries {
            if pid == PID_CODEPAGE && u32_at(b, po)? == VT_I2 {
                cp = u16_at(b, po + 4)? as u32;
            }
        }
        for (pid, po) in entries {
            let vt = u32_at(b, po)?;
            let p = match vt {
                VT_I2 => Prop::I2(u16_at(b, po + 4)? as i16),
                VT_I4 => Prop::I4(u32_at(b, po + 4)? as i32),
                VT_LPSTR => {
                    let len = u32_at(b, po + 4)? as usize;
                    let raw = b.get(po + 8..po + 8 + len)?;
                    let raw = &raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())];
                    Prop::Str(crate::pool::decode_cp(if cp == 0 || cp == 1200 { 0 } else { cp }, raw))
                }
                VT_FILETIME => Prop::Time(u32_at(b, po + 4)? as u64 | (u32_at(b, po + 8)? as u64) << 32),
                _ => continue,
            };
            if pid <= MAX_PID {
                self.props.insert(pid, p);
            }
        }
        Some(())
    }

    pub fn serialize(&self) -> Vec<u8> {
        let cp = self.codepage();
        let mut out = Vec::new();
        out.extend(0xfffeu16.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(0x0002_0005u32.to_le_bytes()); // (Windows NT 5)
        out.extend([0u8; 16]);
        out.extend(1u32.to_le_bytes());
        out.extend(FMTID);
        out.extend(48u32.to_le_bytes());
        let mut vals = Vec::new();
        let mut idx = Vec::new();
        let hdr = 8 + 8 * self.props.len();
        for (&pid, p) in &self.props {
            idx.push((pid, (hdr + vals.len()) as u32));
            vals.extend(p.vt().to_le_bytes());
            match p {
                Prop::I2(v) => vals.extend((*v as i32).to_le_bytes()),
                Prop::I4(v) => vals.extend(v.to_le_bytes()),
                Prop::Str(s) => {
                    let mut b = crate::pool::encode_cp(cp, s);
                    b.push(0);
                    vals.extend((b.len() as u32).to_le_bytes());
                    vals.extend(&b);
                    while vals.len() % 4 != 0 {
                        vals.push(0);
                    }
                }
                Prop::Time(t) => vals.extend(t.to_le_bytes()),
            }
        }
        out.extend(((hdr + vals.len()) as u32).to_le_bytes());
        out.extend((self.props.len() as u32).to_le_bytes());
        for (pid, o) in idx {
            out.extend(pid.to_le_bytes());
            out.extend(o.to_le_bytes());
        }
        out.extend(vals);
        out
    }

    /// MsiSummaryInfoSetProperty's checks: the type must be the property's.
    pub fn set(&mut self, pid: u32, p: Prop) -> R<()> {
        let want = type_of(pid).ok_or(ERROR_UNKNOWN_PROPERTY)?;
        if p.vt() != want {
            return Err(ERROR_DATATYPE_MISMATCH);
        }
        self.props.insert(pid, p);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut s = SummaryInfo::default();
        s.set(PID_CODEPAGE, Prop::I2(932)).unwrap();
        s.set(PID_TITLE, Prop::Str("インストール".into())).unwrap();
        s.set(PID_TEMPLATE, Prop::Str("Intel;1041".into())).unwrap();
        s.set(PID_WORDCOUNT, Prop::I4(2)).unwrap();
        s.set(PID_CREATE_DTM, Prop::Time(0x01d0_0000_1234_5678)).unwrap();
        assert_eq!(s.set(PID_TITLE, Prop::I4(1)), Err(ERROR_DATATYPE_MISMATCH));
        assert_eq!(s.set(99, Prop::I4(1)), Err(ERROR_UNKNOWN_PROPERTY));
        let b = s.serialize();
        assert_eq!(SummaryInfo::parse(&b), s);
    }
}
