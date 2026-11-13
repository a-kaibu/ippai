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
//! - User-mode service management is implemented in services.rs. Kernel drivers
//!   remain unsupported because sake runs user-mode guest code.

#![allow(non_snake_case)]

use std::collections::HashSet;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, cstr, put32, putp, r16, r32, r8, w16, w32, w8, wstr};
use crate::obj::{self, Obj};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_NO_TOKEN: u32 = 1008;
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
pub(crate) fn the_user() -> Sid {
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
        3 => Sid(3, vec![0]),     // CreatorOwner
        4 => Sid(3, vec![1]),     // CreatorGroup
        5 => Sid(3, vec![2]),     // CreatorOwnerServer
        6 => Sid(3, vec![3]),     // CreatorGroupServer
        7 => Sid(5, vec![]),      // NtAuthority
        8 => Sid(5, vec![1]),     // Dialup
        9 => Sid(5, vec![2]),     // Network
        10 => Sid(5, vec![3]),    // Batch
        11 => Sid(5, vec![4]),    // Interactive
        12 => Sid(5, vec![6]),    // Service
        13 => Sid(5, vec![7]),    // Anonymous
        14 => Sid(5, vec![8]),    // Proxy
        15 => Sid(5, vec![9]),    // EnterpriseControllers
        16 => Sid(5, vec![10]),   // Self
        18 => Sid(5, vec![12]),   // RestrictedCode
        19 => Sid(5, vec![13]),   // TerminalServer
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
pub(crate) const PRIVILEGES: &[(u32, &str, bool)] = &[
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

/// SetEntriesInAcl(count, EXPLICIT_ACCESS list, old ACL, &new ACL): a new ACL (LocalAlloc'd)
/// from the old one's ACEs and the entries: SET_ACCESS and REVOKE_ACCESS drop the trustee's
/// ACEs first, DENY_ACCESS puts a denied ACE before the allowed ones, GRANT_ACCESS and
/// SET_ACCESS add an allowed one. Trustees by SID, or by CURRENT_USER and the accounts'
/// names (LookupAccountName's).
fn set_entries(c: &mut Ctx, wide: bool) -> u32 {
    let (count, list, old, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 || count != 0 && list == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    // (no entries and no old ACL: no ACL at all, a NULL one (winref host_aclentries))
    if count == 0 && old == 0 {
        mem::putp(out, 0);
        return 0;
    }
    let x64 = mem::x64();
    let (stride, trustee) = if x64 { (48, 16) } else { (32, 12) };
    // (deny ACEs, then allowed ones: the order Windows' explicit entries get)
    let mut denied: Vec<(u8, u8, u32, Sid)> = Vec::new();
    let mut allowed: Vec<(u8, u8, u32, Sid)> = Vec::new();
    if old != 0 {
        for (ty, flags, mask, sid) in aces(old) {
            let Some(sid) = sid else { continue };
            if ty == 1 { denied.push((ty, flags, mask, sid)) } else { allowed.push((ty, flags, mask, sid)) }
        }
    }
    for i in 0..count {
        let e = list + i * stride;
        let (mask, mode, inherit) = (r32(e), r32(e + 4), r32(e + 8));
        let t = e + trustee;
        let (form, name) = if x64 { (r32(t + 12), mem::rp(t + 24)) } else { (r32(t + 8), r32(t + 16)) };
        let sid = match form {
            0 => match Sid::read(name) { Some(s) => s, None => return ERROR_INVALID_SID },
            1 => {
                let n = if wide { String::from_utf16_lossy(&wstr(name)) } else { crate::text::a2s(&mem::cstr(name)) };
                if n.eq_ignore_ascii_case("CURRENT_USER") {
                    the_user()
                } else {
                    match account_by_name(&n) {
                        Some(a) => a.2,
                        None => return ERROR_NONE_MAPPED,
                    }
                }
            }
            _ => return ERROR_INVALID_PARAMETER,
        };
        let flags = (inherit & 0x1f) as u8;
        match mode {
            // NOT_USED_ACCESS
            0 => {}
            // GRANT_ACCESS, SET_ACCESS
            1 | 2 => {
                if mode == 2 {
                    allowed.retain(|a| a.3 != sid);
                    denied.retain(|a| a.3 != sid);
                }
                if let Some(a) = allowed.iter_mut().find(|a| a.0 == 0 && a.3 == sid && a.1 == flags) {
                    a.2 |= mask;
                } else {
                    allowed.push((0, flags, mask, sid));
                }
            }
            3 => denied.push((1, flags, mask, sid)),
            4 => {
                allowed.retain(|a| a.3 != sid);
                denied.retain(|a| a.3 != sid);
            }
            _ => return ERROR_INVALID_PARAMETER,
        }
    }
    let all: Vec<_> = denied.into_iter().chain(allowed).collect();
    let size = 8 + all.iter().map(|a| 8 + a.3.len()).sum::<u32>();
    // (LMEM_ZEROINIT: the caller frees it with LocalFree)
    let acl = crate::k32::global_alloc(0x40, size);
    if acl == 0 {
        return 8;
    }
    w8(acl, 2);
    w16(acl + 2, size as u16);
    let mut at = acl + 8;
    for (ty, flags, mask, sid) in &all {
        w8(at, *ty);
        w8(at + 1, *flags);
        w16(at + 2, (8 + sid.len()) as u16);
        w32(at + 4, *mask);
        mem::write(at + 8, &sid.bytes());
        at += 8 + sid.len();
    }
    w16(acl + 4, all.len() as u16);
    mem::wp(out, acl);
    0
}
fn SetEntriesInAclA(c: &mut Ctx) -> u32 {
    set_entries(c, false)
}
fn SetEntriesInAclW(c: &mut Ctx) -> u32 {
    set_entries(c, true)
}

// ---------------------------------------------------------------------------------------------
// objects' descriptors (GetNamedSecurityInfo / GetSecurityInfo / SetNamedSecurityInfo)

const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;

/// SECURITY_INFORMATION bits
const OWNER_SECURITY_INFORMATION: u32 = 1;
const GROUP_SECURITY_INFORMATION: u32 = 2;
const DACL_SECURITY_INFORMATION: u32 = 4;
const SACL_SECURITY_INFORMATION: u32 = 8;

/// The DACL every object of the machine has, inherited from its drive as on a Windows data
/// drive: Administrators and SYSTEM full control, Authenticated Users modify, Users read and
/// execute (a folder's ACEs also inherit to what it holds). The process's user (an elevated
/// administrator) may read, write and delete.
fn object_dacl(dir: bool) -> Vec<(u8, u8, u32, Sid)> {
    let f = 0x10 | if dir { 0x03 } else { 0 }; // INHERITED_ACE (| OBJECT_INHERIT | CONTAINER_INHERIT)
    vec![
        (0, f, 0x001f_01ff, admins()),
        (0, f, 0x001f_01ff, Sid(5, vec![18])),
        (0, f, 0x0013_01bf, Sid(5, vec![11])),
        (0, f, 0x0012_00a9, Sid(5, vec![32, 545])),
    ]
}

/// A self-relative descriptor with the parts `info` asks for: header, owner, group, DACL (the
/// order Windows lays them out in); a SACL asked for is present and empty
fn object_sd(info: u32, dir: bool) -> Vec<u8> {
    let mut v = vec![0u8; 20];
    v[0] = 1;
    let mut ctl: u16 = 0x8000; // SE_SELF_RELATIVE
    let off = |v: &mut Vec<u8>, at: usize, b: &[u8]| {
        let o = v.len() as u32;
        v[at..at + 4].copy_from_slice(&o.to_le_bytes());
        v.extend_from_slice(b);
    };
    if info & OWNER_SECURITY_INFORMATION != 0 {
        off(&mut v, 4, &admins().bytes());
    }
    if info & GROUP_SECURITY_INFORMATION != 0 {
        off(&mut v, 8, &the_user().bytes());
    }
    if info & SACL_SECURITY_INFORMATION != 0 {
        ctl |= 0x10 | 0x800; // SE_SACL_PRESENT | SE_SACL_AUTO_INHERITED
        off(&mut v, 12, &[2, 0, 8, 0, 0, 0, 0, 0]);
    }
    if info & DACL_SECURITY_INFORMATION != 0 {
        ctl |= 0x4 | 0x400; // SE_DACL_PRESENT | SE_DACL_AUTO_INHERITED
        let aces = object_dacl(dir);
        let size = 8 + aces.iter().map(|a| 8 + a.3.len()).sum::<u32>();
        let mut acl = vec![2, 0];
        acl.extend_from_slice(&(size as u16).to_le_bytes());
        acl.extend_from_slice(&(aces.len() as u16).to_le_bytes());
        acl.extend_from_slice(&[0, 0]);
        for (ty, fl, mask, sid) in &aces {
            acl.extend_from_slice(&[*ty, *fl]);
            acl.extend_from_slice(&((8 + sid.len()) as u16).to_le_bytes());
            acl.extend_from_slice(&mask.to_le_bytes());
            acl.extend_from_slice(&sid.bytes());
        }
        off(&mut v, 16, &acl);
    }
    v[2..4].copy_from_slice(&ctl.to_le_bytes());
    v
}

/// The descriptor (LocalAlloc'd) and the pointers into it the caller asked for; a Win32 error
fn give_sd(info: u32, dir: bool, owner: u32, group: u32, dacl: u32, sacl: u32, sdp: u32) -> u32 {
    if info & SACL_SECURITY_INFORMATION != 0 && !enabled(8) {
        return ERROR_PRIVILEGE_NOT_HELD;
    }
    let b = object_sd(info, dir);
    let sd = crate::k32::global_alloc(0x40, b.len() as u32);
    if sd == 0 {
        return 8;
    }
    mem::write(sd, &b);
    let part = |i: u32| {
        let o = r32(sd + 4 + 4 * i);
        if o == 0 { 0 } else { sd + o }
    };
    for (want, p, i) in [(OWNER_SECURITY_INFORMATION, owner, 0), (GROUP_SECURITY_INFORMATION, group, 1), (SACL_SECURITY_INFORMATION, sacl, 2), (DACL_SECURITY_INFORMATION, dacl, 3)] {
        if p != 0 && info & want != 0 {
            mem::wp(p, part(i));
        }
    }
    if sdp != 0 {
        mem::wp(sdp, sd);
    }
    0
}

/// GetNamedSecurityInfo(name, type, info, &owner, &group, &dacl, &sacl, &sd): returns a Win32
/// error. A file or folder (SE_FILE_OBJECT) has to exist; other kinds get the same descriptor.
fn named_info(c: &mut Ctx, wide: bool) -> u32 {
    let (name, ty, info) = (c.a(0), c.a(1), c.a(2));
    let (owner, group, dacl, sacl, sdp) = (c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    if name == 0 || owner == 0 && group == 0 && dacl == 0 && sacl == 0 && sdp == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let mut dir = false;
    if ty == 1 {
        let n = crate::k32::arg_str(name, wide);
        let Some(h) = crate::files::resolve(&n) else { return crate::files::missing(&n) };
        match kura::fs::metadata(&h) {
            Ok(m) => dir = m.is_dir(),
            Err(_) => return crate::files::missing(&n),
        }
    }
    give_sd(info, dir, owner, group, dacl, sacl, sdp)
}
fn GetNamedSecurityInfoA(c: &mut Ctx) -> u32 {
    named_info(c, false)
}
fn GetNamedSecurityInfoW(c: &mut Ctx) -> u32 {
    named_info(c, true)
}
/// SetNamedSecurityInfo(name, type, info, owner, group, dacl, sacl): sake's files keep no
/// descriptors, so a file or folder that exists takes the change (its DACL stays the one every
/// object has); a SID or ACL that is not one fails as Windows' does
fn set_named(c: &mut Ctx, wide: bool) -> u32 {
    let (name, ty, info) = (c.a(0), c.a(1), c.a(2));
    let (owner, group, dacl) = (c.a(3), c.a(4), c.a(5));
    if name == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    if info & OWNER_SECURITY_INFORMATION != 0 && Sid::read(owner).is_none() || info & GROUP_SECURITY_INFORMATION != 0 && Sid::read(group).is_none() {
        return ERROR_INVALID_SID;
    }
    if info & DACL_SECURITY_INFORMATION != 0 && dacl != 0 && !((2..=4).contains(&r8(dacl)) && used(dacl) <= r16(dacl + 2) as u32) {
        return ERROR_INVALID_ACL;
    }
    if info & SACL_SECURITY_INFORMATION != 0 && !enabled(8) {
        return ERROR_PRIVILEGE_NOT_HELD;
    }
    if ty == 1 {
        let n = crate::k32::arg_str(name, wide);
        if crate::files::resolve(&n).map_or(true, |h| kura::fs::metadata(&h).is_err()) {
            return crate::files::missing(&n);
        }
    }
    0
}
fn SetNamedSecurityInfoA(c: &mut Ctx) -> u32 {
    set_named(c, false)
}
fn SetNamedSecurityInfoW(c: &mut Ctx) -> u32 {
    set_named(c, true)
}
/// GetSecurityInfo(handle, type, info, &owner, &group, &dacl, &sacl, &sd)
fn GetSecurityInfo(c: &mut Ctx) -> u32 {
    let (h, info) = (c.a(0), c.a(2));
    let (owner, group, dacl, sacl, sdp) = (c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    if h == 0 || h == u32::MAX {
        return ERROR_INVALID_HANDLE;
    }
    give_sd(info, false, owner, group, dacl, sacl, sdp)
}

// ---------------------------------------------------------------------------------------------
// accounts (LookupAccountName / LookupAccountSid)

const ERROR_NONE_MAPPED: u32 = 1332;

/// (name, domain, SID, SID_NAME_USE: 1 user, 4 alias, 5 well-known group): the names Japanese
/// Windows gives them too (the built-in accounts keep their English names there).
fn accounts() -> Vec<(&'static str, &'static str, Sid, u32)> {
    vec![
        ("SYSTEM", "NT AUTHORITY", Sid(5, vec![18]), 5),
        ("LOCAL SERVICE", "NT AUTHORITY", Sid(5, vec![19]), 5),
        ("NETWORK SERVICE", "NT AUTHORITY", Sid(5, vec![20]), 5),
        ("Authenticated Users", "NT AUTHORITY", Sid(5, vec![11]), 5),
        ("INTERACTIVE", "NT AUTHORITY", Sid(5, vec![4]), 5),
        ("Administrators", "BUILTIN", admins(), 4),
        ("Users", "BUILTIN", Sid(5, vec![32, 545]), 4),
        ("Guests", "BUILTIN", Sid(5, vec![32, 546]), 4),
        ("Everyone", "", everyone(), 5),
        ("LOCAL", "", Sid(2, vec![0]), 5),
        ("CREATOR OWNER", "", Sid(3, vec![0]), 5),
        ("user", "SAKE", the_user(), 1),
    ]
}
/// The account a name means: "name" or "DOMAIN\name", any case.
fn account_by_name(name: &str) -> Option<(&'static str, &'static str, Sid, u32)> {
    let (dom, n) = match name.rsplit_once('\\') {
        Some((d, n)) => (Some(d), n),
        None => (None, name),
    };
    accounts().into_iter().find(|a| a.0.eq_ignore_ascii_case(n) && dom.is_none_or(|d| a.1.eq_ignore_ascii_case(d)))
}
fn account_by_sid(sid: &Sid) -> Option<(&'static str, &'static str, Sid, u32)> {
    accounts().into_iter().find(|a| a.2 == *sid)
}
/// Copies `s` (and its NUL) to a `cap`-character buffer whose size goes to/from `*capp`; false
/// (with `*capp` the size needed, NUL included) when it does not fit.
fn put_name(buf: u32, capp: u32, s: &str, wide: bool) -> bool {
    let n = if wide { s.encode_utf16().count() } else { crate::text::s2a(s).len() } as u32;
    let cap = if capp == 0 { 0 } else { r32(capp) };
    if cap <= n || buf == 0 {
        if capp != 0 {
            w32(capp, n + 1);
        }
        return false;
    }
    if wide {
        mem::put_wstr(buf, n + 1, &s.encode_utf16().collect::<Vec<_>>());
    } else {
        mem::put_cstr(buf, n + 1, &crate::text::s2a(s));
    }
    w32(capp, n);
    true
}
/// LookupAccountName(system, name, sid, &cbSid, domain, &cchDomain, &use)
fn lookup_name(c: &mut Ctx, wide: bool) -> u32 {
    let name = crate::k32::arg_str(c.a(1), wide);
    let (sidp, cbp, dom, cchp, usep) = (c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let Some((_, d, sid, use_)) = account_by_name(&name) else {
        last_error(ERROR_NONE_MAPPED);
        return 0;
    };
    let b = sid.bytes();
    let sid_fits = cbp != 0 && r32(cbp) >= b.len() as u32 && sidp != 0;
    let dn = if wide { d.encode_utf16().count() } else { crate::text::s2a(d).len() } as u32;
    let dom_fits = cchp != 0 && r32(cchp) > dn && dom != 0;
    if !sid_fits || !dom_fits {
        if cbp != 0 {
            w32(cbp, b.len() as u32);
        }
        if cchp != 0 {
            w32(cchp, dn + 1);
        }
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    mem::write(sidp, &b);
    w32(cbp, b.len() as u32);
    put_name(dom, cchp, d, wide);
    if usep != 0 {
        w32(usep, use_);
    }
    1
}
fn LookupAccountNameA(c: &mut Ctx) -> u32 {
    lookup_name(c, false)
}
fn LookupAccountNameW(c: &mut Ctx) -> u32 {
    lookup_name(c, true)
}
/// LookupAccountSid(system, sid, name, &cchName, domain, &cchDomain, &use)
fn lookup_sid(c: &mut Ctx, wide: bool) -> u32 {
    let (sidp, name, cchn, dom, cchd, usep) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let Some(sid) = Sid::read(sidp) else {
        last_error(ERROR_INVALID_SID);
        return 0;
    };
    let Some((n, d, _, use_)) = account_by_sid(&sid) else {
        last_error(ERROR_NONE_MAPPED);
        return 0;
    };
    let len = |s: &str| if wide { s.encode_utf16().count() } else { crate::text::s2a(s).len() } as u32;
    let fits = |b: u32, p: u32, s: &str| b != 0 && p != 0 && r32(p) > len(s);
    if !fits(name, cchn, n) || !fits(dom, cchd, d) {
        if cchn != 0 {
            w32(cchn, len(n) + 1);
        }
        if cchd != 0 {
            w32(cchd, len(d) + 1);
        }
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    put_name(name, cchn, n, wide);
    put_name(dom, cchd, d, wide);
    if usep != 0 {
        w32(usep, use_);
    }
    1
}
fn LookupAccountSidA(c: &mut Ctx) -> u32 {
    lookup_sid(c, false)
}
fn LookupAccountSidW(c: &mut Ctx) -> u32 {
    lookup_sid(c, true)
}

pub fn init() {
    reg!("advapi32";
        GetNamedSecurityInfoA, GetNamedSecurityInfoW, GetSecurityInfo, SetNamedSecurityInfoA, SetNamedSecurityInfoW,
        LookupAccountNameA, LookupAccountNameW, LookupAccountSidA, LookupAccountSidW,
    );
    reg!("advapi32";
        SetEntriesInAclA, SetEntriesInAclW,
        AllocateAndInitializeSid, FreeSid, GetLengthSid, IsValidSid, EqualSid, CopySid, GetSidLengthRequired,
        InitializeSid, GetSidSubAuthority, GetSidSubAuthorityCount, GetSidIdentifierAuthority, CreateWellKnownSid,
        IsWellKnownSid, InitializeAcl, AddAce, AddAccessAllowedAce, AddAccessAllowedAceEx, AddAccessDeniedAce,
        AddAccessDeniedAceEx, IsValidAcl, GetAclInformation, GetAce, InitializeSecurityDescriptor,
        SetSecurityDescriptorOwner, SetSecurityDescriptorGroup, SetSecurityDescriptorDacl, SetSecurityDescriptorSacl,
        IsValidSecurityDescriptor, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetSecurityDescriptorGroup,
        GetSecurityDescriptorControl, GetSecurityDescriptorLength, AccessCheck, OpenProcessToken, OpenThreadToken,
        ImpersonateSelf, RevertToSelf, DuplicateToken, DuplicateTokenEx, LookupPrivilegeValueA, LookupPrivilegeValueW,
        LookupPrivilegeNameW, AdjustTokenPrivileges, CheckTokenMembership, GetTokenInformation,
        ConvertSidToStringSidA, ConvertSidToStringSidW,
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

    /// WELL_KNOWN_SID_TYPE numbers (winnt.h): WinServiceSid is 12, WinAnonymousSid 13 (13 was
    /// SERVICE's SID); every type 0..=28 sake answers is S-1-<authority>-... as Windows has it
    #[test]
    fn well_known_numbers() {
        let want = [(0, "S-1-0-0"), (1, "S-1-1-0"), (2, "S-1-2-0"), (3, "S-1-3-0"), (4, "S-1-3-1"), (5, "S-1-3-2"), (6, "S-1-3-3"),
            (7, "S-1-5"), (8, "S-1-5-1"), (9, "S-1-5-2"), (10, "S-1-5-3"), (11, "S-1-5-4"), (12, "S-1-5-6"), (13, "S-1-5-7"),
            (14, "S-1-5-8"), (15, "S-1-5-9"), (16, "S-1-5-10"), (17, "S-1-5-11"), (18, "S-1-5-12"), (19, "S-1-5-13"),
            (22, "S-1-5-18"), (23, "S-1-5-19"), (24, "S-1-5-20"), (25, "S-1-5-32"), (26, "S-1-5-32-544"), (27, "S-1-5-32-545"),
            (28, "S-1-5-32-546")];
        for (t, s) in want {
            assert_eq!(well_known(t).map(|x| sid_to_string(&x)).as_deref(), Some(s), "type {t}");
        }
    }

    #[test]
    fn sid_bytes() {
        assert_eq!(admins().bytes(), [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 2, 0, 0]);
        assert_eq!(everyone().len(), 12);
        assert!(member(&admins()) && member(&everyone()) && !member(&Sid(5, vec![18])));
    }

    /// What `object_sd` holds: (control, owner, group, sacl present, DACL ACEs)
    fn parse_sd(b: &[u8]) -> (u16, Option<Vec<u8>>, Option<Vec<u8>>, bool, Option<Vec<(u8, u8, u32, Vec<u8>)>>) {
        let u32at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
        let u16at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as usize;
        let sid = |o: usize| b[o..o + 8 + 4 * b[o + 1] as usize].to_vec();
        let ctl = u16at(2) as u16;
        let (ow, gr, sa, da) = (u32at(4), u32at(8), u32at(12), u32at(16));
        let dacl = (da != 0).then(|| {
            let (size, n) = (u16at(da + 2), u16at(da + 4));
            assert!(da + size <= b.len());
            let mut at = da + 8;
            let mut v = Vec::new();
            for _ in 0..n {
                let sz = u16at(at + 2);
                v.push((b[at], b[at + 1], u32at(at + 4) as u32, sid(at + 8)));
                assert_eq!(sz, 8 + v.last().unwrap().3.len());
                at += sz;
            }
            assert_eq!(at, da + size, "the ACL's size is its ACEs'");
            v
        });
        ((ctl), (ow != 0).then(|| sid(ow)), (gr != 0).then(|| sid(gr)), sa != 0, dacl)
    }

    #[test]
    fn object_descriptor_lets_the_user_write() {
        for dir in [false, true] {
            let (ctl, ow, gr, sa, dacl) = parse_sd(&object_sd(0xf, dir));
            assert_eq!(ctl, 0x8000 | 0x4 | 0x400 | 0x10 | 0x800);
            assert_eq!(ow, Some(admins().bytes()));
            assert_eq!(gr, Some(the_user().bytes()));
            assert!(sa);
            let dacl = dacl.unwrap();
            // FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE, from the ACEs of groups the token has
            let mut grant = 0;
            for (ty, fl, mask, sid) in &dacl {
                assert_eq!(*ty, 0);
                assert_eq!(*fl, if dir { 0x13 } else { 0x10 });
                let s = Sid(u64::from_be_bytes([0, 0, sid[2], sid[3], sid[4], sid[5], sid[6], sid[7]]), sid[8..].chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect());
                if member(&s) {
                    grant |= mask;
                }
            }
            assert_eq!(grant & 0x0013_01bf, 0x0013_01bf);
            assert_eq!(grant, 0x001f_01ff);
        }
    }

    #[test]
    fn account_names() {
        for (n, d, sid, u) in accounts() {
            assert_eq!(account_by_name(n).map(|a| a.2), Some(sid.clone()), "{n}");
            assert_eq!(account_by_name(&n.to_lowercase()).map(|a| a.2), Some(sid.clone()));
            assert_eq!(account_by_name(&format!("{d}\\{n}")).map(|a| a.2), Some(sid.clone()));
            assert_eq!(account_by_sid(&sid).map(|a| (a.0, a.1, a.3)), Some((n, d, u)));
        }
        assert_eq!(account_by_name("Users").map(|a| sid_to_string(&a.2)), Some("S-1-5-32-545".into()));
        assert_eq!(account_by_name("SYSTEM").map(|a| sid_to_string(&a.2)), Some("S-1-5-18".into()));
        assert_eq!(account_by_name("OTHER\\Users"), None);
        assert_eq!(account_by_name("nobody"), None);
        // one account per SID, one per name
        let a = accounts();
        for (i, x) in a.iter().enumerate() {
            for y in &a[i + 1..] {
                assert_ne!(x.2, y.2);
                assert!(!x.0.eq_ignore_ascii_case(y.0));
            }
        }
    }

    proptest! {
        /// Only the parts asked for are there, and they are well formed
        #[test]
        fn object_descriptor_parts(info in 0u32..16, dir in any::<bool>()) {
            let b = object_sd(info, dir);
            let (ctl, ow, gr, sa, dacl) = parse_sd(&b);
            prop_assert_eq!(ctl & 0x8000, 0x8000);
            prop_assert_eq!(ow.is_some(), info & 1 != 0);
            prop_assert_eq!(gr.is_some(), info & 2 != 0);
            prop_assert_eq!(dacl.is_some(), info & 4 != 0);
            prop_assert_eq!(ctl & 4 != 0, info & 4 != 0);
            prop_assert_eq!(sa, info & 8 != 0);
            prop_assert_eq!(b.len() % 4, 0);
        }
    }
}
