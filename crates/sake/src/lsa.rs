//! Accounts and the Local Security Authority as a standalone machine "SAKE" (workgroup
//! WORKGROUP) with one user, "user", sees them: LookupAccountSid/Name, GetUserName,
//! LsaOpenPolicy and its queries (LsaQueryInformationPolicy, LsaLookupNames(2), LsaLookupSids,
//! LsaLookupPrivilegeName, LsaEnumerateAccountRights, LsaGetUserName), LsaFreeMemory/LsaClose.
//!
//! The account table (`ACCOUNTS`) is pure data: `by_sid` and `by_name` are what the guest
//! entry points and the tests below use.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, cstr, r16, r32, rp, w32, wstr};
use crate::security::{the_user, Sid, PRIVILEGES};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

pub const COMPUTER: &str = "SAKE";
pub const USER: &str = "user";

const STATUS_SUCCESS: u32 = 0;
const STATUS_SOME_NOT_MAPPED: u32 = 0x0000_0107;
const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
const STATUS_NO_SUCH_PRIVILEGE: u32 = 0xC000_0060;
const STATUS_NONE_MAPPED: u32 = 0xC000_0073;

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_NONE_MAPPED: u32 = 1332;

/// SID_NAME_USE
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Use {
    User = 1,
    Domain = 3,
    Alias = 4,
    WellKnownGroup = 5,
    Label = 10,
    Unknown = 8,
}

pub struct Account {
    pub sid: Sid,
    pub name: String,
    pub domain: String,
    pub usage: Use,
}

/// The machine's domain: S-1-5-21-<the user's three numbers>
pub fn machine_sid() -> Sid {
    let u = the_user();
    Sid(5, u.1[..4].to_vec())
}

/// Every account the machine knows, as Windows names them in English and Japanese.
/// (sid, name, domain, use, other names it is found by)
fn table() -> Vec<(Account, &'static [&'static str])> {
    let a = |auth: u64, subs: &[u32], name: &str, domain: &str, usage: Use| Account { sid: Sid(auth, subs.to_vec()), name: name.into(), domain: domain.into(), usage };
    let nt = "NT AUTHORITY";
    let m = machine_sid().1;
    let local = |rid: u32| [m.as_slice(), &[rid]].concat();
    vec![
        (a(0, &[0], "NULL SID", "", Use::WellKnownGroup), &[]),
        (a(1, &[0], "Everyone", "", Use::WellKnownGroup), &[]),
        (a(2, &[0], "LOCAL", "", Use::WellKnownGroup), &[]),
        (a(2, &[1], "CONSOLE LOGON", "", Use::WellKnownGroup), &[]),
        (a(3, &[0], "CREATOR OWNER", "", Use::WellKnownGroup), &[]),
        (a(3, &[1], "CREATOR GROUP", "", Use::WellKnownGroup), &[]),
        (a(3, &[4], "OWNER RIGHTS", "", Use::WellKnownGroup), &[]),
        (a(5, &[1], "DIALUP", nt, Use::WellKnownGroup), &[]),
        (a(5, &[2], "NETWORK", nt, Use::WellKnownGroup), &[]),
        (a(5, &[3], "BATCH", nt, Use::WellKnownGroup), &[]),
        (a(5, &[4], "INTERACTIVE", nt, Use::WellKnownGroup), &[]),
        (a(5, &[6], "SERVICE", nt, Use::WellKnownGroup), &[]),
        (a(5, &[7], "ANONYMOUS LOGON", nt, Use::WellKnownGroup), &[]),
        (a(5, &[9], "ENTERPRISE DOMAIN CONTROLLERS", nt, Use::WellKnownGroup), &[]),
        (a(5, &[10], "SELF", nt, Use::WellKnownGroup), &[]),
        (a(5, &[11], "Authenticated Users", nt, Use::WellKnownGroup), &[]),
        (a(5, &[12], "RESTRICTED", nt, Use::WellKnownGroup), &[]),
        (a(5, &[13], "TERMINAL SERVER USER", nt, Use::WellKnownGroup), &[]),
        (a(5, &[14], "REMOTE INTERACTIVE LOGON", nt, Use::WellKnownGroup), &[]),
        (a(5, &[15], "This Organization", nt, Use::WellKnownGroup), &[]),
        (a(5, &[17], "IUSR", nt, Use::WellKnownGroup), &[]),
        (a(5, &[18], "SYSTEM", nt, Use::WellKnownGroup), &["LocalSystem"]),
        (a(5, &[19], "LOCAL SERVICE", nt, Use::WellKnownGroup), &["LocalService"]),
        (a(5, &[20], "NETWORK SERVICE", nt, Use::WellKnownGroup), &["NetworkService"]),
        (a(5, &[32], "BUILTIN", "BUILTIN", Use::Domain), &[]),
        (a(5, &[32, 544], "Administrators", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 545], "Users", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 546], "Guests", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 547], "Power Users", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 551], "Backup Operators", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 552], "Replicator", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 555], "Remote Desktop Users", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 556], "Network Configuration Operators", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 558], "Performance Monitor Users", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 559], "Performance Log Users", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 568], "IIS_IUSRS", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 569], "Cryptographic Operators", "BUILTIN", Use::Alias), &[]),
        (a(5, &[32, 573], "Event Log Readers", "BUILTIN", Use::Alias), &[]),
        (a(5, &[64, 10], "NTLM Authentication", nt, Use::WellKnownGroup), &[]),
        (a(5, &[64, 14], "SChannel Authentication", nt, Use::WellKnownGroup), &[]),
        (a(5, &[64, 21], "Digest Authentication", nt, Use::WellKnownGroup), &[]),
        (a(5, &[113], "Local account", nt, Use::WellKnownGroup), &[]),
        (a(5, &[114], "Local account and member of Administrators group", nt, Use::WellKnownGroup), &[]),
        (a(16, &[0], "Untrusted Mandatory Level", "Mandatory Label", Use::Label), &[]),
        (a(16, &[0x1000], "Low Mandatory Level", "Mandatory Label", Use::Label), &[]),
        (a(16, &[0x2000], "Medium Mandatory Level", "Mandatory Label", Use::Label), &[]),
        (a(16, &[0x2100], "Medium Plus Mandatory Level", "Mandatory Label", Use::Label), &[]),
        (a(16, &[0x3000], "High Mandatory Level", "Mandatory Label", Use::Label), &[]),
        (a(16, &[0x4000], "System Mandatory Level", "Mandatory Label", Use::Label), &[]),
        (a(5, &m, COMPUTER, COMPUTER, Use::Domain), &[]),
        (a(5, &local(500), "Administrator", COMPUTER, Use::User), &[]),
        (a(5, &local(501), "Guest", COMPUTER, Use::User), &[]),
        (a(5, &local(503), "DefaultAccount", COMPUTER, Use::User), &[]),
        (a(5, &local(513), "None", COMPUTER, Use::Domain), &[]),
        (a(5, &the_user().1, USER, COMPUTER, Use::User), &[]),
    ]
}

pub fn by_sid(s: &Sid) -> Option<Account> {
    table().into_iter().map(|e| e.0).find(|a| a.sid == *s)
}

/// An account by "name" or "domain\name" (case-insensitive). A name alone is looked up as an
/// account name first, then as a domain's own name (BUILTIN, SAKE).
pub fn by_name(n: &str) -> Option<Account> {
    let eq = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();
    let t = table();
    if let Some((d, n)) = n.split_once('\\') {
        if n.is_empty() {
            return t.into_iter().map(|e| e.0).find(|a| a.usage == Use::Domain && eq(&a.name, d));
        }
        return t.into_iter().find(|(a, alt)| eq(&a.domain, d) && (eq(&a.name, n) || alt.iter().any(|x| eq(x, n)))).map(|e| e.0);
    }
    if n.is_empty() {
        return None;
    }
    t.into_iter().find(|(a, alt)| eq(&a.name, n) || alt.iter().any(|x| eq(x, n))).map(|e| e.0)
}

// ---------------------------------------------------------------------------------------------
// Guest blocks: one allocation with the fixed part first, strings and SIDs after it

struct Blk {
    b: Vec<u8>,
    /// (offset of a pointer, offset it points at)
    ptrs: Vec<(usize, usize)>,
}

impl Blk {
    fn new(fixed: usize) -> Blk {
        Blk { b: vec![0; fixed], ptrs: Vec::new() }
    }
    fn add(&mut self, data: &[u8]) -> usize {
        while self.b.len() % 4 != 0 {
            self.b.push(0);
        }
        let at = self.b.len();
        self.b.extend_from_slice(data);
        at
    }
    fn ptr(&mut self, at: usize, data: &[u8]) {
        let o = self.add(data);
        self.ptrs.push((at, o));
    }
    fn u32(&mut self, at: usize, v: u32) {
        self.b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    /// A UNICODE_STRING at `at`: Length (no terminator), MaximumLength (with it), Buffer
    fn ustr(&mut self, at: usize, s: &str) {
        let w: Vec<u16> = s.encode_utf16().collect();
        let n = 2 * w.len();
        self.b[at..at + 2].copy_from_slice(&(n as u16).to_le_bytes());
        self.b[at + 2..at + 4].copy_from_slice(&(n as u16 + 2).to_le_bytes());
        let bytes: Vec<u8> = w.iter().chain(&[0]).flat_map(|c| c.to_le_bytes()).collect();
        self.ptr(at + ps(), &bytes);
    }
    fn put(self) -> u32 {
        let p = mem::heap_alloc(self.b.len().max(1) as u32, true);
        mem::write(p, &self.b);
        for (at, o) in self.ptrs {
            mem::wp(p + at as u32, p + o as u32);
        }
        p
    }
}

fn ps() -> usize {
    mem::ps() as usize
}
/// sizeof(UNICODE_STRING)
fn us() -> usize {
    2 * ps()
}

// ---------------------------------------------------------------------------------------------
// LookupAccountSid / LookupAccountName / GetUserName

fn put_name(dst: u32, cap: u32, s: &str, wide: bool) {
    if wide {
        mem::put_wstr(dst, cap, &s.encode_utf16().collect::<Vec<_>>());
    } else {
        mem::put_cstr(dst, cap, s.as_bytes());
    }
}
fn text(p: u32, wide: bool) -> String {
    if wide { String::from_utf16_lossy(&wstr(p)) } else { String::from_utf8_lossy(&cstr(p)).into_owned() }
}

/// LookupAccountSid(system, sid, name, &cchName, domain, &cchDomain, &use): both buffers must
/// hold their text and its terminator, else both sizes are set and ERROR_INSUFFICIENT_BUFFER.
fn lookup_sid(a: [u32; 6], wide: bool) -> u32 {
    let [sid, name, namelen, dom, domlen, usep] = a;
    let Some(s) = Sid::read(sid) else {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let Some(a) = by_sid(&s) else {
        last_error(ERROR_NONE_MAPPED);
        return 0;
    };
    let len = |t: &str| if wide { t.encode_utf16().count() } else { t.len() } as u32;
    let (nn, dn) = (len(&a.name), len(&a.domain));
    let (hn, hd) = (if namelen != 0 { r32(namelen) } else { 0 }, if domlen != 0 { r32(domlen) } else { 0 });
    if hn <= nn || hd <= dn || name == 0 || dom == 0 {
        if namelen != 0 {
            w32(namelen, nn + 1);
        }
        if domlen != 0 {
            w32(domlen, dn + 1);
        }
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    put_name(name, hn, &a.name, wide);
    put_name(dom, hd, &a.domain, wide);
    w32(namelen, nn);
    w32(domlen, dn);
    if usep != 0 {
        w32(usep, a.usage as u32);
    }
    1
}
fn LookupAccountSidA(c: &mut Ctx) -> u32 {
    lookup_sid([c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6)], false)
}
fn LookupAccountSidW(c: &mut Ctx) -> u32 {
    lookup_sid([c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6)], true)
}
/// LookupAccountSidLocal(sid, name, &cchName, domain, &cchDomain, &use)
fn LookupAccountSidLocalA(c: &mut Ctx) -> u32 {
    lookup_sid([c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5)], false)
}
fn LookupAccountSidLocalW(c: &mut Ctx) -> u32 {
    lookup_sid([c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5)], true)
}

/// LookupAccountName(system, name, sid, &cbSid, domain, &cchDomain, &use)
fn lookup_name(c: &mut Ctx, wide: bool) -> u32 {
    let (namep, sid, sidlen, dom, domlen, usep) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    if namep == 0 || sidlen == 0 || domlen == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let n = text(namep, wide);
    // (an empty name is the computer's domain)
    let a = if n.is_empty() { by_sid(&machine_sid()) } else { by_name(&n) };
    let Some(a) = a else {
        last_error(ERROR_NONE_MAPPED);
        return 0;
    };
    let sb = a.sid.bytes();
    let dn = if wide { a.domain.encode_utf16().count() } else { a.domain.len() } as u32;
    let (hs, hd) = (r32(sidlen), r32(domlen));
    if hs < sb.len() as u32 || hd <= dn || sid == 0 || dom == 0 && hd != 0 {
        w32(sidlen, sb.len() as u32);
        w32(domlen, dn + 1);
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    mem::write(sid, &sb);
    w32(sidlen, sb.len() as u32);
    put_name(dom, hd, &a.domain, wide);
    w32(domlen, dn);
    if usep != 0 {
        w32(usep, a.usage as u32);
    }
    1
}
fn LookupAccountNameA(c: &mut Ctx) -> u32 {
    lookup_name(c, false)
}
fn LookupAccountNameW(c: &mut Ctx) -> u32 {
    lookup_name(c, true)
}

/// GetUserName(buffer, &size): "user"; a buffer too small (or none) gets the size it needs
/// with the terminator and ERROR_INSUFFICIENT_BUFFER.
fn user_name(c: &mut Ctx, wide: bool) -> u32 {
    let (buf, sizep) = (c.a(0), c.a(1));
    if sizep == 0 {
        last_error(998); // ERROR_NOACCESS
        return 0;
    }
    let need = USER.len() as u32 + 1;
    if buf == 0 || r32(sizep) < need {
        w32(sizep, need);
        last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    put_name(buf, need, USER, wide);
    w32(sizep, need);
    1
}
fn GetUserNameA(c: &mut Ctx) -> u32 {
    user_name(c, false)
}
fn GetUserNameW(c: &mut Ctx) -> u32 {
    user_name(c, true)
}

// ---------------------------------------------------------------------------------------------
// LSA

/// Policy handles: one tag value; LsaClose takes any of them.
const POLICY: u32 = 0x4c53_4100;
fn policy_ok(h: u32) -> bool {
    h == POLICY
}

/// LsaOpenPolicy(system, attributes, access, &handle)
fn LsaOpenPolicy(c: &mut Ctx) -> u32 {
    let (attrs, out) = (c.a(1), c.a(3));
    if attrs == 0 || out == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    mem::putp(out, POLICY);
    STATUS_SUCCESS
}
fn LsaClose(c: &mut Ctx) -> u32 {
    if policy_ok(c.a(0)) { STATUS_SUCCESS } else { STATUS_INVALID_HANDLE }
}
fn LsaFreeMemory(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    STATUS_SUCCESS
}

/// The bytes LsaQueryInformationPolicy returns for a class, or None (STATUS_INVALID_PARAMETER)
fn policy_info(class: u32) -> Option<Blk> {
    let p = ps();
    Some(match class {
        // PolicyAuditEventsInformation: auditing off, 9 categories with no options
        2 => {
            let mut b = Blk::new(3 * p);
            b.ptr(p, &[0; 9 * 4]);
            b.u32(2 * p, 9);
            b
        }
        // PolicyPrimaryDomainInformation: a workgroup, no SID
        3 | 6 => {
            let mut b = Blk::new(us() + p);
            b.ustr(0, "WORKGROUP");
            b
        }
        // PolicyAccountDomainInformation: the machine
        5 => {
            let mut b = Blk::new(us() + p);
            b.ustr(0, COMPUTER);
            b.ptr(us(), &machine_sid().bytes());
            b
        }
        // PolicyDnsDomainInformation: the workgroup's name, no DNS names, GUID or SID
        12 => {
            let mut b = Blk::new(3 * us() + 16 + p);
            b.ustr(0, "WORKGROUP");
            b
        }
        _ => return None,
    })
}
/// LsaQueryInformationPolicy(policy, class, &buffer)
fn LsaQueryInformationPolicy(c: &mut Ctx) -> u32 {
    let (h, class, out) = (c.a(0), c.a(1), c.a(2));
    if !policy_ok(h) {
        return STATUS_INVALID_HANDLE;
    }
    if out == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    match policy_info(class) {
        Some(b) => {
            mem::putp(out, b.put());
            STATUS_SUCCESS
        }
        None => STATUS_INVALID_PARAMETER,
    }
}

/// LsaEnumerateAccountRights(policy, sid, &rights, &count): no account has rights assigned
/// on its own (they come through groups), so STATUS_OBJECT_NAME_NOT_FOUND with both cleared.
fn LsaEnumerateAccountRights(c: &mut Ctx) -> u32 {
    let (h, rights, count) = (c.a(0), c.a(2), c.a(3));
    if rights != 0 {
        mem::putp(rights, 0);
    }
    if count != 0 {
        w32(count, 0);
    }
    if !policy_ok(h) {
        return STATUS_INVALID_HANDLE;
    }
    STATUS_OBJECT_NAME_NOT_FOUND
}

/// The referenced domains of a lookup: their names and SIDs, in first-seen order
struct Domains(Vec<(String, Option<Sid>)>);
impl Domains {
    fn index(&mut self, a: &Account) -> u32 {
        // (an account that is a domain refers to itself; a well-known group with no domain
        // name to the empty one)
        let (name, sid) = if a.usage == Use::Domain {
            (a.name.clone(), Some(a.sid.clone()))
        } else {
            let mut d = a.sid.clone();
            let sid = if a.domain.is_empty() || d.1.is_empty() {
                None
            } else {
                d.1.pop();
                Some(d)
            };
            (a.domain.clone(), sid)
        };
        if let Some(i) = self.0.iter().position(|d| d.0 == name) {
            return i as u32;
        }
        self.0.push((name, sid));
        self.0.len() as u32 - 1
    }
    /// LSA_REFERENCED_DOMAIN_LIST {Entries, LSA_TRUST_INFORMATION *Domains} with the entries
    /// {UNICODE_STRING Name; PSID Sid} after it
    fn put(&self) -> u32 {
        let p = ps();
        let e = us() + p;
        let mut b = Blk::new(2 * p);
        b.u32(0, self.0.len() as u32);
        if !self.0.is_empty() {
            let at = b.add(&vec![0; e * self.0.len()]);
            b.ptrs.push((p, at));
            for (i, (name, sid)) in self.0.iter().enumerate() {
                b.ustr(at + i * e, name);
                if let Some(s) = sid {
                    b.ptr(at + i * e + us(), &s.bytes());
                }
            }
        }
        b.put()
    }
}

/// The names of LsaLookupNames(2)'s UNICODE_STRING array
fn names_in(p: u32, n: u32) -> Vec<String> {
    let s = us() as u32;
    (0..n)
        .map(|i| {
            let u = p + i * s;
            let len = r16(u) as u32 / 2;
            let buf = rp(u + mem::ps());
            String::from_utf16_lossy(&(0..len).map(|k| r16(buf + 2 * k)).collect::<Vec<_>>())
        })
        .collect()
}

/// LsaLookupNames2(policy, flags, count, names, &domains, &sids): LSA_TRANSLATED_SID2
/// {Use; PSID Sid; LONG DomainIndex; ULONG Flags} per name. LsaLookupNames (v1) gives
/// LSA_TRANSLATED_SID {Use; ULONG RelativeId; LONG DomainIndex} instead.
fn lookup_names(h: u32, count: u32, names: u32, domsp: u32, sidsp: u32, v2: bool) -> u32 {
    if !policy_ok(h) {
        return STATUS_INVALID_HANDLE;
    }
    if domsp == 0 || sidsp == 0 || count != 0 && names == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let p = ps();
    let found: Vec<Option<Account>> = names_in(names, count).iter().map(|n| by_name(n)).collect();
    let mut doms = Domains(Vec::new());
    let e = if v2 { if p == 8 { 24 } else { 16 } } else { 12 };
    let mut b = Blk::new(e * count as usize);
    for (i, a) in found.iter().enumerate() {
        let o = i * e;
        match a {
            Some(a) => {
                let di = doms.index(a);
                b.u32(o, a.usage as u32);
                if v2 {
                    b.ptr(o + p, &a.sid.bytes());
                    b.u32(o + 2 * p, di);
                } else {
                    b.u32(o + 4, if a.usage == Use::Domain { u32::MAX } else { *a.sid.1.last().unwrap_or(&0) });
                    b.u32(o + 8, di);
                }
            }
            None => {
                b.u32(o, Use::Unknown as u32);
                b.u32(o + if v2 { 2 * p } else { 8 }, u32::MAX);
            }
        }
    }
    mem::putp(sidsp, b.put());
    mem::putp(domsp, doms.put());
    let hits = found.iter().filter(|a| a.is_some()).count();
    if hits == 0 && count != 0 {
        STATUS_NONE_MAPPED
    } else if hits < count as usize {
        STATUS_SOME_NOT_MAPPED
    } else {
        STATUS_SUCCESS
    }
}
fn LsaLookupNames2(c: &mut Ctx) -> u32 {
    lookup_names(c.a(0), c.a(2), c.a(3), c.a(4), c.a(5), true)
}
fn LsaLookupNames(c: &mut Ctx) -> u32 {
    lookup_names(c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), false)
}

/// LsaLookupSids(policy, count, sids, &domains, &names): LSA_TRANSLATED_NAME {Use;
/// UNICODE_STRING Name; LONG DomainIndex} per SID; an unknown SID is named by its text with
/// index -1.
fn LsaLookupSids(c: &mut Ctx) -> u32 {
    let (h, count, sids, domsp, namesp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if !policy_ok(h) {
        return STATUS_INVALID_HANDLE;
    }
    if domsp == 0 || namesp == 0 || count != 0 && sids == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let p = ps();
    let e = if p == 8 { 32 } else { 16 };
    let mut doms = Domains(Vec::new());
    let mut b = Blk::new(e * count as usize);
    let mut hits = 0;
    for i in 0..count {
        let o = i as usize * e;
        let s = Sid::read(rp(sids + i * p as u32));
        match s.as_ref().and_then(by_sid) {
            Some(a) => {
                hits += 1;
                let di = doms.index(&a);
                b.u32(o, a.usage as u32);
                b.ustr(o + p, &a.name);
                b.u32(o + p + us(), di);
            }
            None => {
                b.u32(o, Use::Unknown as u32);
                b.ustr(o + p, &s.map(|s| crate::security::sid_to_string(&s)).unwrap_or_default());
                b.u32(o + p + us(), u32::MAX);
            }
        }
    }
    mem::putp(namesp, b.put());
    mem::putp(domsp, doms.put());
    if hits == 0 && count != 0 {
        STATUS_NONE_MAPPED
    } else if hits < count {
        STATUS_SOME_NOT_MAPPED
    } else {
        STATUS_SUCCESS
    }
}

/// LsaLookupPrivilegeName(policy, luid, &name): a UNICODE_STRING of the name; an unknown
/// LUID clears the pointer.
fn LsaLookupPrivilegeName(c: &mut Ctx) -> u32 {
    let (h, luid, out) = (c.a(0), c.a(1), c.a(2));
    if !policy_ok(h) {
        return STATUS_INVALID_HANDLE;
    }
    if luid == 0 || out == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let Some(p) = PRIVILEGES.iter().find(|p| p.0 == r32(luid) && r32(luid + 4) == 0) else {
        mem::putp(out, 0);
        return STATUS_NO_SUCH_PRIVILEGE;
    };
    let mut b = Blk::new(us());
    b.ustr(0, p.1);
    mem::putp(out, b.put());
    STATUS_SUCCESS
}

/// LsaGetUserName(&user, &domain): UNICODE_STRINGs (each its own allocation) of the user and
/// the computer
fn LsaGetUserName(c: &mut Ctx) -> u32 {
    let (user, dom) = (c.a(0), c.a(1));
    if user == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let one = |s: &str| {
        let mut b = Blk::new(us());
        b.ustr(0, s);
        b.put()
    };
    mem::putp(user, one(USER));
    if dom != 0 {
        mem::putp(dom, one(COMPUTER));
    }
    STATUS_SUCCESS
}

/// LsaNtStatusToWinError(status)
fn LsaNtStatusToWinError(c: &mut Ctx) -> u32 {
    crate::kern::win32(c.a(0))
}

pub fn init() {
    reg!("advapi32";
        LookupAccountSidA, LookupAccountSidW, LookupAccountSidLocalA, LookupAccountSidLocalW,
        LookupAccountNameA, LookupAccountNameW, GetUserNameA, GetUserNameW,
        LsaOpenPolicy, LsaClose, LsaFreeMemory, LsaQueryInformationPolicy, LsaEnumerateAccountRights,
        LsaLookupNames, LsaLookupNames2, LsaLookupSids, LsaLookupPrivilegeName, LsaGetUserName,
        LsaNtStatusToWinError,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn accounts_as_windows_names_them() {
        let a = by_name("LOCAL SERVICE").unwrap();
        assert_eq!((a.sid.clone(), a.domain.as_str(), a.usage), (Sid(5, vec![19]), "NT AUTHORITY", Use::WellKnownGroup));
        assert_eq!(by_name("NT AUTHORITY\\LocalService").unwrap().name, "LOCAL SERVICE");
        assert!(by_name("LOCAL SERVIC").is_none());
        assert!(by_name("").is_none());
        assert_eq!(by_name("USER").unwrap().usage, Use::User);
        assert_eq!(by_name("sake\\User").unwrap().sid, the_user());
        assert_eq!(by_name("BUILTIN").unwrap().usage, Use::Domain);
        assert_eq!(by_name("BUILTIN\\").unwrap().sid, Sid(5, vec![32]));
        assert_eq!(by_sid(&Sid(1, vec![0])).unwrap().name, "Everyone");
        assert!(by_sid(&Sid(1234, vec![5678, 1234, 5678])).is_none());
        assert_eq!(by_sid(&machine_sid()).unwrap().name, COMPUTER);
    }

    #[test]
    fn domains_of_a_lookup() {
        let mut d = Domains(Vec::new());
        assert_eq!(d.index(&by_name("Everyone").unwrap()), 0);
        assert_eq!(d.index(&by_name("LOCAL SERVICE").unwrap()), 1);
        assert_eq!(d.index(&by_name("SYSTEM").unwrap()), 1);
        assert_eq!(d.index(&by_name("user").unwrap()), 2);
        assert_eq!(d.0[2], (COMPUTER.to_string(), Some(machine_sid())));
        assert_eq!(d.0[1], ("NT AUTHORITY".to_string(), Some(Sid(5, vec![]))));
        assert_eq!(d.index(&by_name("SAKE").unwrap()), 2);
    }

    proptest! {
        /// Every account is found by its own name, by domain\name and by its SID, in any case
        #[test]
        fn table_round_trips(i in 0usize..56, upper in any::<bool>()) {
            let t = table();
            let (a, _) = &t[i % t.len()];
            let case = |s: &str| if upper { s.to_uppercase() } else { s.to_lowercase() };
            let b = by_sid(&a.sid).unwrap();
            prop_assert_eq!(&b.name, &a.name);
            // (names are unique except where a domain's own entry shares its name)
            let n = by_name(&case(&a.name)).unwrap();
            prop_assert!(n.name == a.name);
            if !a.domain.is_empty() && a.usage != Use::Domain {
                let q = by_name(&format!("{}\\{}", case(&a.domain), case(&a.name))).unwrap();
                prop_assert_eq!(q.sid, a.sid.clone());
            }
        }
        /// Looking up any text never panics
        #[test]
        fn any_name(t in "\\PC{0,24}") {
            let _ = by_name(&t);
        }
    }

    #[test]
    fn sids_unique() {
        let t = table();
        for (i, a) in t.iter().enumerate() {
            for b in &t[i + 1..] {
                assert_ne!(a.0.sid, b.0.sid, "{} {}", a.0.name, b.0.name);
            }
        }
    }
}
