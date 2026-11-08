//! Decode the supported subset of MIDL's published -Oif procedure/type metadata.
//! Used by the local COM transport, with explicit rejection of unsupported types.
//! No guest addresses, runtime-specific interfaces or authorization decisions.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Scalar(u32), Fixed(u32), User { index: u16, size: u16 },
    Array { element: u16, count_stack: u16 },
}
#[derive(Clone, Debug)]
pub struct Param { pub flags: u16, pub stack: u16, pub kind: Kind, pub reference: bool, pub nullable: bool }
impl Param {
    pub fn input(&self) -> bool { self.flags & 8 != 0 }
    pub fn output(&self) -> bool { self.flags & 0x10 != 0 }
    pub fn retval(&self) -> bool { self.flags & 0x20 != 0 }
}
#[derive(Clone, Debug)]
pub struct Method { pub number: u16, pub stack: u16, pub params: Vec<Param> }
fn word(b: &[u8], at: usize) -> Option<u16> { Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?)) }
fn scalar(fc: u8) -> Option<u32> {
    Some(match fc { 1..=4 => 1, 5..=7 => 2, 8 | 9 | 0x0e | 0x10 => 4, 0x0b => 8, _ => return None })
}
fn kind(types: &[u8], at: usize, depth: usize) -> Option<(Kind, bool)> {
    if depth > 8 { return None; }
    let fc = *types.get(at)?;
    Some(match fc {
        0x11..=0x14 => {
            let flags = *types.get(at + 1)?;
            if flags & 0x10 != 0 { return None; } // pointer-to-pointer needs a distinct representation
            let (k, _) = if flags & 8 != 0 { (Kind::Scalar(scalar(*types.get(at + 2)?)?), false) }
                else { kind(types, (at + 2).checked_add_signed(word(types, at + 2)? as i16 as isize)?, depth + 1)? };
            (k, fc != 0x11)
        }
        0x15 | 0x1d | 0x1e => { // pointer-free fixed structure / small fixed array
            let size = word(types, at + 2)? as u32;
            if size == 0 || size > 16384 { return None; }
            (Kind::Fixed(size), false)
        }
        0xb4 => (Kind::User { index: word(types, at + 2)?, size: word(types, at + 4)? }, false),
        0x1b => {
            // A top-level parameter supplies the element count directly. Other
            // correlation expressions must be implemented rather than guessed.
            let corr = *types.get(at + 4)?;
            if !matches!(corr, 0x28 | 0x29) || *types.get(at + 5)? != 0 { return None; }
            (Kind::Array { element: word(types, at + 2)?, count_stack: word(types, at + 6)? }, false)
        }
        _ => (Kind::Scalar(scalar(fc)?), false),
    })
}
pub fn parse(proc: &[u8], types: &[u8], pointer_size: u16) -> Option<Method> {
    if !matches!(pointer_size, 4 | 8) || *proc.first()? != 0x33 || *proc.get(1)? & 8 == 0 { return None; }
    let number = word(proc, 6)?;
    let stack = word(proc, 8)?;
    if stack < pointer_size * 2 || stack % pointer_size != 0 || stack > 4096 { return None; }
    let count = *proc.get(15)? as usize;
    if count == 0 || count > 128 { return None; }
    let mut at = 16;
    if proc[14] & 0x40 != 0 {
        let extension = *proc.get(at)? as usize;
        if extension < 2 { return None; }
        at += extension;
    }
    let mut params = Vec::new();
    for _ in 0..count {
        proc.get(at..at.checked_add(6)?)?;
        let flags = word(proc, at)?;
        let offset = word(proc, at + 2)?;
        if offset < pointer_size || offset >= stack || offset % pointer_size != 0 { return None; }
        let (kind, nullable) = if flags & 0x40 != 0 { (Kind::Scalar(scalar(*proc.get(at + 4)?)?), false) }
            else { kind(types, word(proc, at + 4)? as usize, 0)? };
        let reference = flags & 0x100 != 0 || flags & (0x40 | 0x80) == 0;
        if flags & 0x20 != 0 && (kind != Kind::Scalar(4) || reference) { return None; }
        params.push(Param { flags, stack: offset, kind, reference, nullable });
        at += 6;
    }
    if params.iter().filter(|p| p.retval()).count() != 1 { return None; }
    Some(Method { number, stack, params })
}

#[cfg(test)]
mod tests {
    use super::*;
    const PROC: &[u8] = &[0x33,0x6c,0,0,0,0,3,0,16,0,8,0,36,0,0x44,3,8,1,0,0,0,0,0,0,
        0x48,0,4,0,8,0, 0x50,0x21,8,0,8,0, 0x70,0,12,0,8,0];
    #[test]
    fn scalar_in_and_reference_out() {
        let m = parse(PROC, &[], 4).unwrap();
        assert_eq!(m.number, 3); assert_eq!(m.stack, 16);
        assert!(m.params[0].input()); assert!(!m.params[0].reference);
        assert!(m.params[1].output()); assert!(m.params[1].reference);
        assert!(m.params[2].retval());
    }
    #[test]
    fn truncated_or_unknown_metadata_is_rejected() {
        for n in 0..PROC.len() { assert!(parse(&PROC[..n], &[], 4).is_none()); }
        let mut b = PROC.to_vec(); b[28] = 0xff; assert!(parse(&b, &[], 4).is_none());
        b = PROC.to_vec(); b[8] = 15; assert!(parse(&b, &[], 4).is_none());
        assert!(parse(PROC, &[], 8).is_none());
    }
    #[test]
    fn fixed_structures_users_and_correlated_arrays() {
        assert_eq!(kind(&[0x15,3,16,0], 0, 0), Some((Kind::Fixed(16), false)));
        assert_eq!(kind(&[0xb4,0x83,0,0,4,0], 0, 0), Some((Kind::User { index: 0, size: 4 }, false)));
        assert_eq!(kind(&[0x1b,0,1,0,0x29,0,8,0], 0, 0), Some((Kind::Array { element: 1, count_stack: 8 }, false)));
        assert!(kind(&[0x1b,0,1,0,0x29,1,8,0], 0, 0).is_none());
        assert!(kind(&[0x16,3,16,0], 0, 0).is_none()); // structure with pointers
    }
}
