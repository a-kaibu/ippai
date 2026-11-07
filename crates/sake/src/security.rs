//! advapi32's security side as an elevated administrator's process sees it, and the service
//! control manager of a machine where kernel drivers do not load.
//!
//! - SIDs (AllocateAndInitializeSid, EqualSid, GetLengthSid ...), ACLs with access-allowed and
//!   access-denied ACEs, absolute security descriptors, AccessCheck over them.
//! - The process token (OpenProcessToken; ImpersonateSelf gives the thread one): the user
//!   S-1-5-21-...-1001 in Everyone, Administrators, Users, Interactive and Authenticated Users,
//!   high integrity, elevated (TokenElevationTypeFull), the administrator's privileges
//!   (SeLoadDriverPrivilege, SeDebugPrivilege ...); AdjustTokenPrivileges enables them.
//!   Programs that check "am I an administrator" (CPU-Z's AccessCheck on a descriptor that only
//!   lets Administrators in) find they are.
//! - Services: the manager opens, a service can be created, queried and deleted, but starting
//!   one fails: a kernel driver with ERROR_DRIVER_BLOCKED (sake runs no kernel code), any other
//!   with ERROR_SERVICE_DISABLED. Hardware tools take this path when their driver will not load
//!   and carry on with what user mode can read.

#![allow(non_snake_case)]

use std::collections::HashSet;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, cstr, put32, putp, r16, r32, r8, w16, w32, w8, wstr};
use crate::obj::{self, Obj};
use crate::HashMap;

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_NO_TOKEN: u32 = 1008;
const ERROR_SERVICE_DISABLED: u32 = 1058;
const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1060;
const ERROR_SERVICE_NOT_ACTIVE: u32 = 1062;
const ERROR_SERVICE_EXISTS: u32 = 1073;
const ERROR_DRIVER_BLOCKED: u32 = 1275;
const ERROR_NO_SUCH_PRIVILEGE: u32 = 1313;
const ERROR_UNKNOWN_REVISION: u32 = 1305;
const ERROR_INVALID_ACL: u32 = 1336;
const ERROR_INVALID_SID: u32 = 1337;
const ERROR_INVALID_SECURITY_DESCR: u32 = 1338;
const ERROR_ALLOTTED_SPACE_EXCEEDED: u32 = 1344;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_NO_IMPERSONATION_TOKEN: u32 = 1309;

// ---------------------------------------------------------------------------------------------
// SIDs: revision 1, the sub-authority count, a 6-byte big-endian authority, the sub-authorities

/// (authority, sub-authorities)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sid(pub u64, pub Vec<u32>);

impl Sid {
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = vec![1, self.1.len() as u8];
        v.extend_from_slice(&self.0.to_be_bytes()[2..]);
        for s in &self.1 {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }
    /// The SID at `p` if it is a valid one (revision 1, at most 15 sub-authorities).
    pub fn read(p: u32) -> Option<Sid> {
        if p == 0 || r8(p) != 1 || r8(p + 1) > 15 {
            return None;
        }
        let n = r8(p + 1) as u32;
        let auth = (0..6).fold(0u64, |a, i| a << 8 | r8(p + 2 + i) as u64);
        Some(Sid(auth, (0..n).map(|i| r32(p + 8 + 4 * i)).collect()))
    }
    fn len(&self) -> u32 {
        8 + 4 * self.1.len() as u32
    }
}

/// "S-1-<authority>-<sub>...": the authority in decimal below 2^32, else 0x and 12 hex digits
pub fn sid_to_string(s: &Sid) -> String {
    let mut t = if s.0 < 1 << 32 { format!("S-1-{}", s.0) } else { format!("S-1-0x{:012X}", s.0) };
    for x in &s.1 {
        t += &format!("-{x}");
    }
    t
}
/// sid_to_string's text back (numbers in decimal or 0x hex, at most 15 sub-authorities), or a
/// two-letter SDDL alias (BA, BU, WD, SY, AU, IU)
pub fn parse_sid(t: &str) -> Option<Sid> {
    match t {
        "BA" => return Some(admins()),
        "BU" => return Some(Sid(5, vec![32, 545])),
        "WD" => return Some(everyone()),
        "SY" => return Some(Sid(5, vec![18])),
        "AU" => return Some(Sid(5, vec![11])),
        "IU" => return Some(Sid(5, vec![4])),
        _ => {}
    }
    let num = |p: &str| -> Option<u64> {
        if let Some(h) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
            (!h.is_empty() && h.len() <= 12 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| u64::from_str_radix(h, 16).ok())?
        } else {
            (!p.is_empty() && p.len() <= 20 && p.bytes().all(|b| b.is_ascii_digit())).then(|| p.parse().ok())?
        }
    };
    let mut parts = t.strip_prefix("S-").or_else(|| t.strip_prefix("s-"))?.split('-');
    if parts.next()? != "1" {
        return None;
    }
    let auth = num(parts.next()?).filter(|&a| a < 1 << 48)?;
    let subs: Vec<u32> = parts.map(|p| num(p).and_then(|v| u32::try_from(v).ok())).collect::<Option<_>>()?;
    (subs.len() <= 15).then_some(Sid(auth, subs))
}
fn ConvertSidToStringSidA(c: &mut Ctx) -> u32 {
    sid_string(c, false)
}
fn ConvertSidToStringSidW(c: &mut Ctx) -> u32 {
    sid_string(c, true)
}
fn sid_string(c: &mut Ctx, wide: bool) -> u32 {
    let Some(sid) = Sid::read(c.a(0)) else {
        last_error(ERROR_INVALID_SID);
        return 0;
    };
    let t = sid_to_string(&sid);
    let p = if wide {
        let v: Vec<u16> = t.encode_utf16().chain([0]).collect();
        let p = mem::heap_alloc(2 * v.len() as u32, false);
        for (i, u) in v.iter().enumerate() {
            w16(p + 2 * i as u32, *u);
        }
        p
    } else {
        let p = mem::heap_alloc(t.len() as u32 + 1, false);
        mem::write(p, t.as_bytes());
        w8(p + t.len() as u32, 0);
        p
    };
    putp(c.a(1), p);
    1
}
fn ConvertStringSidToSidA(c: &mut Ctx) -> u32 {
    string_sid(c, String::from_utf8_lossy(&cstr(c.a(0))).into_owned())
}
fn ConvertStringSidToSidW(c: &mut Ctx) -> u32 {
    string_sid(c, String::from_utf16_lossy(&wstr(c.a(0))))
}
fn string_sid(c: &mut Ctx, t: String) -> u32 {
    if c.a(0) == 0 || c.a(1) == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let Some(sid) = parse_sid(&t) else {
        last_error(ERROR_INVALID_SID);
        return 0;
    };
    putp(c.a(1), sid_alloc(&sid));
    1
}

fn everyone() -> Sid {
    Sid(1, vec![0])
}
fn admins() -> Sid {
    Sid(5, vec![32, 544])
}
fn the_user() -> Sid {
    Sid(5, vec![21, 3_623_811_015, 3_361_044_348, 30_300_820, 1001])
}
/// The token's groups: (SID, SE_GROUP_* attributes)
fn groups() -> Vec<(Sid, u32)> {
    const MANDATORY: u32 = 0x7; // SE_GROUP_MANDATORY | ENABLED_BY_DEFAULT | ENABLED
    vec![
        (everyone(), MANDATORY),
        (Sid(5, vec![32, 544]), MANDATORY | 0x8), // BUILTIN\Administrators, SE_GROUP_OWNER
        (Sid(5, vec![32, 545]), MANDATORY),       // BUILTIN\Users
        (Sid(5, vec![4]), MANDATORY),             // INTERACTIVE
        (Sid(5, vec![11]), MANDATORY),            // Authenticated Users
        (Sid(5, vec![15]), MANDATORY),            // This Organization
        (Sid(2, vec![0]), MANDATORY),             // LOCAL
        (Sid(16, vec![0x3000]), 0x60),            // Mandatory Label\High (SE_GROUP_INTEGRITY(_ENABLED))
    ]
}
fn member(sid: &Sid) -> bool {
    *sid == the_user() || groups().iter().any(|(g, a)| g == sid && a & 4 != 0)
}

fn sid_alloc(s: &Sid) -> u32 {
    let b = s.bytes();
    let p = mem::heap_alloc(b.len() as u32, false);
    mem::write(p, &b);
    p
}

fn AllocateAndInitializeSid(c: &mut Ctx) -> u32 {
    let (auth, n, out) = (c.a(0), c.a(1) & 0xff, c.a(10));
    // (more than 8 sub-authorities: ERROR_INVALID_SID, winref security)
    if auth == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    if n > 8 {
        last_error(ERROR_INVALID_SID);
        return 0;
    }
    let a = (0..6).fold(0u64, |a, i| a << 8 | r8(auth + i) as u64);
    let sid = Sid(a, (0..n).map(|i| c.a(2 + i)).collect());
    putp(out, sid_alloc(&sid));
    1
}
fn FreeSid(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}
fn GetLengthSid(c: &mut Ctx) -> u32 {
    8 + 4 * r8(c.a(0) + 1) as u32
}
fn IsValidSid(c: &mut Ctx) -> u32 {
    Sid::read(c.a(0)).is_some() as u32
}
fn EqualSid(c: &mut Ctx) -> u32 {
    match (Sid::read(c.a(0)), Sid::read(c.a(1))) {
        (Some(a), Some(b)) => {
            last_error(0);
            (a == b) as u32
        }
        _ => {
            last_error(ERROR_INVALID_SID);
            0
        }
    }
}
fn CopySid(c: &mut Ctx) -> u32 {
    let (len, dst, src) = (c.a(0), c.a(1), c.a(2));
    let Some(s) = Sid::read(src) else {
        last_error(ERROR_INVALID_SID);
        return 0;
    };
    if len < s.len() {
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    mem::write(dst, &s.bytes());
    1
}
fn GetSidLengthRequired(c: &mut Ctx) -> u32 {
    8 + 4 * (c.a(0) & 0xff)
}
fn InitializeSid(c: &mut Ctx) -> u32 {
    let (p, auth, n) = (c.a(0), c.a(1), c.a(2) & 0xff);
    if n > 15 {
        return 0;
    }
    w8(p, 1);
    w8(p + 1, n as u8);
    for i in 0..6 {
        w8(p + 2 + i, r8(auth + i));
    }
    1
}
fn GetSidSubAuthority(c: &mut Ctx) -> u32 {
    last_error(0);
    c.a(0) + 8 + 4 * c.a(1)
}
fn GetSidSubAuthorityCount(c: &mut Ctx) -> u32 {
    last_error(0);
    c.a(0) + 1
}
fn GetSidIdentifierAuthority(c: &mut Ctx) -> u32 {
    last_error(0);
    c.a(0) + 2
}

/// CreateWellKnownSid: the ones programs ask for (WELL_KNOWN_SID_TYPE)
fn well_known(t: u32) -> Option<Sid> {
    Some(match t {
        0 => Sid(0, vec![0]),     // Null
        1 => everyone(),          // World
        2 => Sid(2, vec![0]),     // Local
        11 => Sid(5, vec![4]),    // Interactive
        13 => Sid(5, vec![6]),    // Service
        17 => Sid(5, vec![11]),   // Authenticated users
        22 => Sid(5, vec![18]),   // LocalSystem
        23 => Sid(5, vec![19]),   // LocalService
        24 => Sid(5, vec![20]),   // NetworkService
        25 => Sid(5, vec![32]),   // BuiltinDomain
        26 => admins(),           // BuiltinAdministrators
        27 => Sid(5, vec![32, 545]),
        28 => Sid(5, vec![32, 546]),
        66 => Sid(16, vec![0]),   // untrusted
        67 => Sid(16, vec![0x1000]),
        68 => Sid(16, vec![0x2000]),
        69 => Sid(16, vec![0x3000]),
        70 => Sid(16, vec![0x4000]),
        _ => return None,
    })
}
fn CreateWellKnownSid(c: &mut Ctx) -> u32 {
    let (t, out, lenp) = (c.a(0), c.a(2), c.a(3));
    let Some(s) = well_known(t) else {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let need = s.len();
    if lenp == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let have = r32(lenp);
    put32(lenp, need);
    if out == 0 || have < need {
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    mem::write(out, &s.bytes());
    1
}
fn IsWellKnownSid(c: &mut Ctx) -> u32 {
    Sid::read(c.a(0)).is_some_and(|s| well_known(c.a(1)) == Some(s)) as u32
}

// ---------------------------------------------------------------------------------------------
// ACLs (revision, size, ACE count, then ACEs: type, flags, size, mask, SID) and absolute
// security descriptors (revision, control, owner, group, SACL, DACL)

fn InitializeAcl(c: &mut Ctx) -> u32 {
    let (p, len, rev) = (c.a(0), c.a(1), c.a(2));
    if !(2..=4).contains(&rev) {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    if len < 8 {
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    if len > 0xffff {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    w8(p, rev as u8);
    w8(p + 1, 0);
    w16(p + 2, (len & !3) as u16);
    w16(p + 4, 0);
    w16(p + 6, 0);
    1
}
/// The ACEs of the ACL at `p`: (type, flags, mask, SID)
fn aces(p: u32) -> Vec<(u8, u8, u32, Option<Sid>)> {
    let mut v = Vec::new();
    let (size, n) = (r16(p + 2) as u32, r16(p + 4) as u32);
    let mut o = 8;
    for _ in 0..n {
        if o + 8 > size {
            break;
        }
        let a = p + o;
        v.push((r8(a), r8(a + 1), r32(a + 4), Sid::read(a + 8)));
        o += (r16(a + 2) as u32).max(8);
    }
    v
}
fn used(p: u32) -> u32 {
    let (size, n) = (r16(p + 2) as u32, r16(p + 4) as u32);
    let mut o = 8;
    for _ in 0..n {
        if o + 4 > size {
            break;
        }
        o += (r16(p + o + 2) as u32).max(8);
    }
    o
}
fn add_ace(acl: u32, rev: u32, ty: u8, flags: u8, mask: u32, sid: u32) -> u32 {
    if acl == 0 || !(2..=4).contains(&(r8(acl) as u32)) {
        last_error(ERROR_INVALID_ACL);
        return 0;
    }
    if !(2..=4).contains(&rev) {
        last_error(ERROR_UNKNOWN_REVISION);
        return 0;
    }
    let Some(s) = Sid::read(sid) else {
        last_error(ERROR_INVALID_SID);
        return 0;
    };
    let at = used(acl);
    let size = 8 + s.len();
    if at + size > r16(acl + 2) as u32 {
        last_error(ERROR_ALLOTTED_SPACE_EXCEEDED);
        return 0;
    }
    let a = acl + at;
    w8(a, ty);
    w8(a + 1, flags);
    w16(a + 2, size as u16);
    w32(a + 4, mask);
    mem::write(a + 8, &s.bytes());
    w16(acl + 4, r16(acl + 4) + 1);
    if rev > r8(acl) as u32 {
        w8(acl, rev as u8);
    }
    1
}
fn AddAccessAllowedAce(c: &mut Ctx) -> u32 {
    add_ace(c.a(0), c.a(1), 0, 0, c.a(2), c.a(3))
}
fn AddAccessAllowedAceEx(c: &mut Ctx) -> u32 {
    add_ace(c.a(0), c.a(1), 0, c.a(2) as u8, c.a(3), c.a(4))
}
fn AddAccessDeniedAce(c: &mut Ctx) -> u32 {
    add_ace(c.a(0), c.a(1), 1, 0, c.a(2), c.a(3))
}
fn AddAccessDeniedAceEx(c: &mut Ctx) -> u32 {
    add_ace(c.a(0), c.a(1), 1, c.a(2) as u8, c.a(3), c.a(4))
}
/// AddAce: copies the ACEs in the `len` bytes at `list` into the ACL before ACE `start`
/// (MAXDWORD, or any index past the end, appends)
fn AddAce(c: &mut Ctx) -> u32 {
    let (acl, rev, start, list, len) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if acl == 0 || !(2..=4).contains(&(r8(acl) as u32)) {
        last_error(ERROR_INVALID_ACL);
        return 0;
    }
    // (any revision from 2 is taken, winref security: AddAce(acl, 9, ..) succeeds)
    if rev < 2 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    // the ACEs in the list, each at least an 8 byte header
    let mut n = 0u32;
    let mut o = 0;
    while o + 4 <= len {
        let sz = r16(list + o + 2) as u32;
        if sz < 8 || o + sz > len {
            break;
        }
        o += sz;
        n += 1;
    }
    if o != len {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let end = used(acl);
    // (no room: AddAce's ERROR_INSUFFICIENT_BUFFER where AddAccessAllowedAce gives
    // ERROR_ALLOTTED_SPACE_EXCEEDED, winref security)
    if end + len > r16(acl + 2) as u32 {
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    let count = r16(acl + 4) as u32;
    let mut at = 8;
    for _ in 0..start.min(count) {
        at += (r16(acl + at + 2) as u32).max(8);
    }
    mem::copy(acl + at + len, acl + at, end - at);
    mem::copy(acl + at, list, len);
    w16(acl + 4, (count + n) as u16);
    if rev > r8(acl) as u32 && rev <= 4 {
        w8(acl, rev as u8);
    }
    1
}
fn IsValidAcl(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    ((2..=4).contains(&r8(p)) && r16(p + 2) >= 8 && used(p) <= r16(p + 2) as u32) as u32
}
fn GetAclInformation(c: &mut Ctx) -> u32 {
    let (acl, out, len, class) = (c.a(0), c.a(1), c.a(2), c.a(3));
    match class {
        // AclRevisionInformation
        1 if len >= 4 => w32(out, r8(acl) as u32),
        // AclSizeInformation: AceCount, AclBytesInUse, AclBytesFree
        2 if len >= 12 => {
            let u = used(acl);
            w32(out, r16(acl + 4) as u32);
            w32(out + 4, u);
            w32(out + 8, r16(acl + 2) as u32 - u);
        }
        _ => {
            last_error(ERROR_INVALID_PARAMETER);
            return 0;
        }
    }
    1
}
fn GetAce(c: &mut Ctx) -> u32 {
    let (acl, i, out) = (c.a(0), c.a(1), c.a(2));
    if i >= r16(acl + 4) as u32 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let mut o = 8;
    for _ in 0..i {
        o += (r16(acl + o + 2) as u32).max(8);
    }
    mem::putp(out, acl + o);
    1
}

/// An absolute descriptor's fields: x86 (revision, sbz, control, owner, group, sacl, dacl),
/// x64 the same with 8-byte pointers from offset 8. A self-relative one keeps offsets instead.
fn sd_field(sd: u32, i: u32) -> u32 {
    let ctl = r16(sd + 2);
    if ctl & 0x8000 != 0 {
        let off = r32(sd + 4 + 4 * i);
        return if off == 0 { 0 } else { sd + off };
    }
    mem::rp(sd + if mem::x64() { 8 + 8 * i } else { 4 + 4 * i })
}
fn sd_set(sd: u32, i: u32, v: u32) {
    mem::wp(sd + if mem::x64() { 8 + 8 * i } else { 4 + 4 * i }, v);
}
fn InitializeSecurityDescriptor(c: &mut Ctx) -> u32 {
    let (sd, rev) = (c.a(0), c.a(1));
    if rev != 1 {
        last_error(ERROR_UNKNOWN_REVISION);
        return 0;
    }
    mem::fill(sd, if mem::x64() { 40 } else { 20 }, 0);
    w8(sd, 1);
    1
}
fn sd_ok(sd: u32) -> bool {
    sd != 0 && r8(sd) == 1
}
fn set_sd(c: &mut Ctx, i: u32, present_flag: u16, default_flag: u16, has_present: bool) -> u32 {
    let sd = c.a(0);
    if !sd_ok(sd) {
        last_error(ERROR_UNKNOWN_REVISION);
        return 0;
    }
    if r16(sd + 2) & 0x8000 != 0 {
        last_error(ERROR_INVALID_SECURITY_DESCR);
        return 0;
    }
    let (present, v, defaulted) = if has_present { (c.a(1) != 0, c.a(2), c.a(3) != 0) } else { (true, c.a(1), c.a(2) != 0) };
    let mut ctl = r16(sd + 2) & !(present_flag | default_flag);
    if present {
        ctl |= present_flag;
        sd_set(sd, i, v);
    } else {
        sd_set(sd, i, 0);
    }
    if defaulted {
        ctl |= default_flag;
    }
    w16(sd + 2, ctl);
    1
}
fn SetSecurityDescriptorOwner(c: &mut Ctx) -> u32 {
    set_sd(c, 0, 0, 1, false)
}
fn SetSecurityDescriptorGroup(c: &mut Ctx) -> u32 {
    set_sd(c, 1, 0, 2, false)
}
fn SetSecurityDescriptorDacl(c: &mut Ctx) -> u32 {
    set_sd(c, 3, 4, 8, true)
}
fn SetSecurityDescriptorSacl(c: &mut Ctx) -> u32 {
    set_sd(c, 2, 0x10, 0x20, true)
}
fn IsValidSecurityDescriptor(c: &mut Ctx) -> u32 {
    if sd_ok(c.a(0)) {
        1
    } else {
        last_error(ERROR_INVALID_SECURITY_DESCR);
        0
    }
}
fn GetSecurityDescriptorDacl(c: &mut Ctx) -> u32 {
    let (sd, present, dacl, defaulted) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if !sd_ok(sd) {
        last_error(ERROR_UNKNOWN_REVISION);
        return 0;
    }
    let ctl = r16(sd + 2);
    put32(present, (ctl & 4 != 0) as u32);
    if ctl & 4 != 0 {
        mem::putp(dacl, sd_field(sd, 3));
        put32(defaulted, (ctl & 8 != 0) as u32);
    }
    1
}
fn GetSecurityDescriptorOwner(c: &mut Ctx) -> u32 {
    let (sd, owner, defaulted) = (c.a(0), c.a(1), c.a(2));
    if !sd_ok(sd) {
        last_error(ERROR_UNKNOWN_REVISION);
        return 0;
    }
    mem::putp(owner, sd_field(sd, 0));
    put32(defaulted, (r16(sd + 2) & 1 != 0) as u32);
    1
}
fn GetSecurityDescriptorGroup(c: &mut Ctx) -> u32 {
    let (sd, group, defaulted) = (c.a(0), c.a(1), c.a(2));
    if !sd_ok(sd) {
        last_error(ERROR_UNKNOWN_REVISION);
        return 0;
    }
    mem::putp(group, sd_field(sd, 1));
    put32(defaulted, (r16(sd + 2) & 2 != 0) as u32);
    1
}
fn GetSecurityDescriptorControl(c: &mut Ctx) -> u32 {
    let sd = c.a(0);
    if !sd_ok(sd) {
        last_error(ERROR_UNKNOWN_REVISION);
        return 0;
    }
    w16(c.a(1), r16(sd + 2));
    put32(c.a(2), r8(sd) as u32);
    1
}
fn GetSecurityDescriptorLength(c: &mut Ctx) -> u32 {
    let sd = c.a(0);
    let mut n = if mem::x64() { 40 } else { 20 };
    for i in [0, 1] {
        if let Some(s) = Sid::read(sd_field(sd, i)) {
            n += s.len();
        }
    }
    for (i, bit) in [(2, 0x10), (3, 4)] {
        let a = sd_field(sd, i);
        if r16(sd + 2) & bit != 0 && a != 0 {
            n += r16(a + 2) as u32;
        }
    }
    n
}

/// GENERIC_* bits mapped through a GENERIC_MAPPING (read, write, execute, all)
fn map_generic(mask: u32, gm: u32) -> u32 {
    let mut m = mask & 0x0fff_ffff;
    for (bit, i) in [(0x8000_0000u32, 0), (0x4000_0000, 1), (0x2000_0000, 2), (0x1000_0000, 3)] {
        if mask & bit != 0 && gm != 0 {
            m |= r32(gm + 4 * i);
        }
    }
    m
}

/// What the token may do with an object of descriptor `sd`: allowed ACEs that apply grant,
/// denied ones refuse first (in order, as the DACL is walked); no DACL grants everything, an
/// empty one nothing. The owner always gets READ_CONTROL and WRITE_DAC.
fn granted(sd: u32, want: u32) -> u32 {
    let ctl = r16(sd + 2);
    let dacl = sd_field(sd, 3);
    if ctl & 4 == 0 || dacl == 0 {
        return if want & 0x0200_0000 != 0 { 0x1f_01ff } else { want };
    }
    let mut ok = 0u32;
    let mut denied = 0u32;
    if Sid::read(sd_field(sd, 0)).is_some_and(|o| member(&o)) {
        ok |= 0x0002_0000 | 0x0004_0000;
    }
    for (ty, flags, mask, sid) in aces(dacl) {
        if flags & 0x8 != 0 || !sid.as_ref().is_some_and(member) {
            continue; // INHERIT_ONLY, or not ours
        }
        match ty {
            0 => ok |= mask & !denied,
            1 => denied |= mask & !ok,
            _ => {}
        }
    }
    ok
}

fn AccessCheck(c: &mut Ctx) -> u32 {
    let (sd, token, want, gm, privs, privlen, grant_p, status_p) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    if !is_token(token) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    if !impersonation_token(token) {
        last_error(ERROR_NO_IMPERSONATION_TOKEN);
        return 0;
    }
    if !sd_ok(sd) || sd_field(sd, 0) == 0 || sd_field(sd, 1) == 0 {
        last_error(ERROR_INVALID_SECURITY_DESCR);
        return 0;
    }
    // (generic rights must be mapped first: ERROR_GENERIC_NOT_MAPPED, winref security)
    if want & 0xf000_0000 != 0 {
        last_error(1360);
        return 0;
    }
    if privlen != 0 && r32(privlen) < 8 {
        put32(privlen, 8);
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    if privs != 0 {
        w32(privs, 0);
        w32(privs + 4, 0);
    }
    let want = map_generic(want, gm);
    let g = granted(sd, want);
    let (status, mask) = if want & 0x0200_0000 != 0 {
        (g != 0, g)
    } else {
        (want & !g == 0, if want & !g == 0 { want } else { 0 })
    };
    put32(grant_p, mask);
    put32(status_p, status as u32);
    last_error(if status { 0 } else { ERROR_ACCESS_DENIED });
    1
}

// ---------------------------------------------------------------------------------------------
// tokens

static IMPERSONATING: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

fn new_token(kind: &'static str) -> u32 {
    obj::add(Obj::Other(kind))
}
fn is_token(h: u32) -> bool {
    // (GetCurrentProcessToken, GetCurrentThreadToken, GetCurrentThreadEffectiveToken)
    matches!(h as i32, -4 | -5 | -6) || obj::with(|t| matches!(t.objs.get(&h), Some((Obj::Other("token" | "impersonation token"), _))))
}
fn impersonation_token(h: u32) -> bool {
    obj::with(|t| matches!(t.objs.get(&h), Some((Obj::Other("impersonation token"), _))))
}

fn OpenProcessToken(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    if out == 0 {
        last_error(998); // ERROR_NOACCESS
        return 0;
    }
    putp(out, new_token("token"));
    1
}
fn OpenThreadToken(c: &mut Ctx) -> u32 {
    let out = c.a(3);
    let tid = crate::cur().tid;
    let imp = IMPERSONATING.lock().unwrap().as_ref().is_some_and(|s| s.contains(&tid));
    if !imp {
        // (the handle is cleared through WoW64's thunk; 64-bit Windows leaves it, winref security)
        if !mem::x64() {
            putp(out, 0);
        }
        last_error(ERROR_NO_TOKEN);
        return 0;
    }
    putp(out, new_token("impersonation token"));
    1
}
fn ImpersonateSelf(_: &mut Ctx) -> u32 {
    IMPERSONATING.lock().unwrap().get_or_insert_with(HashSet::new).insert(crate::cur().tid);
    1
}
fn RevertToSelf(_: &mut Ctx) -> u32 {
    if let Some(s) = IMPERSONATING.lock().unwrap().as_mut() {
        s.remove(&crate::cur().tid);
    }
    1
}
fn DuplicateToken(c: &mut Ctx) -> u32 {
    if !is_token(c.a(0)) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    putp(c.a(2), new_token("impersonation token"));
    1
}
fn DuplicateTokenEx(c: &mut Ctx) -> u32 {
    if !is_token(c.a(0)) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    // TokenType: 1 primary, 2 impersonation
    putp(c.a(5), new_token(if c.a(4) == 2 { "impersonation token" } else { "token" }));
    1
}

/// Privileges an elevated administrator holds: (LUID, name, enabled by default)
const PRIVILEGES: &[(u32, &str, bool)] = &[
    (2, "SeCreateTokenPrivilege", false),
    (5, "SeIncreaseQuotaPrivilege", false),
    (8, "SeSecurityPrivilege", false),
    (9, "SeTakeOwnershipPrivilege", false),
    (10, "SeLoadDriverPrivilege", false),
    (11, "SeSystemProfilePrivilege", false),
    (12, "SeSystemtimePrivilege", false),
    (13, "SeProfileSingleProcessPrivilege", false),
    (14, "SeIncreaseBasePriorityPrivilege", false),
    (15, "SeCreatePagefilePrivilege", false),
    (17, "SeBackupPrivilege", false),
    (18, "SeRestorePrivilege", false),
    (19, "SeShutdownPrivilege", false),
    (20, "SeDebugPrivilege", false),
    (22, "SeSystemEnvironmentPrivilege", false),
    (23, "SeChangeNotifyPrivilege", true),
    (24, "SeRemoteShutdownPrivilege", false),
    (25, "SeUndockPrivilege", false),
    (28, "SeManageVolumePrivilege", false),
    (29, "SeImpersonatePrivilege", true),
    (30, "SeCreateGlobalPrivilege", true),
    (33, "SeIncreaseWorkingSetPrivilege", false),
    (34, "SeTimeZonePrivilege", false),
    (35, "SeCreateSymbolicLinkPrivilege", false),
    (36, "SeDelegateSessionUserImpersonatePrivilege", false),
];
static ENABLED: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
fn enabled(luid: u32) -> bool {
    ENABLED.lock().unwrap().as_ref().map_or(false, |s| s.contains(&luid)) || PRIVILEGES.iter().any(|p| p.0 == luid && p.2)
}

fn lookup_privilege(name: &str) -> Option<u32> {
    PRIVILEGES.iter().find(|p| p.1.eq_ignore_ascii_case(name)).map(|p| p.0)
}
fn LookupPrivilegeValueA(c: &mut Ctx) -> u32 {
    let name = String::from_utf8_lossy(&cstr(c.a(1))).into_owned();
    put_luid(c.a(2), lookup_privilege(&name))
}
fn LookupPrivilegeValueW(c: &mut Ctx) -> u32 {
    let name = String::from_utf16_lossy(&wstr(c.a(1)));
    put_luid(c.a(2), lookup_privilege(&name))
}
fn put_luid(p: u32, l: Option<u32>) -> u32 {
    match l {
        Some(l) => {
            put32(p, l);
            put32(p + 4, 0);
            last_error(0); // (as Windows: success clears it, winref security)
            1
        }
        None => {
            last_error(ERROR_NO_SUCH_PRIVILEGE);
            0
        }
    }
}
fn LookupPrivilegeNameW(c: &mut Ctx) -> u32 {
    let (luid, out, lenp) = (c.a(1), c.a(2), c.a(3));
    let Some(p) = PRIVILEGES.iter().find(|p| p.0 == r32(luid) && r32(luid + 4) == 0) else {
        last_error(ERROR_NO_SUCH_PRIVILEGE);
        return 0;
    };
    let w: Vec<u16> = p.1.encode_utf16().collect();
    let have = r32(lenp);
    if have <= w.len() as u32 {
        put32(lenp, w.len() as u32 + 1);
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    mem::put_wstr(out, have, &w);
    put32(lenp, w.len() as u32);
    last_error(0);
    1
}

/// AdjustTokenPrivileges: TOKEN_PRIVILEGES (count, then LUID + attributes each); the previous
/// state if asked; ERROR_NOT_ALL_ASSIGNED (1300) for a privilege the token does not hold.
fn AdjustTokenPrivileges(c: &mut Ctx) -> u32 {
    let (token, disable_all, new, buflen, prev, retlen) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if !is_token(token) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    let mut changes: Vec<(u32, bool)> = Vec::new();
    let mut missing = false;
    if disable_all != 0 {
        changes.extend(PRIVILEGES.iter().map(|p| (p.0, false)));
    } else if new != 0 {
        for i in 0..r32(new) {
            let e = new + 4 + 12 * i;
            let (luid, attr) = (r32(e), r32(e + 8));
            if r32(e + 4) != 0 || !PRIVILEGES.iter().any(|p| p.0 == luid) {
                missing = true;
                continue;
            }
            changes.push((luid, attr & 2 != 0)); // SE_PRIVILEGE_ENABLED
        }
    }
    let changed: Vec<(u32, bool)> = changes.iter().filter(|(l, on)| enabled(*l) != *on).map(|(l, _)| (*l, enabled(*l))).collect();
    if prev != 0 {
        let need = 4 + 12 * changed.len() as u32;
        put32(retlen, need);
        if buflen < need {
            last_error(ERROR_INSUFFICIENT_BUFFER);
            return 0;
        }
        w32(prev, changed.len() as u32);
        for (i, (l, on)) in changed.iter().enumerate() {
            let e = prev + 4 + 12 * i as u32;
            w32(e, *l);
            w32(e + 4, 0);
            w32(e + 8, if *on { 2 } else { 0 });
        }
    }
    let mut g = ENABLED.lock().unwrap();
    let s = g.get_or_insert_with(HashSet::new);
    for (l, on) in changes {
        if on {
            s.insert(l);
        } else {
            s.remove(&l);
        }
    }
    last_error(if missing { 1300 } else { 0 });
    1
}

fn CheckTokenMembership(c: &mut Ctx) -> u32 {
    let (token, sid, out) = (c.a(0), c.a(1), c.a(2));
    if token != 0 && !is_token(token) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    if token != 0 && !impersonation_token(token) && !matches!(token as i32, -4 | -5 | -6) {
        last_error(ERROR_NO_IMPERSONATION_TOKEN);
        return 0;
    }
    let Some(s) = Sid::read(sid) else {
        last_error(ERROR_INVALID_SID);
        return 0;
    };
    put32(out, member(&s) as u32);
    1
}

/// GetTokenInformation: the classes programs ask (TokenUser, TokenGroups, TokenPrivileges,
/// TokenOwner, TokenPrimaryGroup, TokenType, TokenElevationType, TokenLinkedToken refused,
/// TokenElevation, TokenIntegrityLevel, TokenIsAppContainer, TokenSessionId)
fn GetTokenInformation(c: &mut Ctx) -> u32 {
    let (token, class, buf, len, retp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if !is_token(token) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    let ps = mem::ps();
    // the fixed part (as bytes, pointers patched in after) and SIDs appended after it
    let sid_and_attr = |sids: &[(Sid, u32)], count: bool| -> (Vec<u8>, Vec<(usize, Vec<u8>)>) {
        let head = if count { ps } else { 0 } as usize;
        let mut fixed = vec![0u8; head + sids.len() * 2 * ps as usize];
        if count {
            fixed[..4].copy_from_slice(&(sids.len() as u32).to_le_bytes());
        }
        let mut tail = Vec::new();
        for (i, (s, a)) in sids.iter().enumerate() {
            let o = head + i * 2 * ps as usize;
            fixed[o + ps as usize..o + ps as usize + 4].copy_from_slice(&a.to_le_bytes());
            tail.push((o, s.bytes()));
        }
        (fixed, tail)
    };
    let (fixed, sids): (Vec<u8>, Vec<(usize, Vec<u8>)>) = match class {
        1 => sid_and_attr(&[(the_user(), 0)], false),
        2 => sid_and_attr(&groups(), true),
        3 => {
            let mut v = (PRIVILEGES.len() as u32).to_le_bytes().to_vec();
            for p in PRIVILEGES {
                v.extend_from_slice(&p.0.to_le_bytes());
                v.extend_from_slice(&0u32.to_le_bytes());
                v.extend_from_slice(&(if enabled(p.0) { 2u32 } else { 0 } | if p.2 { 1 } else { 0 }).to_le_bytes());
            }
            (v, Vec::new())
        }
        4 => (vec![0; ps as usize], vec![(0, admins().bytes())]),
        5 => (vec![0; ps as usize], vec![(0, the_user().bytes())]),
        8 => ((if impersonation_token(token) { 2u32 } else { 1 }).to_le_bytes().to_vec(), Vec::new()),
        9 if impersonation_token(token) => (2u32.to_le_bytes().to_vec(), Vec::new()), // SecurityImpersonation
        12 => (1u32.to_le_bytes().to_vec(), Vec::new()),                               // TokenSessionId
        18 => (2u32.to_le_bytes().to_vec(), Vec::new()),                               // TokenElevationTypeFull
        20 => (1u32.to_le_bytes().to_vec(), Vec::new()),                               // TokenIsElevated
        25 => sid_and_attr(&[(Sid(16, vec![0x3000]), 0x60)], false),
        29 => (0u32.to_le_bytes().to_vec(), Vec::new()), // TokenIsAppContainer
        _ => {
            last_error(ERROR_INVALID_PARAMETER);
            return 0;
        }
    };
    let need = fixed.len() + sids.iter().map(|s| s.1.len()).sum::<usize>();
    put32(retp, need as u32);
    if (len as usize) < need || buf == 0 {
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    mem::write(buf, &fixed);
    let mut at = buf + fixed.len() as u32;
    for (o, b) in sids {
        mem::wp(buf + o as u32, at);
        mem::write(at, &b);
        at += b.len() as u32;
    }
    1
}


// ---------------------------------------------------------------------------------------------
// the service control manager

struct Service {
    display: String,
    ty: u32,
    start: u32,
    path: String,
}
#[derive(Default)]
struct Scm {
    services: HashMap<String, Service>,
    /// service handle -> service name
    open: HashMap<u32, String>,
    managers: HashSet<u32>,
}
static SCM: Mutex<Option<Scm>> = Mutex::new(None);
fn scm<R>(f: impl FnOnce(&mut Scm) -> R) -> R {
    f(SCM.lock().unwrap().get_or_insert_with(Scm::default))
}
fn arg(p: u32, wide: bool) -> Option<String> {
    (p != 0).then(|| if wide { String::from_utf16_lossy(&wstr(p)) } else { crate::text::a2s(&cstr(p)) })
}

fn open_manager() -> u32 {
    let h = obj::add(Obj::Other("service manager"));
    scm(|s| s.managers.insert(h));
    h
}
fn OpenSCManagerA(_: &mut Ctx) -> u32 {
    open_manager()
}
fn OpenSCManagerW(_: &mut Ctx) -> u32 {
    open_manager()
}
fn open_service(c: &mut Ctx, wide: bool) -> u32 {
    let (m, name) = (c.a(0), arg(c.a(1), wide).unwrap_or_default());
    if !scm(|s| s.managers.contains(&m)) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    let key = name.to_ascii_lowercase();
    if !scm(|s| s.services.contains_key(&key)) {
        last_error(ERROR_SERVICE_DOES_NOT_EXIST);
        return 0;
    }
    let h = obj::add(Obj::Other("service"));
    scm(|s| s.open.insert(h, key));
    h
}
fn OpenServiceA(c: &mut Ctx) -> u32 {
    open_service(c, false)
}
fn OpenServiceW(c: &mut Ctx) -> u32 {
    open_service(c, true)
}
fn create_service(c: &mut Ctx, wide: bool) -> u32 {
    let m = c.a(0);
    if !scm(|s| s.managers.contains(&m)) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    let Some(name) = arg(c.a(1), wide).filter(|n| !n.is_empty()) else {
        last_error(123); // ERROR_INVALID_NAME
        return 0;
    };
    let key = name.to_ascii_lowercase();
    if scm(|s| s.services.contains_key(&key)) {
        last_error(ERROR_SERVICE_EXISTS);
        return 0;
    }
    let display = arg(c.a(2), wide).unwrap_or_else(|| name.clone());
    let (ty, start, path) = (c.a(4), c.a(5), arg(c.a(7), wide).unwrap_or_default());
    if std::env::var("SAKE_TRACE_SERVICES").is_ok() {
        eprintln!("sake: CreateService {name} type {ty:#x} start {start} {path}");
    }
    let h = obj::add(Obj::Other("service"));
    scm(|s| {
        s.services.insert(key.clone(), Service { display, ty, start, path });
        s.open.insert(h, key);
    });
    put32(c.a(10), 0);
    h
}
fn CreateServiceA(c: &mut Ctx) -> u32 {
    create_service(c, false)
}
fn CreateServiceW(c: &mut Ctx) -> u32 {
    create_service(c, true)
}
fn service_of(h: u32) -> Option<(u32, u32)> {
    scm(|s| s.open.get(&h).and_then(|k| s.services.get(k)).map(|v| (v.ty, v.start)))
}
fn start_service(c: &mut Ctx) -> u32 {
    let Some((ty, start)) = service_of(c.a(0)) else {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    // SERVICE_KERNEL_DRIVER / SERVICE_FILE_SYSTEM_DRIVER: no kernel here
    last_error(if ty & 3 != 0 { ERROR_DRIVER_BLOCKED } else if start == 4 { ERROR_SERVICE_DISABLED } else { ERROR_SERVICE_DISABLED });
    0
}
fn StartServiceA(c: &mut Ctx) -> u32 {
    start_service(c)
}
fn StartServiceW(c: &mut Ctx) -> u32 {
    start_service(c)
}
/// SERVICE_STATUS: type, SERVICE_STOPPED, accepted 0, exit code, ...
fn put_status(p: u32, ty: u32) {
    mem::fill(p, 28, 0);
    w32(p, ty);
    w32(p + 4, 1);
}
fn ControlService(c: &mut Ctx) -> u32 {
    let Some((ty, _)) = service_of(c.a(0)) else {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    put_status(c.a(2), ty);
    last_error(ERROR_SERVICE_NOT_ACTIVE);
    0
}
fn QueryServiceStatus(c: &mut Ctx) -> u32 {
    let Some((ty, _)) = service_of(c.a(0)) else {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    put_status(c.a(1), ty);
    1
}
fn QueryServiceStatusEx(c: &mut Ctx) -> u32 {
    let (h, level, buf, len, need) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((ty, _)) = service_of(h) else {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    if level != 0 {
        last_error(124); // ERROR_INVALID_LEVEL
        return 0;
    }
    put32(need, 36);
    if len < 36 {
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    mem::fill(buf, 36, 0);
    put_status(buf, ty);
    1
}
fn DeleteService(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let ok = scm(|s| match s.open.get(&h).cloned() {
        Some(k) => s.services.remove(&k).is_some(),
        None => false,
    });
    if !ok {
        last_error(ERROR_INVALID_HANDLE);
    }
    ok as u32
}
fn CloseServiceHandle(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let known = scm(|s| s.open.remove(&h).is_some() || s.managers.remove(&h));
    if !known {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    obj::close(h);
    1
}
fn query_config(c: &mut Ctx, wide: bool) -> u32 {
    let (h, buf, len, need) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((ty, start, path, display)) = scm(|s| s.open.get(&h).and_then(|k| s.services.get(k)).map(|v| (v.ty, v.start, v.path.clone(), v.display.clone()))) else {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    let ps = mem::ps();
    let fixed = 4 * 3 + ps * 6;
    let enc = |s: &str| -> Vec<u8> {
        if wide {
            s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect()
        } else {
            let mut v = crate::text::w2a(&s.encode_utf16().collect::<Vec<_>>());
            v.push(0);
            v
        }
    };
    let strs = [enc(&path), enc(""), enc(""), enc(""), enc(""), enc(&display)];
    let total = fixed + strs.iter().map(|s| s.len() as u32).sum::<u32>();
    put32(need, total);
    if len < total || buf == 0 {
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    w32(buf, ty);
    w32(buf + 4, start);
    w32(buf + 8, 1); // SERVICE_ERROR_NORMAL
    let mut at = buf + fixed;
    for (i, s) in strs.iter().enumerate() {
        mem::wp(buf + 12 + ps * i as u32, at);
        mem::write(at, s);
        at += s.len() as u32;
    }
    1
}
fn QueryServiceConfigA(c: &mut Ctx) -> u32 {
    query_config(c, false)
}
fn QueryServiceConfigW(c: &mut Ctx) -> u32 {
    query_config(c, true)
}

pub fn init() {
    reg!("advapi32";
        AllocateAndInitializeSid, FreeSid, GetLengthSid, IsValidSid, EqualSid, CopySid, GetSidLengthRequired,
        InitializeSid, GetSidSubAuthority, GetSidSubAuthorityCount, GetSidIdentifierAuthority, CreateWellKnownSid,
        IsWellKnownSid, InitializeAcl, AddAce, AddAccessAllowedAce, AddAccessAllowedAceEx, AddAccessDeniedAce,
        AddAccessDeniedAceEx, IsValidAcl, GetAclInformation, GetAce, InitializeSecurityDescriptor,
        SetSecurityDescriptorOwner, SetSecurityDescriptorGroup, SetSecurityDescriptorDacl, SetSecurityDescriptorSacl,
        IsValidSecurityDescriptor, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetSecurityDescriptorGroup,
        GetSecurityDescriptorControl, GetSecurityDescriptorLength, AccessCheck, OpenProcessToken, OpenThreadToken,
        ImpersonateSelf, RevertToSelf, DuplicateToken, DuplicateTokenEx, LookupPrivilegeValueA, LookupPrivilegeValueW,
        LookupPrivilegeNameW, AdjustTokenPrivileges, CheckTokenMembership, GetTokenInformation,
        OpenSCManagerA, OpenSCManagerW, OpenServiceA, OpenServiceW, CreateServiceA, CreateServiceW, StartServiceA,
        StartServiceW, ControlService, QueryServiceStatus, QueryServiceStatusEx, DeleteService, CloseServiceHandle,
        QueryServiceConfigA, QueryServiceConfigW, ConvertSidToStringSidA, ConvertSidToStringSidW,
        ConvertStringSidToSidA, ConvertStringSidToSidW,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn sid_strings() {
        assert_eq!(sid_to_string(&admins()), "S-1-5-32-544");
        assert_eq!(sid_to_string(&Sid(0x1234_5678_9abc, vec![1])), "S-1-0x123456789ABC-1");
        assert_eq!(parse_sid("S-1-5-32-544"), Some(admins()));
        assert_eq!(parse_sid("BA"), Some(admins()));
        assert_eq!(parse_sid("S-1-0x5-0x20"), Some(Sid(5, vec![32])));
        assert_eq!(parse_sid("S-1-5"), Some(Sid(5, vec![])));
        for bad in ["", "S-", "S-1", "S-2-5-32", "S-1-5-", "S-1--5", "S-1-5-4294967296", "S-1-x", "S-1-281474976710656", "S-1-5-+1"] {
            assert_eq!(parse_sid(bad), None, "{bad}");
        }
        assert_eq!(parse_sid(&format!("S-1-5{}", "-1".repeat(15))).map(|s| s.1.len()), Some(15));
        assert_eq!(parse_sid(&format!("S-1-5{}", "-1".repeat(16))), None);
    }

    proptest! {
        /// Every SID's text reads back as the same SID, and its bytes as Sid::read would see them
        #[test]
        fn sid_text_round_trips(auth in 0u64..1 << 48, subs in proptest::collection::vec(any::<u32>(), 0..=15)) {
            let s = Sid(auth, subs);
            prop_assert_eq!(parse_sid(&sid_to_string(&s)), Some(s.clone()));
            prop_assert_eq!(s.bytes().len() as u32, s.len());
        }
        /// Text that is not a SID never panics, and what parses prints back to the same SID
        #[test]
        fn sid_text_any(t in "[Ss0-9xXa-fA-F-]{0,40}") {
            if let Some(s) = parse_sid(&t) {
                prop_assert_eq!(parse_sid(&sid_to_string(&s)), Some(s));
            }
        }
    }

    #[test]
    fn sid_bytes() {
        assert_eq!(admins().bytes(), [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 2, 0, 0]);
        assert_eq!(everyone().len(), 12);
        assert!(member(&admins()) && member(&everyone()) && !member(&Sid(5, vec![18])));
    }
}
