#![allow(non_snake_case)]
//! ws2_32's side that needs no socket: byte order, address text (on top of netaddr's Rtl*
//! parsers), the host's own name, the protocol and service databases, name resolution
//! (getaddrinfo, gethostbyname, getnameinfo, ...) and the provider catalog.
//!
//! There is no network: names resolve only when they are IP literals, "localhost", or this
//! machine's name (which has only its loopback addresses), and everything else is
//! WSAHOST_NOT_FOUND, as on a Windows machine without a connection.

use crate::api::{implement, Ctx};
use crate::mem::{self, ps, r16, r32, w16, w32, w8, wp};
use crate::netaddr;
use crate::ws2::{err, last, started, SOCKET_ERROR};
use crate::HashMap;
use std::sync::Mutex;

pub const AF_UNSPEC: u32 = 0;
pub const AF_INET: u32 = 2;
pub const AF_INET6: u32 = 23;

pub const WSAEFAULT: u32 = 10014;
pub const WSAEINVAL: u32 = 10022;
pub const WSAENOBUFS: u32 = 10055;
pub const WSAEAFNOSUPPORT: u32 = 10047;
pub const WSAESOCKTNOSUPPORT: u32 = 10044;
pub const WSANOTINITIALISED: u32 = 10093;
pub const WSATYPE_NOT_FOUND: u32 = 10109;
pub const WSAHOST_NOT_FOUND: u32 = 11001;
pub const WSANO_DATA: u32 = 11004;
pub const WSANO_RECOVERY: u32 = 11003;
pub const WSA_E_NO_MORE: u32 = 10110;
pub const WSASERVICE_NOT_FOUND: u32 = 10108;

/// This machine's DNS name (GetComputerName's)
pub const HOSTNAME: &str = "SAKE";

/// A socket address: 4 or 16 address bytes, the port in host order, the IPv6 flow and scope
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addr {
    pub ip: Vec<u8>,
    pub port: u16,
    pub flow: u32,
    pub scope: u32,
}

impl Addr {
    pub fn v4(ip: [u8; 4], port: u16) -> Addr {
        Addr {
            ip: ip.to_vec(),
            port,
            flow: 0,
            scope: 0,
        }
    }
    pub fn v6(ip: [u8; 16], port: u16) -> Addr {
        Addr {
            ip: ip.to_vec(),
            port,
            flow: 0,
            scope: 0,
        }
    }
    pub fn is_v6(&self) -> bool {
        self.ip.len() == 16
    }
    pub fn family(&self) -> u32 {
        if self.is_v6() {
            AF_INET6
        } else {
            AF_INET
        }
    }
    pub fn size(&self) -> u32 {
        if self.is_v6() {
            28
        } else {
            16
        }
    }
    pub fn any(&self) -> bool {
        self.ip.iter().all(|&b| b == 0)
    }
    /// the IPv4 address inside an IPv4-mapped IPv6 one (::ffff:a.b.c.d), or of an IPv4 one
    pub fn as_v4(&self) -> Option<[u8; 4]> {
        if !self.is_v6() {
            return Some(self.ip[..4].try_into().unwrap());
        }
        (self.ip[..10].iter().all(|&b| b == 0) && self.ip[10] == 0xff && self.ip[11] == 0xff)
            .then(|| self.ip[12..].try_into().unwrap())
    }
    pub fn mapped(v4: [u8; 4], port: u16) -> Addr {
        let mut ip = [0u8; 16];
        ip[10] = 0xff;
        ip[11] = 0xff;
        ip[12..].copy_from_slice(&v4);
        Addr::v6(ip, port)
    }
    pub fn loopback(&self) -> bool {
        match self.as_v4() {
            Some(a) => a[0] == 127,
            None => self.ip[..15].iter().all(|&b| b == 0) && self.ip[15] == 1,
        }
    }
    /// SOCKADDR_IN / SOCKADDR_IN6 bytes
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = vec![0u8; self.size() as usize];
        v[..2].copy_from_slice(&(self.family() as u16).to_le_bytes());
        v[2..4].copy_from_slice(&self.port.to_be_bytes());
        if self.is_v6() {
            v[4..8].copy_from_slice(&self.flow.to_le_bytes());
            v[8..24].copy_from_slice(&self.ip);
            v[24..28].copy_from_slice(&self.scope.to_le_bytes());
        } else {
            v[4..8].copy_from_slice(&self.ip);
        }
        v
    }
    pub fn text(&self) -> String {
        if self.is_v6() {
            netaddr::v6_text(
                self.ip.as_slice().try_into().unwrap(),
                self.scope,
                self.port.to_be(),
            )
        } else {
            netaddr::v4_text(self.ip.as_slice().try_into().unwrap(), self.port.to_be())
        }
    }
}

/// Reads a sockaddr of `len` bytes; Err(WSAEFAULT) when too short, Err(WSAEAFNOSUPPORT) for
/// another family
pub fn read_sa(p: u32, len: u32) -> Result<Addr, u32> {
    if p == 0 || len < 2 {
        return Err(WSAEFAULT);
    }
    match r16(p) as u32 {
        AF_INET => {
            if len < 16 {
                return Err(WSAEFAULT);
            }
            Ok(Addr::v4(
                mem::slice(p + 4, 4).try_into().unwrap(),
                u16::from_be(r16(p + 2)),
            ))
        }
        AF_INET6 => {
            if len < 24 {
                return Err(WSAEFAULT);
            }
            let scope = if len >= 28 { r32(p + 24) } else { 0 };
            Ok(Addr {
                ip: mem::slice(p + 8, 16).to_vec(),
                port: u16::from_be(r16(p + 2)),
                flow: r32(p + 4),
                scope,
            })
        }
        _ => Err(WSAEAFNOSUPPORT),
    }
}

/// Writes `a` to a sockaddr buffer whose size is at `lenp` (which gets the size written);
/// Err(WSAEFAULT) if it does not fit
pub fn write_sa(p: u32, lenp: u32, a: &Addr) -> Result<(), u32> {
    if p == 0 || lenp == 0 || (r32(lenp) as i32) < a.size() as i32 {
        return Err(WSAEFAULT);
    }
    mem::write(p, &a.bytes());
    w32(lenp, a.size());
    Ok(())
}

// ---- byte order ----

fn htonl(c: &mut Ctx) -> u32 {
    c.a(0).swap_bytes()
}
fn htons(c: &mut Ctx) -> u32 {
    (c.a(0) as u16).swap_bytes() as u32
}
/// WSAHtonl(s, v, &out) and friends
fn wsa_order(c: &mut Ctx, wide: bool) -> u32 {
    let (s, v, out) = (c.a(0), c.a(1), c.a(2));
    if !started() {
        return err(WSANOTINITIALISED);
    }
    if !crate::ws2::is_socket(s) {
        return err(crate::ws2::WSAENOTSOCK);
    }
    if out == 0 {
        return err(WSAEFAULT);
    }
    if wide {
        w32(out, v.swap_bytes());
    } else {
        w16(out, (v as u16).swap_bytes());
    }
    0
}

// ---- address text ----

fn astr(p: u32) -> Vec<u16> {
    mem::cstr(p).iter().map(|&b| b as u16).collect()
}

/// inet_addr: an IPv4 address in any of the old forms ("a", "a.b", "a.b.c", octal, hex), ended by
/// the end or a space; INADDR_NONE otherwise
fn inet_addr(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        last(WSAEFAULT);
        return u32::MAX;
    }
    let s = astr(p);
    if s == [b' ' as u16] {
        return 0;
    }
    let r = netaddr::v4_parse(&s, false, true, false);
    // (the text may end in any isspace() character)
    match (r.ok, s.get(r.end).copied().unwrap_or(0)) {
        (true, 0 | 9..=13 | 0x20) => u32::from_le_bytes(r.addr.unwrap()),
        _ => u32::MAX,
    }
}

/// a per-thread buffer that a function returns (inet_ntoa's, hostent, ...), `size` bytes
fn thread_buf(kind: u8, size: u32) -> u32 {
    static BUFS: Mutex<Option<HashMap<(u32, u8), u32>>> = Mutex::new(None);
    let tid = crate::cur().tid;
    let mut g = BUFS.lock().unwrap();
    *g.get_or_insert_with(HashMap::default)
        .entry((tid, kind))
        .or_insert_with(|| mem::heap_alloc(size, true))
}

fn inet_ntoa(c: &mut Ctx) -> u32 {
    let b = thread_buf(0, 16);
    let s = netaddr::v4_text(c.a(0).to_le_bytes(), 0);
    netaddr::put_raw(&s, b, false);
    b
}

/// inet_pton(family, text, out): 1, 0 when the text is not an address (the output may still
/// get what was parsed), -1 for a bad family or pointer
/// InetPtonW (not inet_pton) sets WSAEINVAL for text that isn't an address
fn pton_ret(ok: bool, wide: bool) -> u32 {
    if !ok && wide {
        last(WSAEINVAL);
    }
    ok as u32
}
fn pton(c: &mut Ctx, wide: bool) -> u32 {
    let (af, p, out) = (c.a(0), c.a(1), c.a(2));
    if p == 0 || out == 0 {
        return err(WSAEFAULT);
    }
    let s = if wide { mem::wstr(p) } else { astr(p) };
    match af {
        AF_INET => {
            let r = netaddr::v4_parse(&s, true, true, false);
            if r.ok {
                mem::write(out, &r.addr.unwrap());
            }
            pton_ret(r.ok && s.get(r.end).is_none(), wide)
        }
        AF_INET6 => {
            let mut a: [u8; 16] = mem::slice(out, 16).try_into().unwrap();
            let r = netaddr::v6_parse(&s, &mut a, false, true);
            mem::write(out, &a);
            pton_ret(r.ok && s.get(r.end).is_none(), wide)
        }
        _ => err(WSAEAFNOSUPPORT),
    }
}

/// inet_ntop(family, addr, buf, size): buf, or NULL (ERROR_INVALID_PARAMETER) when it is too small
fn ntop(c: &mut Ctx, wide: bool) -> u32 {
    let (af, a, buf, size) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if a == 0 || buf == 0 {
        last(0xC000000D); // (STATUS_INVALID_PARAMETER, as Windows leaves it)
        return 0;
    }
    let s = match af {
        AF_INET => netaddr::v4_text(mem::slice(a, 4).try_into().unwrap(), 0),
        AF_INET6 => netaddr::v6_text(mem::slice(a, 16).try_into().unwrap(), 0, 0),
        _ => {
            last(WSAEAFNOSUPPORT);
            return 0;
        }
    };
    if (size as usize) < s.len() + 1 {
        last(0xC000000D);
        return 0;
    }
    netaddr::put_raw(&s, buf, wide);
    buf
}

/// WSAStringToAddress(text, family, protocolInfo, sockaddr, &len)
fn string_to_address(c: &mut Ctx, wide: bool) -> u32 {
    let (p, af, sa, lenp) = (c.a(0), c.a(1), c.a(3), c.a(4));
    if !started() {
        return err(WSANOTINITIALISED);
    }
    if p == 0 || lenp == 0 {
        return err(WSAEINVAL);
    }
    let s = if wide { mem::wstr(p) } else { astr(p) };
    let a = match af {
        AF_INET => {
            if (r32(lenp) as i32) < 16 {
                w32(lenp, 16);
                return err(WSAEFAULT);
            }
            let r = netaddr::v4_parse(&s, false, false, true);
            if !r.ok {
                // (a failure still zeroes the sockaddr and leaves what was parsed of the address)
                if sa != 0 {
                    mem::write(sa, &[0; 16]);
                    if let Some(a) = r.addr {
                        mem::write(sa + 4, &a);
                    }
                }
                return err(WSAEINVAL);
            }
            Addr::v4(r.addr.unwrap(), u16::from_be(r.port))
        }
        AF_INET6 => {
            if (r32(lenp) as i32) < 28 {
                w32(lenp, 28);
                return err(WSAEFAULT);
            }
            let mut ip = [0u8; 16];
            let r = netaddr::v6_parse(&s, &mut ip, true, false);
            if !r.ok {
                if sa != 0 {
                    mem::write(sa, &[0; 28]);
                    mem::write(sa + 8, &ip);
                }
                return err(WSAEINVAL);
            }
            Addr {
                ip: ip.to_vec(),
                port: u16::from_be(r.port),
                flow: 0,
                scope: r.scope,
            }
        }
        _ => return err(WSAEINVAL),
    };
    if sa == 0 {
        return err(WSAEFAULT);
    }
    mem::write(sa, &vec![0; a.size() as usize]);
    mem::write(sa, &a.bytes());
    w32(lenp, a.size());
    0
}

/// WSAAddressToString(sockaddr, len, protocolInfo, buf, &bufLen): "a.b.c.d[:port]",
/// "[v6%scope]:port"
fn address_to_string(c: &mut Ctx, wide: bool) -> u32 {
    let (sa, len, buf, lenp) = (c.a(0), c.a(1), c.a(3), c.a(4));
    if !started() {
        return err(WSANOTINITIALISED);
    }
    if sa == 0 || lenp == 0 {
        return err(WSAEFAULT);
    }
    let a = match read_sa(sa, len) {
        Ok(a) => a,
        Err(WSAEAFNOSUPPORT) => return err(WSAEINVAL),
        Err(_) => return err(WSAEFAULT),
    };
    let s = a.text();
    let need = s.len() as u32 + 1;
    if buf == 0 || r32(lenp) < need {
        w32(lenp, need);
        return err(WSAEFAULT);
    }
    netaddr::put_raw(&s, buf, wide);
    w32(lenp, need);
    0
}

// ---- databases ----

/// (number, name, alias) as Windows' protocol file has them
const PROTOCOLS: &[(u32, &str, &str)] = &[
    (0, "ip", "IP"),
    (1, "icmp", "ICMP"),
    (3, "ggp", "GGP"),
    (6, "tcp", "TCP"),
    (8, "egp", "EGP"),
    (12, "pup", "PUP"),
    (17, "udp", "UDP"),
    (20, "hmp", "HMP"),
    (22, "xns-idp", "XNS-IDP"),
    (27, "rdp", "RDP"),
    (41, "ipv6", "IPv6"),
    (43, "ipv6-route", "IPv6-Route"),
    (44, "ipv6-frag", "IPv6-Frag"),
    (50, "esp", "ESP"),
    (51, "ah", "AH"),
    (58, "ipv6-icmp", "IPv6-ICMP"),
    (59, "ipv6-nonxt", "IPv6-NoNxt"),
    (60, "ipv6-opts", "IPv6-Opts"),
    (66, "rvd", "RVD"),
];

/// (name, port, protocol, aliases): a part of Windows' services file
const SERVICES: &[(&str, u16, &str, &[&str])] = &[
    ("echo", 7, "tcp", &[]),
    ("echo", 7, "udp", &[]),
    ("discard", 9, "tcp", &["sink", "null"]),
    ("discard", 9, "udp", &["sink", "null"]),
    ("systat", 11, "tcp", &["users"]),
    ("systat", 11, "udp", &["users"]),
    ("daytime", 13, "tcp", &[]),
    ("daytime", 13, "udp", &[]),
    ("qotd", 17, "tcp", &["quote"]),
    ("qotd", 17, "udp", &["quote"]),
    ("chargen", 19, "tcp", &["ttytst", "source"]),
    ("chargen", 19, "udp", &["ttytst", "source"]),
    ("ftp-data", 20, "tcp", &[]),
    ("ftp", 21, "tcp", &[]),
    ("ssh", 22, "tcp", &[]),
    ("telnet", 23, "tcp", &[]),
    ("smtp", 25, "tcp", &["mail"]),
    ("time", 37, "tcp", &["timserver"]),
    ("time", 37, "udp", &["timserver"]),
    ("rlp", 39, "udp", &["resource"]),
    ("nameserver", 42, "tcp", &["name"]),
    ("nameserver", 42, "udp", &["name"]),
    ("nicname", 43, "tcp", &["whois"]),
    ("domain", 53, "tcp", &[]),
    ("domain", 53, "udp", &[]),
    ("bootps", 67, "udp", &["dhcps"]),
    ("bootpc", 68, "udp", &["dhcpc"]),
    ("tftp", 69, "udp", &[]),
    ("gopher", 70, "tcp", &[]),
    ("finger", 79, "tcp", &[]),
    ("http", 80, "tcp", &["www", "www-http"]),
    ("hosts2-ns", 81, "tcp", &[]),
    ("hosts2-ns", 81, "udp", &[]),
    ("kerberos", 88, "tcp", &["krb5", "kerberos-sec"]),
    ("kerberos", 88, "udp", &["krb5", "kerberos-sec"]),
    ("hostname", 101, "tcp", &["hostnames"]),
    ("iso-tsap", 102, "tcp", &[]),
    ("rtelnet", 107, "tcp", &[]),
    ("pop2", 109, "tcp", &["postoffice"]),
    ("pop3", 110, "tcp", &[]),
    ("sunrpc", 111, "tcp", &["rpcbind", "portmap"]),
    ("sunrpc", 111, "udp", &["rpcbind", "portmap"]),
    ("auth", 113, "tcp", &["ident", "tap"]),
    ("uucp-path", 117, "tcp", &[]),
    ("sqlserv", 118, "tcp", &[]),
    ("nntp", 119, "tcp", &["usenet"]),
    ("ntp", 123, "udp", &[]),
    ("epmap", 135, "tcp", &["loc-srv"]),
    ("epmap", 135, "udp", &["loc-srv"]),
    ("netbios-ns", 137, "tcp", &["nbname"]),
    ("netbios-ns", 137, "udp", &["nbname"]),
    ("netbios-dgm", 138, "udp", &["nbdatagram"]),
    ("netbios-ssn", 139, "tcp", &["nbsession"]),
    ("imap", 143, "tcp", &["imap4"]),
    ("sql-net", 150, "tcp", &[]),
    ("sqlsrv", 156, "tcp", &[]),
    ("pcmail-srv", 158, "tcp", &[]),
    ("snmp", 161, "udp", &[]),
    ("snmptrap", 162, "udp", &["snmp-trap"]),
    ("print-srv", 170, "tcp", &[]),
    ("bgp", 179, "tcp", &[]),
    ("irc", 194, "tcp", &[]),
    ("ipx", 213, "udp", &[]),
    ("rtsps", 322, "tcp", &[]),
    ("rtsps", 322, "udp", &[]),
    ("mftp", 349, "tcp", &[]),
    ("mftp", 349, "udp", &[]),
    ("ldap", 389, "tcp", &[]),
    ("https", 443, "tcp", &["MCom"]),
    ("https", 443, "udp", &["MCom"]),
    ("microsoft-ds", 445, "tcp", &[]),
    ("microsoft-ds", 445, "udp", &[]),
    ("kpasswd", 464, "tcp", &["kpwd"]),
    ("kpasswd", 464, "udp", &["kpwd"]),
    ("isakmp", 500, "udp", &["ike"]),
    ("crs", 507, "tcp", &[]),
    ("crs", 507, "udp", &[]),
    ("exec", 512, "tcp", &[]),
    ("biff", 512, "udp", &["comsat"]),
    ("login", 513, "tcp", &[]),
    ("who", 513, "udp", &["whod"]),
    ("cmd", 514, "tcp", &["shell"]),
    ("syslog", 514, "udp", &[]),
    ("printer", 515, "tcp", &["spooler"]),
    ("talk", 517, "udp", &[]),
    ("ntalk", 518, "udp", &[]),
    ("efs", 520, "tcp", &[]),
    ("router", 520, "udp", &["route", "routed"]),
    ("ulp", 522, "tcp", &[]),
    ("ulp", 522, "udp", &[]),
    ("timed", 525, "udp", &["timeserver"]),
    ("tempo", 526, "tcp", &["newdate"]),
    ("irc-serv", 529, "tcp", &[]),
    ("irc-serv", 529, "udp", &[]),
    ("courier", 530, "tcp", &["rpc"]),
    ("conference", 531, "tcp", &["chat"]),
    ("netnews", 532, "tcp", &["readnews"]),
    ("netwall", 533, "udp", &[]),
    ("uucp", 540, "tcp", &["uucpd"]),
    ("klogin", 543, "tcp", &[]),
    ("kshell", 544, "tcp", &["krcmd"]),
    ("dhcpv6-client", 546, "tcp", &[]),
    ("dhcpv6-client", 546, "udp", &[]),
    ("dhcpv6-server", 547, "tcp", &[]),
    ("dhcpv6-server", 547, "udp", &[]),
    ("afpovertcp", 548, "tcp", &[]),
    ("afpovertcp", 548, "udp", &[]),
    ("new-rwho", 550, "udp", &["new-who"]),
    ("rtsp", 554, "tcp", &[]),
    ("rtsp", 554, "udp", &[]),
    ("remotefs", 556, "tcp", &["rfs", "rfs_server"]),
    ("rmonitor", 560, "udp", &["rmonitord"]),
    ("monitor", 561, "udp", &[]),
    ("nntps", 563, "tcp", &["snntp"]),
    ("nntps", 563, "udp", &["snntp"]),
    ("whoami", 565, "tcp", &[]),
    ("whoami", 565, "udp", &[]),
    ("ms-shuttle", 568, "tcp", &[]),
    ("ms-shuttle", 568, "udp", &[]),
    ("ms-rome", 569, "tcp", &[]),
    ("ms-rome", 569, "udp", &[]),
    ("http-rpc-epmap", 593, "tcp", &[]),
    ("http-rpc-epmap", 593, "udp", &[]),
    ("hmmp-ind", 612, "tcp", &[]),
    ("hmmp-ind", 612, "udp", &[]),
    ("hmmp-op", 613, "tcp", &[]),
    ("hmmp-op", 613, "udp", &[]),
    ("ldaps", 636, "tcp", &["sldap"]),
    ("doom", 666, "tcp", &[]),
    ("doom", 666, "udp", &[]),
    ("msexch-routing", 691, "tcp", &[]),
    ("msexch-routing", 691, "udp", &[]),
    ("kerberos-adm", 749, "tcp", &[]),
    ("kerberos-adm", 749, "udp", &[]),
    ("kerberos-iv", 750, "udp", &[]),
    ("mdbs_daemon", 800, "tcp", &[]),
    ("mdbs_daemon", 800, "udp", &[]),
    ("ftps-data", 989, "tcp", &[]),
    ("ftps", 990, "tcp", &[]),
    ("telnets", 992, "tcp", &[]),
    ("imaps", 993, "tcp", &[]),
    ("ircs", 994, "tcp", &[]),
    ("pop3s", 995, "tcp", &["spop3"]),
    ("pop3s", 995, "udp", &["spop3"]),
    ("kpop", 1109, "tcp", &[]),
    ("nfsd-status", 1110, "tcp", &[]),
    ("nfsd-keepalive", 1110, "udp", &[]),
    ("nfa", 1155, "tcp", &[]),
    ("nfa", 1155, "udp", &[]),
    ("activesync", 1034, "tcp", &[]),
    ("opsmgr", 1270, "tcp", &[]),
    ("opsmgr", 1270, "udp", &[]),
    ("ms-sql-s", 1433, "tcp", &[]),
    ("ms-sql-s", 1433, "udp", &[]),
    ("ms-sql-m", 1434, "tcp", &[]),
    ("ms-sql-m", 1434, "udp", &[]),
    ("ms-sna-server", 1477, "tcp", &[]),
    ("ms-sna-server", 1477, "udp", &[]),
    ("ms-sna-base", 1478, "tcp", &[]),
    ("ms-sna-base", 1478, "udp", &[]),
    ("wins", 1512, "tcp", &[]),
    ("wins", 1512, "udp", &[]),
    ("ingreslock", 1524, "tcp", &["ingres"]),
    ("stt", 1607, "tcp", &[]),
    ("stt", 1607, "udp", &[]),
    ("l2tp", 1701, "udp", &[]),
    ("pptconference", 1711, "tcp", &[]),
    ("pptconference", 1711, "udp", &[]),
    ("pptp", 1723, "tcp", &[]),
    ("msiccp", 1731, "tcp", &[]),
    ("msiccp", 1731, "udp", &[]),
    ("remote-winsock", 1745, "tcp", &[]),
    ("remote-winsock", 1745, "udp", &[]),
    ("ms-streaming", 1755, "tcp", &[]),
    ("ms-streaming", 1755, "udp", &[]),
    ("msmq", 1801, "tcp", &[]),
    ("msmq", 1801, "udp", &[]),
    ("radius", 1812, "udp", &[]),
    ("radacct", 1813, "udp", &[]),
    ("msnp", 1863, "tcp", &[]),
    ("msnp", 1863, "udp", &[]),
    ("ssdp", 1900, "tcp", &[]),
    ("ssdp", 1900, "udp", &[]),
    ("close-combat", 1944, "tcp", &[]),
    ("close-combat", 1944, "udp", &[]),
    ("nfsd", 2049, "udp", &["nfs"]),
    ("knetd", 2053, "tcp", &[]),
    ("mzap", 2106, "tcp", &[]),
    ("mzap", 2106, "udp", &[]),
    ("qwave", 2177, "tcp", &[]),
    ("qwave", 2177, "udp", &[]),
    ("directplay", 2234, "tcp", &[]),
    ("directplay", 2234, "udp", &[]),
    ("ms-olap3", 2382, "tcp", &[]),
    ("ms-olap3", 2382, "udp", &[]),
    ("ms-olap4", 2383, "tcp", &[]),
    ("ms-olap4", 2383, "udp", &[]),
    ("ms-olap1", 2393, "tcp", &[]),
    ("ms-olap1", 2393, "udp", &[]),
    ("ms-olap2", 2394, "tcp", &[]),
    ("ms-olap2", 2394, "udp", &[]),
    ("ms-theater", 2460, "tcp", &[]),
    ("ms-theater", 2460, "udp", &[]),
    ("wlbs", 2504, "tcp", &[]),
    ("wlbs", 2504, "udp", &[]),
    ("ms-v-worlds", 2525, "tcp", &[]),
    ("ms-v-worlds", 2525, "udp", &[]),
    ("sms-rcinfo", 2701, "tcp", &[]),
    ("sms-rcinfo", 2701, "udp", &[]),
    ("sms-xfer", 2702, "tcp", &[]),
    ("sms-xfer", 2702, "udp", &[]),
    ("sms-chat", 2703, "tcp", &[]),
    ("sms-chat", 2703, "udp", &[]),
    ("sms-remctrl", 2704, "tcp", &[]),
    ("sms-remctrl", 2704, "udp", &[]),
    ("msolap-ptp2", 2725, "tcp", &[]),
    ("msolap-ptp2", 2725, "udp", &[]),
    ("icslap", 2869, "tcp", &[]),
    ("icslap", 2869, "udp", &[]),
    ("cifs", 3020, "tcp", &[]),
    ("cifs", 3020, "udp", &[]),
    ("xbox", 3074, "tcp", &[]),
    ("xbox", 3074, "udp", &[]),
    ("ms-dotnetster", 3126, "tcp", &[]),
    ("ms-dotnetster", 3126, "udp", &[]),
    ("ms-rule-engine", 3132, "tcp", &[]),
    ("ms-rule-engine", 3132, "udp", &[]),
    ("msft-gc", 3268, "tcp", &[]),
    ("msft-gc", 3268, "udp", &[]),
    ("msft-gc-ssl", 3269, "tcp", &[]),
    ("msft-gc-ssl", 3269, "udp", &[]),
    ("ms-cluster-net", 3343, "tcp", &[]),
    ("ms-cluster-net", 3343, "udp", &[]),
    ("ms-wbt-server", 3389, "tcp", &[]),
    ("ms-wbt-server", 3389, "udp", &[]),
    ("ms-la", 3535, "tcp", &[]),
    ("ms-la", 3535, "udp", &[]),
    ("pnrp-port", 3540, "tcp", &[]),
    ("pnrp-port", 3540, "udp", &[]),
    ("teredo", 3544, "tcp", &[]),
    ("teredo", 3544, "udp", &[]),
    ("p2pgroup", 3587, "tcp", &[]),
    ("p2pgroup", 3587, "udp", &[]),
    ("ws-discovery", 3702, "udp", &[]),
    ("dvcprov-port", 3776, "tcp", &[]),
    ("dvcprov-port", 3776, "udp", &[]),
    ("msfw-control", 3847, "tcp", &[]),
    ("msdts1", 3882, "tcp", &[]),
    ("sdp-portmapper", 3935, "tcp", &[]),
    ("sdp-portmapper", 3935, "udp", &[]),
    ("net-device", 4350, "tcp", &[]),
    ("net-device", 4350, "udp", &[]),
    ("ipsec-msft", 4500, "tcp", &[]),
    ("ipsec-msft", 4500, "udp", &[]),
    ("llmnr", 5355, "tcp", &[]),
    ("llmnr", 5355, "udp", &[]),
    ("rrac", 5678, "tcp", &[]),
    ("rrac", 5678, "udp", &[]),
    ("dccm", 5679, "tcp", &[]),
    ("dccm", 5679, "udp", &[]),
    ("ms-licensing", 5720, "tcp", &[]),
    ("ms-licensing", 5720, "udp", &[]),
    ("directplay8", 6073, "tcp", &[]),
    ("directplay8", 6073, "udp", &[]),
    ("man", 9535, "tcp", &[]),
    ("rasadv", 9753, "tcp", &[]),
    ("rasadv", 9753, "udp", &[]),
    ("imip-channels", 11320, "tcp", &[]),
    ("imip-channels", 11320, "udp", &[]),
    ("directplaysrvr", 47624, "tcp", &[]),
    ("directplaysrvr", 47624, "udp", &[]),
];

fn cstr_lossy(p: u32) -> String {
    String::from_utf8_lossy(&mem::cstr(p)).into_owned()
}

/// Lays out strings and pointer arrays after a fixed struct in a buffer: `put_str` returns the
/// string's address, `put_ptrs` a NULL-ended array's
struct Pack {
    at: u32,
}
impl Pack {
    fn ptrs(&mut self, n: usize) -> u32 {
        let a = self.at;
        self.at += ps() * (n as u32 + 1);
        a
    }
    fn bytes(&mut self, b: &[u8]) -> u32 {
        let a = self.at;
        mem::write(a, b);
        self.at += b.len() as u32;
        a
    }
    fn str(&mut self, s: &str) -> u32 {
        let a = self.bytes(s.as_bytes());
        self.bytes(&[0]);
        a
    }
}

/// protoent { p_name, p_aliases, p_proto (short) }
fn protoent(e: &(u32, &str, &str)) -> u32 {
    let b = thread_buf(1, 256);
    let k = ps();
    let mut p = Pack { at: b + 3 * k };
    let al = p.ptrs(1);
    let name = p.str(e.1);
    let a0 = p.str(e.2);
    wp(b, name);
    wp(b + k, al);
    w16(b + 2 * k, e.0 as u16);
    wp(al, a0);
    wp(al + k, 0);
    b
}
fn getprotobyname(c: &mut Ctx) -> u32 {
    proto_by_name(c.a(0))
}
fn proto_by_name(p: u32) -> u32 {
    let n = cstr_lossy(p);
    match PROTOCOLS
        .iter()
        .find(|p| p.1.eq_ignore_ascii_case(&n) || p.2.eq_ignore_ascii_case(&n))
    {
        Some(p) => protoent(p),
        None => {
            last(WSANO_DATA);
            0
        }
    }
}
fn getprotobynumber(c: &mut Ctx) -> u32 {
    proto_by_number(c.a(0))
}
fn proto_by_number(n: u32) -> u32 {
    match PROTOCOLS.iter().find(|p| p.0 == n) {
        Some(p) => protoent(p),
        None => {
            last(WSANO_DATA);
            0
        }
    }
}

/// servent: x86 { s_name, s_aliases, s_port (short), s_proto }, x64 { s_name, s_aliases,
/// s_proto, s_port }
fn servent(e: &(&str, u16, &str, &[&str])) -> u32 {
    let b = thread_buf(2, 512);
    let k = ps();
    let mut p = Pack { at: b + 4 * k };
    let al = p.ptrs(e.3.len());
    let name = p.str(e.0);
    let proto = p.str(e.2);
    for (i, a) in e.3.iter().enumerate() {
        let s = p.str(a);
        wp(al + k * i as u32, s);
    }
    wp(al + k * e.3.len() as u32, 0);
    wp(b, name);
    wp(b + k, al);
    if mem::x64() {
        wp(b + 16, proto);
        w16(b + 24, e.1.to_be());
    } else {
        w16(b + 8, e.1.to_be());
        wp(b + 12, proto);
    }
    b
}
fn service_by_name(
    name: &str,
    proto: Option<&str>,
) -> Option<&'static (&'static str, u16, &'static str, &'static [&'static str])> {
    SERVICES.iter().find(|s| {
        (s.0.eq_ignore_ascii_case(name) || s.3.iter().any(|a| a.eq_ignore_ascii_case(name)))
            && proto.is_none_or(|p| s.2.eq_ignore_ascii_case(p))
    })
}
fn getservbyname(c: &mut Ctx) -> u32 {
    serv_by_name(c.a(0), c.a(1))
}
fn serv_by_name(n: u32, p: u32) -> u32 {
    let n = cstr_lossy(n);
    let proto = (p != 0).then(|| cstr_lossy(p));
    match service_by_name(&n, proto.as_deref()) {
        Some(s) => servent(s),
        None => {
            last(WSANO_DATA);
            0
        }
    }
}
fn getservbyport(c: &mut Ctx) -> u32 {
    serv_by_port(c.a(0), c.a(1))
}
fn serv_by_port(port: u32, p: u32) -> u32 {
    let port = u16::from_be(port as u16);
    let proto = (p != 0).then(|| cstr_lossy(p));
    match SERVICES
        .iter()
        .find(|s| s.1 == port && proto.as_deref().is_none_or(|p| s.2.eq_ignore_ascii_case(p)))
    {
        Some(s) => servent(s),
        None => {
            last(WSANO_DATA);
            0
        }
    }
}

// ---- host names ----

fn gethostname(c: &mut Ctx, wide: bool) -> u32 {
    let (buf, len) = (c.a(0), c.a(1) as i32);
    if !wide && !started() {
        return err(WSANOTINITIALISED);
    }
    if buf == 0 || len <= HOSTNAME.len() as i32 {
        return err(WSAEFAULT);
    }
    netaddr::put_raw(HOSTNAME, buf, wide);
    0
}

/// The addresses a host name stands for (IPv4 then IPv6, each in preference order), or None
pub fn resolve(name: &str) -> Option<Vec<Addr>> {
    let lo4 = Addr::v4([127, 0, 0, 1], 0);
    let mut lo6 = [0u8; 16];
    lo6[15] = 1;
    let lo6 = Addr::v6(lo6, 0);
    if name.is_empty()
        || name.eq_ignore_ascii_case(HOSTNAME)
        || name.eq_ignore_ascii_case("localhost")
        || name.eq_ignore_ascii_case(&format!("{HOSTNAME}."))
    {
        return Some(vec![lo6, lo4]);
    }
    literal(name).map(|a| vec![a]).or_else(|| hosts(name))
}

const AI_DISABLE_IDN_ENCODING: u32 = 0x80000;

/// A host name's ASCII form: each label with non-ASCII characters becomes "xn--" + its
/// Punycode (RFC 3492), lower-cased first
pub fn idna(name: &str) -> String {
    name.split('.')
        .map(|l| {
            if l.is_ascii() {
                l.to_string()
            } else {
                format!("xn--{}", punycode(&l.to_lowercase()))
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn punycode(label: &str) -> String {
    const BASE: u32 = 36;
    let digit = |d: u32| {
        (if d < 26 {
            b'a' + d as u8
        } else {
            b'0' + (d - 26) as u8
        }) as char
    };
    let adapt = |mut delta: u32, n: u32, first: bool| {
        delta /= if first { 700 } else { 2 };
        delta += delta / n;
        let mut k = 0;
        while delta > ((BASE - 1) * 26) / 2 {
            delta /= BASE - 1;
            k += BASE;
        }
        k + (BASE * delta) / (delta + 38)
    };
    let cps: Vec<u32> = label.chars().map(|c| c as u32).collect();
    let mut out: String = label.chars().filter(char::is_ascii).collect();
    let basic = out.len() as u32;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias, mut h) = (128u32, 0u32, 72u32, basic);
    while (h as usize) < cps.len() {
        let m = *cps.iter().filter(|&&c| c >= n).min().unwrap();
        delta += (m - n) * (h + 1);
        n = m;
        for &c in &cps {
            if c < n {
                delta += 1;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        1
                    } else if k >= bias + 26 {
                        26
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, h + 1, h == basic);
                delta = 0;
                h += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    out
}

/// an IPv4 / IPv6 address written as text
fn literal(name: &str) -> Option<Addr> {
    let u: Vec<u16> = name.encode_utf16().collect();
    let r = netaddr::v4_parse(&u, false, false, false);
    if r.ok && !name.contains(':') {
        return Some(Addr::v4(r.addr.unwrap(), 0));
    }
    let mut a = [0u8; 16];
    let r = netaddr::v6_parse(&u, &mut a, true, false);
    if r.ok {
        let mut x = Addr::v6(a, 0);
        x.scope = r.scope;
        return Some(x);
    }
    None
}

/// There is no DNS: a name resolves only through the hosts file (drivers\etc\hosts, lines of
/// "address name [aliases] # comment"), every matching line in order
fn hosts(name: &str) -> Option<Vec<Addr>> {
    let text = std::fs::read(crate::files::to_host(
        "C:\\windows\\system32\\drivers\\etc\\hosts",
    ))
    .ok()?;
    let found: Vec<Addr> = String::from_utf8_lossy(&text)
        .lines()
        .filter_map(|l| {
            let mut w = l.split('#').next()?.split_whitespace();
            let a = literal(w.next()?)?;
            w.any(|n| n.eq_ignore_ascii_case(name.trim_end_matches('.')))
                .then_some(a)
        })
        .collect();
    (!found.is_empty()).then_some(found)
}

/// hostent { h_name, h_aliases, h_addrtype (short), h_length (short), h_addr_list }, then the
/// aliases array, the address list array, the addresses and the strings, in that order
fn hostent(name: &str, af: u32, addrs: &[Vec<u8>]) -> u32 {
    let b = thread_buf(3, 1024);
    let k = ps();
    let len = if af == AF_INET6 { 16 } else { 4 };
    let mut p = Pack { at: b + 4 * k };
    let al = p.ptrs(0);
    let list = p.ptrs(addrs.len());
    for (i, a) in addrs.iter().enumerate() {
        let x = p.bytes(a);
        wp(list + k * i as u32, x);
    }
    wp(list + k * addrs.len() as u32, 0);
    let n = p.str(name);
    wp(al, 0);
    wp(b, n);
    wp(b + k, al);
    w16(b + 2 * k, af as u16);
    w16(b + 2 * k + 2, len);
    wp(b + 3 * k, list);
    b
}
fn gethostbyname(c: &mut Ctx) -> u32 {
    host_by_name(c.a(0))
}
fn host_by_name(p: u32) -> u32 {
    if !started() {
        last(WSANOTINITIALISED);
        return 0;
    }
    let name = if p == 0 {
        HOSTNAME.to_string()
    } else {
        cstr_lossy(p)
    };
    match resolve(&name) {
        Some(v) => {
            let v4: Vec<Vec<u8>> = v
                .iter()
                .filter(|a| !a.is_v6())
                .map(|a| a.ip.clone())
                .collect();
            if v4.is_empty() {
                last(WSANO_DATA);
                return 0;
            }
            let shown = if name.is_empty()
                || name.eq_ignore_ascii_case("localhost")
                || name.eq_ignore_ascii_case(HOSTNAME)
            {
                if name.eq_ignore_ascii_case("localhost") {
                    HOSTNAME.to_string()
                } else {
                    HOSTNAME.to_string()
                }
            } else {
                name.clone()
            };
            hostent(&shown, AF_INET, &v4)
        }
        None => {
            last(WSAHOST_NOT_FOUND);
            0
        }
    }
}
fn gethostbyaddr(c: &mut Ctx) -> u32 {
    host_by_addr(c.a(0), c.a(1), c.a(2))
}
fn host_by_addr(a: u32, len: u32, af: u32) -> u32 {
    if !started() {
        last(WSANOTINITIALISED);
        return 0;
    }
    if a == 0 {
        last(WSAEFAULT);
        return 0;
    }
    let want = match af {
        AF_INET => 4,
        AF_INET6 => 16,
        _ => {
            last(WSAEAFNOSUPPORT);
            return 0;
        }
    };
    if len < want {
        last(WSAEFAULT);
        return 0;
    }
    let ip = mem::slice(a, want).to_vec();
    let addr = Addr {
        ip: ip.clone(),
        port: 0,
        flow: 0,
        scope: 0,
    };
    if !addr.loopback() {
        last(WSAHOST_NOT_FOUND);
        return 0;
    }
    hostent(HOSTNAME, af, &[ip])
}

// ---- getaddrinfo ----

const AI_PASSIVE: u32 = 1;
const AI_CANONNAME: u32 = 2;
const AI_NUMERICHOST: u32 = 4;
const AI_NUMERICSERV: u32 = 8;
const AI_ALL: u32 = 0x100;
const AI_ADDRCONFIG: u32 = 0x400;
const AI_V4MAPPED: u32 = 0x800;
const AI_NON_AUTHORITATIVE: u32 = 0x4000;
const AI_SECURE: u32 = 0x8000;
const AI_RETURN_PREFERRED_NAMES: u32 = 0x10000;
const AI_FQDN: u32 = 0x20000;
const AI_FILESERVER: u32 = 0x40000;
const AI_EXTENDED: u32 = 0x80000000;
const AI_RESOLUTION_HANDLE: u32 = 0x40000000;

pub struct Hints {
    pub flags: u32,
    pub family: u32,
    pub socktype: u32,
    pub protocol: u32,
}

/// The entries for (node, service, hints): Ok((canonical name, [(addr, socktype, protocol)]))
/// or the error
pub fn addrinfo(
    node: Option<&str>,
    service: Option<&str>,
    h: &Hints,
) -> Result<(String, Vec<(Addr, u32, u32)>), u32> {
    if node.is_none() && service.is_none() {
        return Err(WSAHOST_NOT_FOUND);
    }
    let known = AI_PASSIVE
        | AI_CANONNAME
        | AI_NUMERICHOST
        | AI_NUMERICSERV
        | AI_ALL
        | AI_ADDRCONFIG
        | AI_V4MAPPED
        | AI_NON_AUTHORITATIVE
        | AI_SECURE
        | AI_RETURN_PREFERRED_NAMES
        | AI_FQDN
        | AI_FILESERVER
        | AI_DISABLE_IDN_ENCODING
        | AI_EXTENDED
        | AI_RESOLUTION_HANDLE
        | 0x10
        | 0x1000
        | 0x2000
        | 0x100000
        | 0x200000;
    if h.flags & !known != 0
        || (h.flags & AI_CANONNAME != 0 && node.is_none())
        || (h.flags & AI_FQDN != 0 && h.flags & AI_CANONNAME != 0)
    {
        return Err(WSAEINVAL);
    }
    if !matches!(h.family, AF_UNSPEC | AF_INET | AF_INET6) {
        return Err(WSAEAFNOSUPPORT);
    }
    if !matches!(h.socktype, 0 | 1 | 2 | 3 | 4 | 5) {
        return Err(WSAESOCKTNOSUPPORT);
    }
    // the port
    let port = match service {
        None | Some("") => 0,
        Some(s) => match s.parse::<u32>() {
            Ok(p) if p <= 0xffff => p as u16,
            Ok(_) => return Err(WSATYPE_NOT_FOUND),
            Err(_) if h.flags & AI_NUMERICSERV != 0 => return Err(WSATYPE_NOT_FOUND),
            Err(_) => {
                let proto = if h.socktype == 2 || h.protocol == 17 {
                    "udp"
                } else {
                    "tcp"
                };
                match service_by_name(s, Some(proto)) {
                    Some(e) => e.1,
                    None => return Err(WSATYPE_NOT_FOUND),
                }
            }
        },
    };
    let mut lo6 = [0u8; 16];
    lo6[15] = 1;
    let addrs = match node {
        None => {
            if h.flags & AI_PASSIVE != 0 {
                vec![Addr::v6([0; 16], 0), Addr::v4([0; 4], 0)]
            } else {
                vec![Addr::v6(lo6, 0), Addr::v4([127, 0, 0, 1], 0)]
            }
        }
        Some(n) => {
            let numeric = literal(n).is_some();
            if h.flags & AI_NUMERICHOST != 0 && !numeric {
                return Err(WSAHOST_NOT_FOUND);
            }
            // (a name in other scripts is looked up as its IDNA "xn--" form)
            let n = if h.flags & AI_DISABLE_IDN_ENCODING == 0 {
                idna(n)
            } else {
                n.to_string()
            };
            resolve(&n).ok_or(WSAHOST_NOT_FOUND)?
        }
    };
    let mut out = Vec::new();
    for mut a in addrs {
        if h.family != AF_UNSPEC && a.family() != h.family {
            if h.family == AF_INET6 && h.flags & AI_V4MAPPED != 0 && !a.is_v6() {
                a = Addr::mapped(a.ip[..4].try_into().unwrap(), 0);
            } else {
                continue;
            }
        }
        a.port = port;
        // (the hints' type and protocol are copied as given, neither filled in from the other)
        out.push((a, h.socktype, h.protocol));
    }
    if out.is_empty() {
        // (a name known only by addresses of the other family)
        return Err(
            if node.is_some_and(|n| !n.is_empty() && literal(n).is_none()) {
                WSANO_DATA
            } else {
                WSAHOST_NOT_FOUND
            },
        );
    }
    let canon = match node {
        Some(n) if !n.is_empty() && !n.eq_ignore_ascii_case("localhost") => n.to_string(),
        _ => HOSTNAME.to_string(),
    };
    Ok((canon, out))
}

fn hints_at(p: u32) -> Hints {
    if p == 0 {
        return Hints {
            flags: 0,
            family: 0,
            socktype: 0,
            protocol: 0,
        };
    }
    Hints {
        flags: r32(p),
        family: r32(p + 4),
        socktype: r32(p + 8),
        protocol: r32(p + 12),
    }
}

/// Builds the result list: ADDRINFOA/W { flags, family, socktype, protocol, addrlen (size_t),
/// canonname, addr, next }, or ADDRINFOEX { ..., addr, blob, bloblen, provider, next } (`ex`)
fn build_ai(canon: &str, v: &[(Addr, u32, u32)], flags: u32, wide: bool, ex: bool) -> u32 {
    let k = ps();
    let hdr = 16 + k * if ex { 7 } else { 4 };
    let next_off = hdr - k;
    let mut head = 0;
    let mut prev = 0;
    for (i, (a, t, p)) in v.iter().enumerate() {
        let cn = if i == 0 && flags & AI_CANONNAME != 0 {
            Some(canon)
        } else {
            None
        };
        let cn_len = cn.map_or(0, |s| (s.len() as u32 + 1) * if wide { 2 } else { 1 });
        let e = mem::heap_alloc(hdr + 28 + cn_len, true);
        w32(e, flags & (AI_PASSIVE | AI_CANONNAME | AI_NUMERICHOST));
        w32(e, 0);
        w32(e + 4, a.family());
        w32(e + 8, *t);
        w32(e + 12, *p);
        wp(e + 16, a.size());
        let sa = e + hdr;
        mem::write(sa, &a.bytes());
        if let Some(s) = cn {
            netaddr::put_raw(s, sa + 28, wide);
            wp(e + 16 + k, sa + 28);
        }
        wp(e + 16 + 2 * k, sa);
        if prev != 0 {
            wp(prev + next_off, e);
        } else {
            head = e;
        }
        prev = e;
    }
    head
}

fn free_ai(c: &mut Ctx, ex: bool) -> u32 {
    let k = ps();
    let next_off = 16 + k * if ex { 7 } else { 4 } - k;
    let mut p = c.a(0);
    while p != 0 {
        let n = mem::rp(p + next_off);
        if mem::heap_owns(p) {
            mem::heap_free(p);
        }
        p = n;
    }
    0
}

fn ostr(p: u32, wide: bool) -> Option<String> {
    (p != 0).then(|| {
        if wide {
            String::from_utf16_lossy(&mem::wstr(p))
        } else {
            cstr_lossy(p)
        }
    })
}

/// getaddrinfo(node, service, hints, &result) / GetAddrInfoW
fn getaddrinfo(c: &mut Ctx, wide: bool) -> u32 {
    let (n, s, h, res) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if !started() {
        last(WSANOTINITIALISED);
        return WSANOTINITIALISED;
    }
    if res == 0 {
        last(WSANO_RECOVERY);
        return WSANO_RECOVERY;
    }
    wp(res, 0);
    let hints = hints_at(h);
    match addrinfo(ostr(n, wide).as_deref(), ostr(s, wide).as_deref(), &hints) {
        Ok((canon, v)) => {
            wp(res, build_ai(&canon, &v, hints.flags, wide, false));
            last(0);
            0
        }
        Err(e) => {
            last(e);
            e
        }
    }
}

/// GetAddrInfoExW(name, service, namespace, nsid, hints, &result, timeout, overlapped,
/// completion, &handle): answered at once (an overlapped request completes immediately)
fn GetAddrInfoExW(c: &mut Ctx) -> u32 {
    let (n, s, h, res, ov, cb) = (c.a(0), c.a(1), c.a(4), c.a(5), c.a(7), c.a(8));
    let fail = |e: u32| {
        last(e);
        e
    };
    if !started() {
        return fail(WSANOTINITIALISED);
    }
    if res == 0 {
        return fail(WSAEINVAL);
    }
    wp(res, 0);
    let k = ps();
    if ov != 0 && cb != 0 && mem::rp(ov + 2 * k + 8) != 0 {
        return fail(WSAEINVAL); // (an event and a routine)
    }
    let (node, serv) = (ostr(n, true), ostr(s, true));
    let hints = hints_at(h);
    let r = addrinfo(node.as_deref(), serv.as_deref(), &hints);
    // a lookup that fails for want of the name completes like one that succeeds; the call's
    // own mistakes (no name and no service, bad hints) fail at once
    let code = match &r {
        Ok(_) => 0,
        Err(e) if *e == WSAHOST_NOT_FOUND && node.is_some() => *e,
        Err(e) => return fail(*e),
    };
    let ai = match r {
        Ok((canon, v)) => build_ai(&canon, &v, hints.flags, true, true),
        Err(_) => 0,
    };
    if ov == 0 {
        wp(res, ai);
        return fail(code);
    }
    let hp = c.a(9);
    if hp != 0 {
        wp(hp, 0x1234);
    }
    // OVERLAPPED { Internal = the result, Pointer (over Offset) = &result, hEvent }
    wp(ov + 2 * k, res);
    if cb != 0 {
        // the routine runs on a thread of its own (a thread pool's, on Windows) once done
        wp(ov, 0x2734); // (WSAEINPROGRESS)
        on_thread(cb, vec![code, 0, ov], move || {
            wp(res, ai);
            wp(ov, code);
        });
    } else {
        wp(res, ai);
        wp(ov, code);
        let ev = mem::rp(ov + 2 * k + 8);
        if ev != 0 {
            crate::k32::set_event(ev);
        }
    }
    fail(997) // WSA_IO_PENDING
}

type Job = (u32, Vec<u32>, Box<dyn FnOnce() + Send>);
static JOBS: Mutex<Vec<(u32, Job)>> = Mutex::new(Vec::new());

/// Runs `before()` and then the guest's `f(args)` on a new guest thread
pub fn on_thread(f: u32, args: Vec<u32>, before: impl FnOnce() + Send + 'static) {
    static ENTRY: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    JOBS.lock().unwrap().push((id, (f, args, Box::new(before))));
    let entry = *ENTRY.get_or_init(|| {
        crate::api::host_fn(
            "ws2_job_thread",
            |c| {
                let id = c.a(0);
                let job = {
                    let mut v = JOBS.lock().unwrap();
                    v.iter().position(|j| j.0 == id).map(|i| v.remove(i).1)
                };
                if let Some((f, args, before)) = job {
                    // (the answer comes a moment later, as a lookup's does: the caller has
                    // returned from the call by then)
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    before();
                    c.call(f, &args);
                }
                0
            },
            4,
        )
    });
    crate::spawn_gated(entry, id, 0x10000, crate::obj::new_thread_obj(0), false);
}

/// getnameinfo(sa, salen, host, hostlen, serv, servlen, flags) / GetNameInfoW
fn getnameinfo(c: &mut Ctx, wide: bool) -> u32 {
    let (sa, salen, host, hlen, serv, slen, flags) =
        (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    const NI_NOFQDN: u32 = 1;
    const NI_NUMERICHOST: u32 = 2;
    const NI_NAMEREQD: u32 = 4;
    const NI_NUMERICSERV: u32 = 8;
    const NI_DGRAM: u32 = 16;
    let fail = |e| {
        last(e);
        e
    };
    if !started() {
        return fail(WSANOTINITIALISED);
    }
    let a = match read_sa(sa, salen) {
        Ok(a) => a,
        Err(e) => {
            return fail(if e == WSAEAFNOSUPPORT {
                WSAEAFNOSUPPORT
            } else {
                WSAEFAULT
            })
        }
    };
    if (host == 0 || hlen == 0) && (serv == 0 || slen == 0) {
        return fail(WSAHOST_NOT_FOUND);
    }
    if host != 0 && hlen != 0 {
        let numeric = {
            let mut x = a.clone();
            x.port = 0;
            if x.is_v6() {
                netaddr::v6_text(x.ip.as_slice().try_into().unwrap(), x.scope, 0)
            } else {
                x.text()
            }
        };
        let name = if flags & NI_NUMERICHOST != 0 {
            numeric
        } else if a.loopback() {
            let _ = NI_NOFQDN;
            HOSTNAME.to_string()
        } else if flags & NI_NAMEREQD != 0 {
            return fail(WSAHOST_NOT_FOUND);
        } else {
            numeric
        };
        if name.len() as u32 >= hlen {
            return fail(WSAEFAULT);
        }
        netaddr::put_raw(&name, host, wide);
    }
    if serv != 0 && slen != 0 {
        let proto = if flags & NI_DGRAM != 0 { "udp" } else { "tcp" };
        let s = if flags & NI_NUMERICSERV != 0 {
            a.port.to_string()
        } else {
            SERVICES
                .iter()
                .find(|s| s.1 == a.port && s.2 == proto)
                .map_or(a.port.to_string(), |s| s.0.to_string())
        };
        if s.len() as u32 >= slen {
            return fail(WSAEFAULT);
        }
        netaddr::put_raw(&s, serv, wide);
    }
    last(0);
    0
}

// ---- WSAAsyncGet*: answered at once, the reply posted to the window ----

static ASYNC: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x100);

/// Copies the per-thread result struct (`r`, of `kind`'s layout) into the caller's buffer and
/// posts (hwnd, msg, handle, MAKELONG(size or needed, error))
fn async_reply(hwnd: u32, msg: u32, buf: u32, blen: u32, r: u32, kind: u8) -> u32 {
    let h = ASYNC.fetch_add(4, std::sync::atomic::Ordering::Relaxed);
    let k = ps();
    let (lp, ok) = if r == 0 {
        (crate::ws2::get_last() << 16, false)
    } else {
        // relocate: copy the struct and the data that follows it (within its buffer), then
        // shift every pointer inside by the distance moved
        let size = match kind {
            1 => 256,
            2 => 512,
            _ => 1024,
        };
        if blen < size.min(blen.max(1)) {
            (WSAENOBUFS << 16 | size, false)
        } else {
            let n = size.min(blen);
            mem::copy(buf, r, n);
            let delta = buf.wrapping_sub(r);
            let fix = |p: u32| {
                let v = mem::rp(p);
                if v >= r && v < r + size {
                    wp(p, v.wrapping_add(delta));
                }
            };
            // the struct's pointer fields, and the arrays they point to
            let fields: &[u32] = match kind {
                1 => &[0, k],
                2 => {
                    if mem::x64() {
                        &[0, 8, 16]
                    } else {
                        &[0, 4, 12]
                    }
                }
                _ => &[0, k, 3 * k],
            };
            for &f in fields {
                fix(buf + f);
            }
            for &f in &fields[1..] {
                if kind == 2 && f != k {
                    continue;
                }
                let mut a = mem::rp(buf + f);
                while a != 0 && a >= buf && a < buf + n && mem::rp(a) != 0 {
                    fix(a);
                    a += k;
                }
            }
            (n, true)
        }
    };
    let _ = ok;
    crate::user::post(hwnd, msg, h, lp);
    h
}

fn async_host(c: &mut Ctx) -> u32 {
    let (hwnd, msg, name, buf, blen) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let r = host_by_name(name);
    async_reply(hwnd, msg, buf, blen, r, 3)
}
fn async_addr(c: &mut Ctx) -> u32 {
    let (hwnd, msg, a, len, ty, buf, blen) =
        (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let r = host_by_addr(a, len, ty);
    async_reply(hwnd, msg, buf, blen, r, 3)
}
fn async_proto(c: &mut Ctx, by_name: bool) -> u32 {
    let (hwnd, msg, key, buf, blen) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let r = if by_name {
        proto_by_name(key)
    } else {
        proto_by_number(key)
    };
    async_reply(hwnd, msg, buf, blen, r, 1)
}
fn async_serv(c: &mut Ctx, by_name: bool) -> u32 {
    let (hwnd, msg, key, proto, buf, blen) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let r = if by_name {
        serv_by_name(key, proto)
    } else {
        serv_by_port(key, proto)
    };
    async_reply(hwnd, msg, buf, blen, r, 2)
}

// ---- the provider catalog ----

/// (family, type, protocol, name, service flags, max message size, catalog id)
pub const CATALOG: &[(u32, u32, u32, &str, u32, u32, u32)] = &[
    (AF_INET, 1, 6, "MSAFD Tcpip [TCP/IP]", 0x20066, 0, 1001),
    (AF_INET, 2, 17, "MSAFD Tcpip [UDP/IP]", 0x20609, 65527, 1002),
    (AF_INET6, 1, 6, "MSAFD Tcpip [TCP/IPv6]", 0x20066, 0, 1003),
    (
        AF_INET6,
        2,
        17,
        "MSAFD Tcpip [UDP/IPv6]",
        0x20609,
        65527,
        1004,
    ),
];
/// {E70F1AA0-AB8B-11CF-8CA3-00805F48A192}, MSAFD's provider id
pub const PROVIDER: [u8; 16] = [
    0xa0, 0x1a, 0x0f, 0xe7, 0x8b, 0xab, 0xcf, 0x11, 0x8c, 0xa3, 0x00, 0x80, 0x5f, 0x48, 0xa1, 0x92,
];

/// WSAPROTOCOL_INFOA (372 bytes) or W (628) for catalog entry `e`; `reserved` goes in
/// dwProviderReserved (a duplicated socket's handle)
pub fn protocol_info(p: u32, e: &(u32, u32, u32, &str, u32, u32, u32), wide: bool, reserved: u32) {
    mem::write(p, &vec![0; if wide { 628 } else { 372 }]);
    w32(p, e.4); // dwServiceFlags1
    w32(p + 16, 8); // PFL_MATCHES_PROTOCOL_ZERO
    mem::write(p + 20, &PROVIDER);
    w32(p + 36, e.6);
    w32(p + 40, 1); // ProtocolChain.ChainLen: a base provider
    w32(p + 72, 2); // iVersion
    w32(p + 76, e.0);
    let sa = if e.0 == AF_INET6 { 28 } else { 16 };
    w32(p + 80, sa);
    w32(p + 84, sa);
    w32(p + 88, e.1);
    w32(p + 92, e.2);
    w32(p + 96, 0);
    w32(p + 100, 0); // BIGENDIAN
    w32(p + 108, e.5);
    w32(p + 112, reserved);
    netaddr::put_raw(e.3, p + 116, wide);
}

/// The catalog entry a socket of (family, type, protocol) comes from
pub fn catalog_entry(
    af: u32,
    ty: u32,
    proto: u32,
) -> Option<&'static (u32, u32, u32, &'static str, u32, u32, u32)> {
    CATALOG
        .iter()
        .find(|e| e.0 == af && e.1 == ty && (proto == 0 || proto == e.2))
}

/// WSAEnumProtocols(protocols (0-ended) or NULL, buf, &len)
fn enum_protocols(c: &mut Ctx, wide: bool) -> u32 {
    enum_protocols_at(c.a(0), c.a(1), c.a(2), wide)
}
fn enum_protocols_at(list: u32, buf: u32, lenp: u32, wide: bool) -> u32 {
    if lenp == 0 {
        return err(WSAEFAULT);
    }
    let mut want = Vec::new();
    if list != 0 {
        let mut i = 0;
        loop {
            let v = r32(list + 4 * i);
            if v == 0 {
                break;
            }
            want.push(v);
            i += 1;
        }
    }
    let v: Vec<_> = CATALOG
        .iter()
        .filter(|e| list == 0 || want.contains(&e.2))
        .collect();
    let each = if wide { 628 } else { 372 };
    let need = each * v.len() as u32;
    if buf == 0 || r32(lenp) < need {
        w32(lenp, need);
        return err(WSAENOBUFS);
    }
    for (i, e) in v.iter().enumerate() {
        protocol_info(buf + each * i as u32, e, wide, 0);
    }
    v.len() as u32
}

/// WSAEnumNameSpaceProviders(&len, buf): the DNS provider (NS_DNS 12), WSANAMESPACE_INFO
/// { NSProviderId, dwNameSpace, fActive, dwVersion, lpszIdentifier }
fn enum_namespaces(c: &mut Ctx, wide: bool) -> u32 {
    let (lenp, buf) = (c.a(0), c.a(1));
    let k = ps();
    let name = "Tcpip";
    let hdr = 16 + 12 + k;
    let need = hdr + (name.len() as u32 + 1) * if wide { 2 } else { 1 };
    if lenp == 0 || buf == 0 || r32(lenp) < need {
        if lenp != 0 {
            w32(lenp, need);
        }
        return err(WSAEFAULT);
    }
    // {22059D40-7E9E-11CF-AE5A-00AA00A7112B}, the DNS name space provider
    mem::write(
        buf,
        &[
            0x40, 0x9d, 0x05, 0x22, 0x9e, 0x7e, 0xcf, 0x11, 0xae, 0x5a, 0x00, 0xaa, 0x00, 0xa7,
            0x11, 0x2b,
        ],
    );
    w32(buf + 16, 12);
    w32(buf + 20, 1);
    w32(buf + 24, 0);
    netaddr::put_raw(name, buf + hdr, wide);
    wp(buf + 28, buf + hdr);
    1
}

/// WSCGetProviderPath(&id, buf, &len, &err): %SystemRoot%\system32\mswsock.dll for MSAFD
fn WSCGetProviderPath(c: &mut Ctx) -> u32 {
    let (id, buf, lenp, errp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if errp == 0 {
        return SOCKET_ERROR;
    }
    if id == 0 || lenp == 0 {
        w32(errp, WSAEFAULT);
        return SOCKET_ERROR;
    }
    if mem::slice(id, 16) != PROVIDER {
        w32(errp, WSAEINVAL);
        return SOCKET_ERROR;
    }
    let path = "%SystemRoot%\\system32\\mswsock.dll";
    if buf == 0 || (r32(lenp) as i32) <= path.len() as i32 {
        w32(lenp, path.len() as u32 + 1);
        w32(errp, WSAEFAULT);
        return SOCKET_ERROR;
    }
    netaddr::put_raw(path, buf, true);
    w32(lenp, path.len() as u32 + 1);
    0
}

pub fn init() {
    for d in ["ws2_32", "wsock32"] {
        implement(d, "htonl", htonl);
        implement(d, "ntohl", htonl);
        implement(d, "htons", htons);
        implement(d, "ntohs", htons);
        implement(d, "inet_addr", inet_addr);
        implement(d, "inet_ntoa", inet_ntoa);
        implement(d, "gethostname", |c| gethostname(c, false));
        implement(d, "gethostbyname", gethostbyname);
        implement(d, "gethostbyaddr", gethostbyaddr);
        implement(d, "getprotobyname", getprotobyname);
        implement(d, "getprotobynumber", getprotobynumber);
        implement(d, "getservbyname", getservbyname);
        implement(d, "getservbyport", getservbyport);
        implement(d, "WSAAsyncGetHostByName", async_host);
        implement(d, "WSAAsyncGetHostByAddr", async_addr);
        implement(d, "WSAAsyncGetProtoByName", |c| async_proto(c, true));
        implement(d, "WSAAsyncGetProtoByNumber", |c| async_proto(c, false));
        implement(d, "WSAAsyncGetServByName", |c| async_serv(c, true));
        implement(d, "WSAAsyncGetServByPort", |c| async_serv(c, false));
        implement(d, "WSACancelAsyncRequest", |c| {
            if c.a(0) == 0 {
                err(WSAEINVAL)
            } else {
                err(WSAEINVAL)
            }
        });
    }
    let d = "ws2_32";
    implement(d, "WSAHtonl", |c| wsa_order(c, true));
    implement(d, "WSANtohl", |c| wsa_order(c, true));
    implement(d, "WSAHtons", |c| wsa_order(c, false));
    implement(d, "WSANtohs", |c| wsa_order(c, false));
    implement(d, "inet_pton", |c| pton(c, false));
    implement(d, "InetPtonW", |c| pton(c, true));
    implement(d, "inet_ntop", |c| ntop(c, false));
    implement(d, "InetNtopW", |c| ntop(c, true));
    implement(d, "WSAStringToAddressA", |c| string_to_address(c, false));
    implement(d, "WSAStringToAddressW", |c| string_to_address(c, true));
    implement(d, "WSAAddressToStringA", |c| address_to_string(c, false));
    implement(d, "WSAAddressToStringW", |c| address_to_string(c, true));
    implement(d, "GetHostNameW", |c| gethostname(c, true));
    implement(d, "getaddrinfo", |c| getaddrinfo(c, false));
    implement(d, "GetAddrInfoW", |c| getaddrinfo(c, true));
    implement(d, "GetAddrInfoExW", GetAddrInfoExW);
    implement(d, "GetAddrInfoExOverlappedResult", |c| {
        if c.a(0) == 0 {
            WSAEINVAL
        } else {
            mem::rp(c.a(0))
        }
    });
    implement(d, "GetAddrInfoExCancel", |c| {
        if c.a(0) == 0 {
            WSAEINVAL
        } else {
            6
        }
    });
    implement(d, "freeaddrinfo", |c| free_ai(c, false));
    implement(d, "FreeAddrInfoW", |c| free_ai(c, false));
    implement(d, "FreeAddrInfoEx", |c| free_ai(c, true));
    implement(d, "FreeAddrInfoExW", |c| free_ai(c, true));
    implement(d, "getnameinfo", |c| getnameinfo(c, false));
    implement(d, "GetNameInfoW", |c| getnameinfo(c, true));
    implement(d, "WSAEnumProtocolsA", |c| enum_protocols(c, false));
    implement(d, "WSAEnumProtocolsW", |c| enum_protocols(c, true));
    implement(d, "WSCEnumProtocols", |c| {
        let errp = c.a(3);
        let r = enum_protocols_at(c.a(0), c.a(1), c.a(2), true);
        if r == SOCKET_ERROR && errp != 0 {
            w32(errp, crate::ws2::get_last());
        }
        r
    });
    implement(d, "WSAEnumNameSpaceProvidersA", |c| {
        enum_namespaces(c, false)
    });
    implement(d, "WSAEnumNameSpaceProvidersW", |c| {
        enum_namespaces(c, true)
    });
    implement(d, "WSCGetProviderPath", WSCGetProviderPath);
    implement(d, "WSCGetProviderInfo", |c| {
        let (id, info, size, errp) = (c.a(0), c.a(2), c.a(3), c.a(5));
        if errp != 0 {
            w32(
                errp,
                if id == 0 || info == 0 && size == 0 {
                    WSAEFAULT
                } else {
                    WSANO_RECOVERY
                },
            );
        }
        SOCKET_ERROR
    });
    implement(d, "WSCGetApplicationCategory", |c| {
        if c.a(5) != 0 {
            w32(c.a(5), WSAEINVAL);
        }
        SOCKET_ERROR
    });
    // name space lookups: no network to list
    implement(d, "WSALookupServiceBeginA", lookup_begin);
    implement(d, "WSALookupServiceBeginW", lookup_begin);
    implement(d, "WSALookupServiceNextA", |_| err(WSA_E_NO_MORE));
    implement(d, "WSALookupServiceNextW", |_| err(WSA_E_NO_MORE));
    implement(d, "WSALookupServiceEnd", |c| {
        if c.a(0) == 0 {
            err(6)
        } else {
            0
        }
    });
    let _ = (w8, WSASERVICE_NOT_FOUND);
}

fn lookup_begin(c: &mut Ctx) -> u32 {
    let (qs, h) = (c.a(0), c.a(2));
    if qs == 0 || h == 0 {
        return err(WSAEFAULT);
    }
    if r32(qs) == 0 {
        return err(WSAEINVAL);
    }
    wp(h, 0x5ea0);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn punycode() {
        // RFC 3492 7.1 samples and the Wine test's name
        assert_eq!(
            super::idna("\u{30c6}\u{30b9}\u{30c8}.winehq.org"),
            "xn--zckzah.winehq.org"
        );
        assert_eq!(
            super::punycode(
                "\u{4ed6}\u{4eec}\u{4e3a}\u{4ec0}\u{4e48}\u{4e0d}\u{8bf4}\u{4e2d}\u{6587}"
            ),
            "ihqwcrb4cv8a8dqg056pqjye"
        );
        assert_eq!(super::punycode("b\u{fc}cher"), "bcher-kva");
        assert_eq!(super::idna("example.com"), "example.com");
    }
    #[test]
    fn ai() {
        let h = Hints {
            flags: 0,
            family: 0,
            socktype: 0,
            protocol: 0,
        };
        assert_eq!(addrinfo(None, None, &h).err(), Some(WSAHOST_NOT_FOUND));
        let (_, v) = addrinfo(Some("localhost"), Some("80"), &h).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].0, Addr::v4([127, 0, 0, 1], 80));
        let h4 = Hints {
            flags: AI_NUMERICHOST,
            family: AF_INET,
            socktype: 1,
            protocol: 0,
        };
        assert_eq!(
            addrinfo(Some("localhost"), None, &h4).err(),
            Some(WSAHOST_NOT_FOUND)
        );
        let (_, v) = addrinfo(Some("1.2.3.4"), Some("http"), &h4).unwrap();
        assert_eq!((v[0].0.port, v[0].1, v[0].2), (80, 1, 0));
        assert_eq!(
            addrinfo(Some("example.invalid"), None, &h).err(),
            Some(WSAHOST_NOT_FOUND)
        );
    }
    #[test]
    fn addr() {
        let a = Addr::mapped([127, 0, 0, 1], 5);
        assert!(a.loopback());
        assert_eq!(a.as_v4(), Some([127, 0, 0, 1]));
        assert_eq!(Addr::v4([1, 2, 3, 4], 80).text(), "1.2.3.4:80");
    }
}
