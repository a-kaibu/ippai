//! iphlpapi: the network configuration of a machine in a browser. It has the loopback interface
//! (127.0.0.1/8, ::1) and one Ethernet adapter that is not connected: it has a MAC address, and
//! some games read that for licensing, but it has no address, no gateway and no DNS server.
//! Every table, adapter list, conversion and notification below describes this one model, so the
//! APIs agree with each other. sake has no sockets, so the TCP and UDP tables are empty. Routes
//! are only the loopback interface's own, and nothing outside 127.0.0.0/8 or ::1 can be reached.
//! ICMP echo to a loopback address gets an answer and anything else times out. Changes to the
//! configuration (routes, addresses, ARP entries) are refused.
//!
//! The layouts are those of the Windows SDK headers for x86 and x64. The behaviour (errors,
//! buffer sizes, flags) follows Wine's iphlpapi tests, which give Windows' answers.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem::{self, ps, r16, r32, w16, w32, w64, w8, wp};

const NO_ERROR: u32 = 0;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_INVALID_DATA: u32 = 13;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_BUFFER_OVERFLOW: u32 = 111;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_NAME: u32 = 123;
const ERROR_NO_DATA: u32 = 232;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_NETWORK_UNREACHABLE: u32 = 1231;
const ERROR_NOT_FOUND: u32 = 1168;
const IP_REQ_TIMED_OUT: u32 = 11010;

const AF_UNSPEC: u32 = 0;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;

/// One network interface of the model
pub struct Iface {
    pub index: u32,
    pub luid_index: u32,
    /// IF_TYPE_ETHERNET_CSMACD 6, IF_TYPE_SOFTWARE_LOOPBACK 24
    pub ty: u32,
    pub guid: [u8; 16],
    pub alias: &'static str,
    pub descr: &'static str,
    pub mac: &'static [u8],
    pub mtu: u32,
    pub speed: u64,
    pub up: bool,
    pub v4: Option<([u8; 4], u8)>,
    pub v6: Option<([u8; 16], u8)>,
    pub metric: u32,
}

impl Iface {
    pub fn luid(&self) -> u64 {
        (self.ty as u64) << 48 | (self.luid_index as u64) << 24
    }
    fn loopback(&self) -> bool {
        self.ty == 24
    }
}

/// The interfaces, in the order Windows lists adapters (the loopback last)
pub fn ifaces() -> &'static [Iface] {
    static IF: [Iface; 2] = [
        Iface {
            index: 2,
            luid_index: 32769,
            ty: 6,
            // {5341d8c2-6b3e-4a2f-9a50-6f2e0d1c7b21}
            guid: [0xc2, 0xd8, 0x41, 0x53, 0x3e, 0x6b, 0x2f, 0x4a, 0x9a, 0x50, 0x6f, 0x2e, 0x0d, 0x1c, 0x7b, 0x21],
            alias: "Ethernet",
            descr: "sake Virtual Ethernet Adapter",
            mac: &[0x02, 0x00, 0x4c, 0x4f, 0x4f, 0x50],
            mtu: 1500,
            speed: 0,
            up: false,
            v4: None,
            v6: None,
            metric: 25,
        },
        Iface {
            index: 1,
            luid_index: 0,
            ty: 24,
            // {f8e3a1c0-5a5b-11ee-8000-000000000001}
            guid: [0xc0, 0xa1, 0xe3, 0xf8, 0x5b, 0x5a, 0xee, 0x11, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
            alias: "Loopback Pseudo-Interface 1",
            descr: "Software Loopback Interface 1",
            mac: &[],
            mtu: u32::MAX,
            speed: 1_073_741_824,
            up: true,
            v4: Some(([127, 0, 0, 1], 8)),
            v6: Some(([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 128)),
            metric: 75,
        },
    ];
    &IF
}

fn by_index(i: u32) -> Option<&'static Iface> {
    ifaces().iter().find(|f| f.index == i)
}
fn by_luid(l: u64) -> Option<&'static Iface> {
    ifaces().iter().find(|f| f.luid() == l)
}
/// ascending by index (the order of the MIB tables)
fn sorted() -> Vec<&'static Iface> {
    let mut v: Vec<&Iface> = ifaces().iter().collect();
    v.sort_by_key(|f| f.index);
    v
}

/// "{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}"
pub fn guid_string(g: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes(g[0..4].try_into().unwrap());
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    format!("{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{}}}", g[8], g[9], g[10..].iter().map(|b| format!("{b:02X}")).collect::<String>())
}

/// The reverse of guid_string (braces required, hex of either case)
pub fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let b = s.as_bytes();
    if b.len() != 38 || b[0] != b'{' || b[37] != b'}' || [9, 14, 19, 24].iter().any(|&i| b[i] != b'-') {
        return None;
    }
    let hex = |r: std::ops::Range<usize>| u64::from_str_radix(&s[r], 16).ok();
    if !s[1..37].chars().all(|c| c == '-' || c.is_ascii_hexdigit()) {
        return None;
    }
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&(hex(1..9)? as u32).to_le_bytes());
    g[4..6].copy_from_slice(&(hex(10..14)? as u16).to_le_bytes());
    g[6..8].copy_from_slice(&(hex(15..19)? as u16).to_le_bytes());
    g[8] = hex(20..22)? as u8;
    g[9] = hex(22..24)? as u8;
    for i in 0..6 {
        g[10 + i] = hex(25 + 2 * i..27 + 2 * i)? as u8;
    }
    Some(g)
}

/// The interface's name for ConvertInterfaceLuidToName: "<type>_<NetLuidIndex>"
pub fn if_name(f: &Iface) -> String {
    let t = match f.ty {
        6 => "ethernet",
        9 => "tokenring",
        23 => "ppp",
        24 => "loopback",
        37 => "atm",
        71 => "wireless",
        131 => "tunnel",
        144 => "ieee1394",
        _ => "other",
    };
    format!("{t}_{}", f.luid_index)
}

/// The LUID a name gives ("ethernet_32769"), for any type and index
pub fn name_to_luid(s: &str) -> Option<u64> {
    let (t, n) = s.rsplit_once('_')?;
    let ty = match t.to_ascii_lowercase().as_str() {
        "other" => 1,
        "ethernet" => 6,
        "tokenring" => 9,
        "ppp" => 23,
        "loopback" => 24,
        "atm" => 37,
        "wireless" => 71,
        "tunnel" => 131,
        "ieee1394" => 144,
        _ => return None,
    };
    let n: u32 = n.parse().ok()?;
    if n > 0xff_ffff {
        return None;
    }
    Some((ty as u64) << 48 | (n as u64) << 24)
}

fn device_name(f: &Iface) -> String {
    format!("\\DEVICE\\TCPIP_{}", guid_string(&f.guid))
}

pub fn v4_mask(len: u8) -> u32 {
    if len == 0 {
        0
    } else {
        (u32::MAX << (32 - len as u32)).to_be()
    }
}

// ---- guest helpers ----

fn put_wfixed(p: u32, chars: u32, s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(chars as usize - 1).collect();
    for (i, &u) in w.iter().enumerate() {
        w16(p + 2 * i as u32, u);
    }
    w16(p + 2 * w.len() as u32, 0);
}
fn put_cfixed(p: u32, n: u32, s: &str) {
    let b: Vec<u8> = s.bytes().take(n as usize - 1).collect();
    mem::write(p, &b);
    w8(p + b.len() as u32, 0);
}

/// The table protocol most "Get*Table(buffer, &size, order)" functions share: the size needed,
/// ERROR_INSUFFICIENT_BUFFER when it does not fit, otherwise `fill` writes it.
fn table(buf: u32, size: u32, need: u32, fill: impl FnOnce(u32)) -> u32 {
    if size == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    if buf == 0 || r32(size) < need {
        w32(size, need);
        return ERROR_INSUFFICIENT_BUFFER;
    }
    w32(size, need);
    mem::write(buf, &vec![0; need as usize]);
    fill(buf);
    NO_ERROR
}

// ---- MIB_IFROW / MIB_IF_ROW2 ----

fn fill_ifrow(p: u32, f: &Iface) {
    mem::write(p, &[0; 860]);
    put_wfixed(p, 256, &device_name(f));
    w32(p + 512, f.index);
    w32(p + 516, f.ty);
    w32(p + 520, f.mtu);
    w32(p + 524, f.speed.min(u32::MAX as u64) as u32);
    w32(p + 528, f.mac.len() as u32);
    mem::write(p + 532, f.mac);
    w32(p + 540, 1); // MIB_IF_ADMIN_STATUS_UP
    w32(p + 544, if f.up { 5 } else { 0 }); // OPERATIONAL / NON_OPERATIONAL
    w32(p + 600, f.descr.len() as u32 + 1);
    put_cfixed(p + 604, 256, f.descr);
}

fn fill_ifrow2(p: u32, f: &Iface) {
    mem::write(p, &[0; 1352]);
    w64(p, f.luid());
    w32(p + 8, f.index);
    mem::write(p + 12, &f.guid);
    put_wfixed(p + 28, 257, f.alias);
    put_wfixed(p + 542, 257, f.descr);
    w32(p + 1056, f.mac.len() as u32);
    mem::write(p + 1060, f.mac);
    mem::write(p + 1092, f.mac);
    w32(p + 1124, f.mtu);
    w32(p + 1128, f.ty);
    w32(p + 1132, 0); // TunnelType: none
    w32(p + 1136, 0); // MediaType: NdisMedium802_3
    w32(p + 1140, if f.loopback() { 0 } else { 14 }); // PhysicalMediumType: unspecified / 802_3
    w32(p + 1144, if f.loopback() { 1 } else { 2 }); // AccessType: loopback / broadcast
    w32(p + 1148, 0); // DirectionType: send and receive
    // InterfaceAndOperStatusFlags: HardwareInterface, ConnectorPresent for the Ethernet
    w8(p + 1152, if f.loopback() { 0 } else { 0x1 | 0x4 });
    w32(p + 1156, if f.up { 1 } else { 2 }); // IfOperStatusUp / Down
    w32(p + 1160, 1); // NET_IF_ADMIN_STATUS_UP
    w32(p + 1164, if f.up { 1 } else { 2 }); // MediaConnectStateConnected / Disconnected
    w32(p + 1184, 1); // NET_IF_CONNECTION_DEDICATED
    w64(p + 1192, f.speed);
    w64(p + 1200, f.speed);
}

fn GetNumberOfInterfaces(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    w32(c.a(0), ifaces().len() as u32);
    NO_ERROR
}
fn GetIfEntry(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match by_index(r32(p + 512)) {
        Some(f) => {
            fill_ifrow(p, f);
            NO_ERROR
        }
        None => ERROR_INVALID_DATA,
    }
}
fn GetIfTable(c: &mut Ctx) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    let v = sorted();
    table(buf, size, 4 + 860 * v.len() as u32, |b| {
        w32(b, v.len() as u32);
        for (i, f) in v.iter().enumerate() {
            fill_ifrow(b + 4 + 860 * i as u32, f);
        }
    })
}
/// GetIfEntry2(row): by InterfaceLuid, or InterfaceIndex when the LUID is 0
fn GetIfEntry2(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let (l, i) = (mem::r64(p), r32(p + 8));
    let f = if l != 0 { by_luid(l) } else if i != 0 { by_index(i) } else { return ERROR_INVALID_PARAMETER };
    match f {
        Some(f) => {
            fill_ifrow2(p, f);
            NO_ERROR
        }
        None => ERROR_FILE_NOT_FOUND,
    }
}
/// A MIB_*_TABLE2: NumEntries, then the rows from offset 8, freed by FreeMibTable
fn table2(out: u32, row: u32, n: usize, fill: impl Fn(u32, usize)) -> u32 {
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let t = mem::heap_alloc(8 + row * n.max(1) as u32, true);
    w32(t, n as u32);
    for i in 0..n {
        fill(t + 8 + row * i as u32, i);
    }
    wp(out, t);
    NO_ERROR
}
fn GetIfTable2(c: &mut Ctx) -> u32 {
    let v = sorted();
    table2(c.a(0), 1352, v.len(), |p, i| fill_ifrow2(p, v[i]))
}
fn GetIfTable2Ex(c: &mut Ctx) -> u32 {
    if c.a(0) > 2 {
        return ERROR_INVALID_PARAMETER;
    }
    let v = sorted();
    table2(c.a(1), 1352, v.len(), |p, i| fill_ifrow2(p, v[i]))
}

// ---- IPv4 address and route tables ----

fn GetIpAddrTable(c: &mut Ctx) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    let v: Vec<&Iface> = sorted().into_iter().filter(|f| f.v4.is_some()).collect();
    table(buf, size, 4 + 24 * v.len() as u32, |b| {
        w32(b, v.len() as u32);
        for (i, f) in v.iter().enumerate() {
            let r = b + 4 + 24 * i as u32;
            let (a, len) = f.v4.unwrap();
            mem::write(r, &a);
            w32(r + 4, f.index);
            w32(r + 8, v4_mask(len));
            w32(r + 12, 1); // dwBCastAddr: the broadcast address is all ones
            w32(r + 16, 65535);
            w16(r + 22, 1); // MIB_IPADDR_PRIMARY
        }
    })
}

/// An IPv4 or IPv6 route: (interface, destination, prefix length, next hop)
struct Route {
    iface: &'static Iface,
    dest: Vec<u8>,
    len: u8,
}

/// The routes of the model: the loopback interface's local, broadcast and multicast routes
fn routes(family: u32) -> Vec<Route> {
    let lo = by_index(1).unwrap();
    let mut v = Vec::new();
    if family != AF_INET6 {
        for (d, l) in [([127, 0, 0, 0], 8), ([127, 0, 0, 1], 32), ([127, 255, 255, 255], 32), ([224, 0, 0, 0], 4), ([255, 255, 255, 255], 32)] {
            v.push(Route { iface: lo, dest: d.to_vec(), len: l });
        }
    }
    if family != AF_INET {
        let mut one = [0u8; 16];
        one[15] = 1;
        let mut ff = [0u8; 16];
        ff[0] = 0xff;
        v.push(Route { iface: lo, dest: one.to_vec(), len: 128 });
        v.push(Route { iface: lo, dest: ff.to_vec(), len: 8 });
    }
    v
}

/// MIB_IPFORWARD_ROW2.Metric of the routes, and the interface metric added for MIB_IPFORWARDROW
const ROUTE_METRIC: u32 = 256;

fn fill_forward_row(r: u32, rt: &Route) {
    let dest = u32::from_le_bytes(rt.dest[..4].try_into().unwrap());
    w32(r, dest);
    w32(r + 4, v4_mask(rt.len));
    w32(r + 8, 0); // dwForwardPolicy
    // next hop: for a direct route, the interface's own address
    w32(r + 12, rt.iface.v4.map_or(0, |(a, _)| u32::from_le_bytes(a)));
    w32(r + 16, rt.iface.index);
    w32(r + 20, 3); // MIB_IPROUTE_TYPE_DIRECT
    w32(r + 24, 2); // MIB_IPPROTO_LOCAL
    w32(r + 28, 0); // age
    w32(r + 32, 0); // next hop AS
    w32(r + 36, ROUTE_METRIC + rt.iface.metric);
}

fn GetIpForwardTable(c: &mut Ctx) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    let v = routes(AF_INET);
    table(buf, size, 4 + 56 * v.len() as u32, |b| {
        w32(b, v.len() as u32);
        for (i, rt) in v.iter().enumerate() {
            fill_forward_row(b + 4 + 56 * i as u32, rt);
        }
    })
}

/// A SOCKADDR_INET of `a` (4 or 16 bytes) with `port` and a scope
fn put_sockaddr(p: u32, a: &[u8], port: u16, scope: u32) {
    mem::write(p, &[0; 28]);
    if a.len() == 4 {
        w16(p, AF_INET as u16);
        w16(p + 2, port.to_be());
        mem::write(p + 4, a);
    } else {
        w16(p, AF_INET6 as u16);
        w16(p + 2, port.to_be());
        mem::write(p + 8, a);
        w32(p + 24, scope);
    }
}

fn fill_forward_row2(r: u32, rt: &Route) {
    mem::write(r, &[0; 104]);
    w64(r, rt.iface.luid());
    w32(r + 8, rt.iface.index);
    put_sockaddr(r + 12, &rt.dest, 0, 0);
    w8(r + 40, rt.len);
    put_sockaddr(r + 44, &vec![0; rt.dest.len()], 0, 0);
    w32(r + 76, u32::MAX);
    w32(r + 80, u32::MAX);
    w32(r + 84, ROUTE_METRIC);
    w32(r + 88, 2); // RouteProtocolLocal
    w8(r + 92, 1); // Loopback
    w8(r + 93, 1); // AutoconfigureAddress
    w8(r + 95, 1); // Immortal
    w32(r + 100, 1); // NlroWellKnown
}

fn family_ok(f: u32) -> bool {
    matches!(f, AF_UNSPEC | AF_INET | AF_INET6)
}
fn GetIpForwardTable2(c: &mut Ctx) -> u32 {
    let f = c.a(0) & 0xffff;
    if !family_ok(f) {
        return ERROR_INVALID_PARAMETER;
    }
    let v = routes(f);
    table2(c.a(1), 104, v.len(), |p, i| fill_forward_row2(p, &v[i]))
}

fn GetIpNetTable(c: &mut Ctx) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    if size == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    // no neighbours: an empty table
    let _ = buf;
    w32(size, 0);
    ERROR_NO_DATA
}
fn GetIpNetTable2(c: &mut Ctx) -> u32 {
    if !family_ok(c.a(0) & 0xffff) {
        return ERROR_INVALID_PARAMETER;
    }
    table2(c.a(1), 88, 0, |_, _| {})
}

// ---- statistics ----

fn stats(c: &mut Ctx, size: u32, fill: impl FnOnce(u32)) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    mem::write(p, &vec![0; size as usize]);
    fill(p);
    NO_ERROR
}
fn stats_ex(c: &mut Ctx, size: u32, fill: impl FnOnce(u32)) -> u32 {
    let (p, f) = (c.a(0), c.a(1));
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    if f != AF_INET && f != AF_INET6 {
        return ERROR_INVALID_PARAMETER;
    }
    mem::write(p, &vec![0; size as usize]);
    fill(p);
    NO_ERROR
}
fn ip_stats(p: u32, family: u32) {
    w32(p, 2); // dwForwarding: MIB_IP_NOT_FORWARDING
    w32(p + 4, 128); // dwDefaultTTL
    w32(p + 76, ifaces().len() as u32); // dwNumIf
    w32(p + 80, ifaces().iter().filter(|f| if family == AF_INET6 { f.v6.is_some() } else { f.v4.is_some() }).count() as u32);
    w32(p + 84, routes(family).len() as u32);
}
fn tcp_stats(p: u32) {
    w32(p, 3); // dwRtoAlgorithm: MIB_TCP_RTO_VANJ... (Windows reports 3)
    w32(p + 4, 5); // RtoMin
    w32(p + 8, u32::MAX); // RtoMax
    w32(p + 12, u32::MAX); // MaxConn: dynamic
}

// ---- TCP / UDP tables: this process's sockets ----

/// The layout of a TCP / UDP table's rows
#[derive(Clone, Copy, PartialEq, Debug)]
enum RowKind {
    /// MIB_TCPROW / MIB_TCP6ROW / MIB_UDPROW / MIB_UDP6ROW
    Basic,
    /// *_OWNER_PID
    Pid,
    /// *_OWNER_MODULE
    Module,
    /// MIB_TCPROW2 / MIB_TCP6ROW2
    Row2,
}

/// (header size, row size) of a table
fn row_layout(tcp: bool, v6: bool, k: RowKind) -> (u32, u32) {
    use RowKind::*;
    let row = match (tcp, v6, k) {
        (true, false, Basic) => 20,
        (true, false, Pid) => 24,
        (true, false, Module) => 160,
        (true, false, Row2) => 28,
        (true, true, Basic) => 52,
        (true, true, Pid) => 56,
        (true, true, Module) => 192,
        (true, true, Row2) => 60,
        (false, false, Basic) => 8,
        (false, false, Pid) => 12,
        (false, false, Module) => 160,
        (false, true, Basic) => 24,
        (false, true, Pid) => 28,
        (false, true, Module) => 176,
        (false, _, Row2) => unreachable!(),
    };
    // the module rows hold a LARGE_INTEGER, so their tables are 8-aligned
    (if k == Module { 8 } else { 4 }, row)
}

/// A DWORD port field: the port in network order in the low word
fn put_port(p: u32, port: u16) {
    mem::write(p, &[(port >> 8) as u8, port as u8, 0, 0]);
}
fn addr16(a: &crate::ws2name::Addr) -> [u8; 16] {
    let mut b = [0; 16];
    if a.ip.len() == 16 {
        b.copy_from_slice(&a.ip);
    }
    b
}
fn addr4(a: &crate::ws2name::Addr) -> [u8; 4] {
    let mut b = [0; 4];
    if a.ip.len() == 4 {
        b.copy_from_slice(&a.ip);
    }
    b
}

/// The rows of a table: this family's sockets of that kind, which `class_ok` keeps, in
/// order (local address, local port, remote address, remote port)
fn conn_rows(tcp: bool, v6: bool, keep: impl Fn(&crate::ws2::ConnRow) -> bool) -> Vec<crate::ws2::ConnRow> {
    let af = if v6 { AF_INET6 } else { AF_INET };
    let mut v: Vec<_> = crate::ws2::conn_rows().into_iter().filter(|r| r.tcp == tcp && r.af == af && keep(r)).collect();
    let key = |r: &crate::ws2::ConnRow| {
        let rem = r.remote.clone().unwrap_or(crate::ws2name::Addr { ip: vec![0; if v6 { 16 } else { 4 }], port: 0, flow: 0, scope: 0 });
        (r.local.ip.clone(), r.local.port, rem.ip, rem.port)
    };
    v.sort_by_key(key);
    v
}

/// Writes one row
fn put_row(p: u32, tcp: bool, v6: bool, k: RowKind, r: &crate::ws2::ConnRow) {
    let none = crate::ws2name::Addr { ip: Vec::new(), port: 0, flow: 0, scope: 0 };
    let rem = r.remote.as_ref().unwrap_or(&none);
    let pid = crate::pid();
    match (tcp, v6) {
        (true, false) => {
            w32(p, r.state);
            mem::write(p + 4, &addr4(&r.local));
            put_port(p + 8, r.local.port);
            mem::write(p + 12, &addr4(rem));
            put_port(p + 16, rem.port);
            if k != RowKind::Basic {
                w32(p + 20, pid);
            }
        }
        (true, true) => {
            // MIB_TCP6ROW puts the state first; the others put it after the addresses
            let o = if k == RowKind::Basic { 4 } else { 0 };
            if k == RowKind::Basic {
                w32(p, r.state);
            }
            mem::write(p + o, &addr16(&r.local));
            w32(p + o + 16, r.local.scope);
            put_port(p + o + 20, r.local.port);
            mem::write(p + o + 24, &addr16(rem));
            w32(p + o + 40, rem.scope);
            put_port(p + o + 44, rem.port);
            if k != RowKind::Basic {
                w32(p + 48, r.state);
                w32(p + 52, pid);
            }
        }
        (false, false) => {
            mem::write(p, &addr4(&r.local));
            put_port(p + 4, r.local.port);
            if k != RowKind::Basic {
                w32(p + 8, pid);
            }
        }
        (false, true) => {
            mem::write(p, &addr16(&r.local));
            w32(p + 16, r.local.scope);
            put_port(p + 20, r.local.port);
            if k != RowKind::Basic {
                w32(p + 24, pid);
            }
        }
    }
}

/// A TCP / UDP table into (buf, &size)
fn conn_table(buf: u32, size: u32, tcp: bool, v6: bool, k: RowKind, keep: impl Fn(&crate::ws2::ConnRow) -> bool) -> u32 {
    let rows = conn_rows(tcp, v6, keep);
    let (hdr, row) = row_layout(tcp, v6, k);
    table(buf, size, hdr + row * rows.len() as u32, |b| {
        w32(b, rows.len() as u32);
        for (i, r) in rows.iter().enumerate() {
            put_row(b + hdr + row * i as u32, tcp, v6, k, r);
        }
    })
}

/// Fills a table at `b` (when not 0); its size
fn conn_fill(b: u32, tcp: bool, v6: bool, k: RowKind) -> u32 {
    let rows = conn_rows(tcp, v6, |_| true);
    let (hdr, row) = row_layout(tcp, v6, k);
    if b != 0 {
        w32(b, rows.len() as u32);
        for (i, r) in rows.iter().enumerate() {
            put_row(b + hdr + row * i as u32, tcp, v6, k, r);
        }
    }
    hdr + row * rows.len() as u32
}

/// GetTcpTable / GetTcp6Table / GetTcpTable2 / GetTcp6Table2 / GetUdpTable / GetUdp6Table
/// (buffer, &size, order)
fn plain_table(c: &mut Ctx, tcp: bool, v6: bool, k: RowKind) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    conn_table(buf, size, tcp, v6, k, |_| true)
}
/// GetExtendedTcpTable / GetExtendedUdpTable(buffer, &size, order, family, class, reserved)
fn GetExtendedTable(c: &mut Ctx, tcp: bool) -> u32 {
    let (buf, size, f, class) = (c.a(0), c.a(1), c.a(3), c.a(4));
    let max_class = if tcp { 8 } else { 2 };
    if size == 0 || (f != AF_INET && f != AF_INET6) || class > max_class {
        return ERROR_INVALID_PARAMETER;
    }
    let v6 = f == AF_INET6;
    if !tcp {
        let k = [RowKind::Basic, RowKind::Pid, RowKind::Module][class as usize];
        return conn_table(buf, size, false, v6, k, |_| true);
    }
    // TCP_TABLE_{BASIC,OWNER_PID,OWNER_MODULE}_{LISTENER,CONNECTIONS,ALL}
    let k = [RowKind::Basic, RowKind::Pid, RowKind::Module][class as usize / 3];
    let which = class % 3;
    conn_table(buf, size, true, v6, k, |r| match which {
        0 => r.state == 2,
        1 => r.state != 2,
        _ => true,
    })
}
/// GetOwnerModuleFromTcpEntry / GetOwnerModuleFromTcp6Entry / ...Udp...(row, class, buffer,
/// &size): the module of the row's owner, which is this process's executable
fn owner_module(c: &mut Ctx, pid_at: u32) -> u32 {
    let (row, class, buf, size) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if row == 0 || size == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match r32(row + pid_at) {
        0 => return ERROR_NOT_FOUND,
        p if p != crate::pid() => return ERROR_INVALID_PARAMETER,
        _ => {}
    }
    if class != 0 {
        // TCPIP_OWNER_MODULE_INFO_BASIC only
        return ERROR_INVALID_PARAMETER;
    }
    let path = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.clone()).unwrap_or_default();
    let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
    let w = |s: &str| s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect::<Vec<u8>>();
    let (nw, pw) = (w(&name), w(&path));
    let hdr = 2 * ps();
    let need = hdr + (nw.len() + pw.len()) as u32;
    if buf == 0 || r32(size) < need {
        w32(size, need);
        return 122; // ERROR_INSUFFICIENT_BUFFER
    }
    let np = buf + hdr;
    let pp = np + nw.len() as u32;
    wp(buf, np);
    wp(buf + ps(), pp);
    mem::write(np, &nw);
    mem::write(pp, &pw);
    NO_ERROR
}
/// AllocateAndGet*TableFromStack(ppTable, bOrder, heap, flags[, family])
fn alloc_from_stack(c: &mut Ctx, fill: impl Fn(u32) -> u32) -> u32 {
    let out = c.a(0);
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let size = fill(0);
    let t = mem::heap_alloc(size.max(4), true);
    fill(t);
    wp(out, t);
    NO_ERROR
}
fn fill_if_table(b: u32) -> u32 {
    let v = sorted();
    if b != 0 {
        w32(b, v.len() as u32);
        for (i, f) in v.iter().enumerate() {
            fill_ifrow(b + 4 + 860 * i as u32, f);
        }
    }
    4 + 860 * v.len() as u32
}
fn fill_addr_table(b: u32) -> u32 {
    let v: Vec<&Iface> = sorted().into_iter().filter(|f| f.v4.is_some()).collect();
    if b != 0 {
        w32(b, v.len() as u32);
        for (i, f) in v.iter().enumerate() {
            let r = b + 4 + 24 * i as u32;
            let (a, len) = f.v4.unwrap();
            mem::write(r, &a);
            w32(r + 4, f.index);
            w32(r + 8, v4_mask(len));
            w32(r + 12, 1);
            w32(r + 16, 65535);
            w16(r + 22, 1);
        }
    }
    4 + 24 * v.len() as u32
}
fn fill_forward_table(b: u32) -> u32 {
    let v = routes(AF_INET);
    if b != 0 {
        w32(b, v.len() as u32);
        for (i, rt) in v.iter().enumerate() {
            fill_forward_row(b + 4 + 56 * i as u32, rt);
        }
    }
    4 + 56 * v.len() as u32
}
fn fill_empty(b: u32) -> u32 {
    if b != 0 {
        w32(b, 0);
    }
    4
}

// ---- unicast addresses and IP interfaces ----

struct Addr {
    iface: &'static Iface,
    a: Vec<u8>,
    len: u8,
}
fn addrs(family: u32) -> Vec<Addr> {
    let mut v = Vec::new();
    for f in sorted() {
        if family != AF_INET6 {
            if let Some((a, l)) = f.v4 {
                v.push(Addr { iface: f, a: a.to_vec(), len: l });
            }
        }
        if family != AF_INET {
            if let Some((a, l)) = f.v6 {
                v.push(Addr { iface: f, a: a.to_vec(), len: l });
            }
        }
    }
    v
}
fn fill_unicast_row(p: u32, a: &Addr) {
    mem::write(p, &[0; 80]);
    put_sockaddr(p, &a.a, 0, 0);
    w64(p + 32, a.iface.luid());
    w32(p + 40, a.iface.index);
    w32(p + 44, 2); // IpPrefixOriginWellKnown
    w32(p + 48, 2); // IpSuffixOriginWellKnown
    w32(p + 52, u32::MAX);
    w32(p + 56, u32::MAX);
    w8(p + 60, a.len);
    w32(p + 64, 4); // IpDadStatePreferred
}
fn GetUnicastIpAddressTable(c: &mut Ctx) -> u32 {
    let f = c.a(0) & 0xffff;
    if !family_ok(f) {
        return ERROR_INVALID_PARAMETER;
    }
    let v = addrs(f);
    table2(c.a(1), 80, v.len(), |p, i| fill_unicast_row(p, &v[i]))
}
/// GetUnicastIpAddressEntry(row): by Address and InterfaceLuid (or InterfaceIndex)
fn GetUnicastIpAddressEntry(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let fam = r16(p) as u32;
    if fam != AF_INET && fam != AF_INET6 {
        return ERROR_INVALID_PARAMETER;
    }
    let (l, i) = (mem::r64(p + 32), r32(p + 40));
    let iface = if l != 0 { by_luid(l) } else { by_index(i) };
    let Some(iface) = iface else { return ERROR_FILE_NOT_FOUND };
    let want = if fam == AF_INET { mem::slice(p + 4, 4).to_vec() } else { mem::slice(p + 8, 16).to_vec() };
    match addrs(fam).into_iter().find(|a| a.iface.index == iface.index && a.a == want) {
        Some(a) => {
            fill_unicast_row(p, &a);
            NO_ERROR
        }
        None => ERROR_NOT_FOUND,
    }
}

fn fill_ipif_row(p: u32, f: &Iface, fam: u32) {
    mem::write(p, &[0; 168]);
    w16(p, fam as u16);
    w64(p + 8, f.luid());
    w32(p + 16, f.index);
    w32(p + 20, if fam == AF_INET { 65535 } else { 0 }); // MaxReassemblySize
    w32(p + 32, 200);
    w32(p + 36, 600);
    w8(p + 44, 1); // UseAutomaticMetric
    if fam == AF_INET6 {
        w8(p + 45, 1);
        w8(p + 46, 1);
        w8(p + 47, 1);
    }
    w32(p + 52, if f.loopback() { 0 } else { 2 }); // RouterDiscoveryBehavior: disabled / DHCP
    w32(p + 56, if fam == AF_INET6 && !f.loopback() { 1 } else { 0 });
    w32(p + 60, 30000);
    w32(p + 64, 1000);
    w32(p + 68, 600000);
    w32(p + 72, if fam == AF_INET { 1 } else { 0 }); // LinkLocalAddressBehavior: delayed / always
    w32(p + 76, if fam == AF_INET { 6000 } else { 0 });
    // ZoneIndices: the interface and link scopes are the interface
    w32(p + 84, f.index);
    w32(p + 88, f.index);
    w32(p + 144, 64); // SitePrefixLength
    w32(p + 148, f.metric);
    w32(p + 152, f.mtu);
    w8(p + 156, f.up as u8);
    w8(p + 158, (!f.loopback()) as u8);
    w8(p + 159, (!f.loopback()) as u8);
    w32(p + 160, 30000);
}
fn GetIpInterfaceTable(c: &mut Ctx) -> u32 {
    let fam = c.a(0) & 0xffff;
    if !family_ok(fam) {
        return ERROR_INVALID_PARAMETER;
    }
    let mut v = Vec::new();
    for fa in [AF_INET, AF_INET6] {
        if fam == AF_UNSPEC || fam == fa {
            for f in sorted() {
                v.push((f, fa));
            }
        }
    }
    table2(c.a(1), 168, v.len(), |p, i| fill_ipif_row(p, v[i].0, v[i].1))
}
fn GetIpInterfaceEntry(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let fam = r16(p) as u32;
    if fam != AF_INET && fam != AF_INET6 {
        return ERROR_INVALID_PARAMETER;
    }
    let (l, i) = (mem::r64(p + 8), r32(p + 16));
    let f = if l != 0 { by_luid(l) } else { by_index(i) };
    match f {
        Some(f) => {
            fill_ipif_row(p, f, fam);
            NO_ERROR
        }
        None => ERROR_NOT_FOUND,
    }
}

// ---- adapters ----

/// IP_ADDR_STRING { Next, IpAddress[16], IpMask[16], Context }
fn put_addr_string(p: u32, a: &str, m: &str, ctx: u32) {
    let k = ps();
    wp(p, 0);
    put_cfixed(p + k, 16, a);
    put_cfixed(p + k + 16, 16, m);
    w32(p + k + 32, ctx);
}

fn adapter_info_size() -> u32 {
    if mem::x64() {
        704
    } else {
        640
    }
}

/// GetAdaptersInfo(buffer, &size): the adapters other than the loopback
fn GetAdaptersInfo(c: &mut Ctx) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    if size == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let v: Vec<&Iface> = ifaces().iter().filter(|f| !f.loopback()).collect();
    if v.is_empty() {
        return ERROR_NO_DATA;
    }
    let one = adapter_info_size();
    let need = one * v.len() as u32;
    if buf == 0 || r32(size) < need {
        w32(size, need);
        return ERROR_BUFFER_OVERFLOW;
    }
    let x64 = mem::x64();
    let o = |a: u32, b: u32| if x64 { b } else { a };
    mem::write(buf, &vec![0; need as usize]);
    for (i, f) in v.iter().enumerate() {
        let p = buf + one * i as u32;
        if i + 1 < v.len() {
            wp(p, p + one);
        }
        w32(p + ps(), f.index); // ComboIndex
        put_cfixed(p + o(8, 12), 260, &guid_string(&f.guid));
        put_cfixed(p + o(268, 272), 132, f.descr);
        w32(p + o(400, 404), f.mac.len() as u32);
        mem::write(p + o(404, 408), f.mac);
        w32(p + o(412, 416), f.index);
        w32(p + o(416, 420), if f.ty == 6 { 6 } else { f.ty }); // MIB_IF_TYPE_ETHERNET
        w32(p + o(420, 424), 1); // DhcpEnabled
        let (a, m) = match f.v4 {
            Some((a, l)) => (format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]), {
                let m = v4_mask(l).to_le_bytes();
                format!("{}.{}.{}.{}", m[0], m[1], m[2], m[3])
            }),
            None => ("0.0.0.0".into(), "0.0.0.0".into()),
        };
        put_addr_string(p + o(428, 440), &a, &m, 0);
        put_addr_string(p + o(468, 488), "0.0.0.0", "0.0.0.0", 0);
        put_addr_string(p + o(508, 536), "", "", 0);
        put_addr_string(p + o(552, 592), "", "", 0);
        put_addr_string(p + o(592, 640), "", "", 0);
    }
    w32(size, need);
    NO_ERROR
}

/// GetAdaptersAddresses(family, flags, reserved, buffer, &size)
fn GetAdaptersAddresses(c: &mut Ctx) -> u32 {
    let (fam, flags, buf, size) = (c.a(0), c.a(1), c.a(3), c.a(4));
    if size == 0 || !family_ok(fam) {
        return ERROR_INVALID_PARAMETER;
    }
    let x64 = mem::x64();
    let o = |a: u32, b: u32| if x64 { b } else { a };
    let (aa, ua, pre) = (o(376, 448), o(48, 64), o(24, 40));
    let skip_unicast = flags & 0x1 != 0;
    let include_prefix = flags & 0x10 != 0;
    // GAA_FLAG_SKIP_FRIENDLY_NAME (0x20) is ignored, as in Windows
    let skip_friendly = false;
    let v = ifaces();
    // layout: the adapters, then each one's addresses, sockaddrs and strings
    let mut need = 0u32;
    let mut plan = Vec::new();
    for f in v {
        let ad = need;
        need += aa;
        let mut uni = Vec::new();
        if !skip_unicast {
            for a in addrs(fam).into_iter().filter(|a| a.iface.index == f.index) {
                uni.push((need, a));
                need += ua + 28;
            }
        }
        let mut pfx = Vec::new();
        if include_prefix {
            for r in routes(fam).into_iter().filter(|r| r.iface.index == f.index && (r.len == 8 || r.len == 128) && r.dest[0] != 0xff) {
                pfx.push((need, r));
                need += pre + 28;
            }
        }
        let name = need;
        need += (guid_string(&f.guid).len() as u32 + 1 + 1) & !1;
        let strs = need;
        let descr_w = 2 * (f.descr.encode_utf16().count() as u32 + 1);
        let alias_w = 2 * (if skip_friendly { 0 } else { f.alias.encode_utf16().count() as u32 } + 1);
        need += 2 + descr_w + alias_w;
        need = (need + 7) & !7;
        plan.push((f, ad, uni, pfx, name, strs, descr_w));
    }
    if buf == 0 || r32(size) < need {
        w32(size, need);
        return ERROR_BUFFER_OVERFLOW;
    }
    mem::write(buf, &vec![0; need as usize]);
    let n = plan.len();
    for (i, (f, ad, uni, pfx, name, strs, descr_w)) in plan.into_iter().enumerate() {
        let p = buf + ad;
        w32(p, aa); // Length
        w32(p + 4, f.index);
        if i + 1 < n {
            // (the next adapter's offset is in the next plan entry: recomputed below)
        }
        put_cfixed(buf + name, 64, &guid_string(&f.guid));
        wp(p + o(12, 16), buf + name);
        let mut prev = 0u32;
        for (off, a) in &uni {
            let u = buf + off;
            w32(u, ua);
            if prev != 0 {
                wp(prev + 8, u);
            } else {
                wp(p + o(16, 24), u);
            }
            prev = u;
            let sa = u + ua;
            let mut sockaddr = [0u8; 28];
            if a.a.len() == 4 {
                sockaddr[0] = 2;
                sockaddr[4..8].copy_from_slice(&a.a);
            } else {
                sockaddr[0] = 23;
                sockaddr[8..24].copy_from_slice(&a.a);
            }
            let sl = if a.a.len() == 4 { 16 } else { 28 };
            mem::write(sa, &sockaddr[..sl]);
            wp(u + o(12, 16), sa);
            w32(u + o(16, 24), sl as u32);
            w32(u + o(20, 32), 2); // IpPrefixOriginWellKnown
            w32(u + o(24, 36), 2);
            w32(u + o(28, 40), 4); // IpDadStatePreferred
            w32(u + o(32, 44), u32::MAX);
            w32(u + o(36, 48), u32::MAX);
            w32(u + o(40, 52), u32::MAX);
            w8(u + o(44, 56), a.len);
        }
        let mut prev = 0u32;
        for (off, r) in &pfx {
            let q = buf + off;
            w32(q, pre);
            if prev != 0 {
                wp(prev + 8, q);
            } else {
                wp(p + o(140, 176), q);
            }
            prev = q;
            let sa = q + pre;
            let sl = if r.dest.len() == 4 { 16 } else { 28 };
            let mut s = [0u8; 28];
            if r.dest.len() == 4 {
                s[0] = 2;
                s[4..8].copy_from_slice(&r.dest);
            } else {
                s[0] = 23;
                s[8..24].copy_from_slice(&r.dest);
            }
            mem::write(sa, &s[..sl]);
            wp(q + o(12, 16), sa);
            w32(q + o(16, 24), sl as u32);
            w32(q + o(20, 32), r.len as u32);
        }
        // DnsSuffix "", Description, FriendlyName
        let s = buf + strs;
        w16(s, 0);
        wp(p + o(32, 56), s);
        put_wfixed(s + 2, descr_w / 2, f.descr);
        wp(p + o(36, 64), s + 2);
        let fname = s + 2 + descr_w;
        put_wfixed(fname, 257, if skip_friendly { "" } else { f.alias });
        wp(p + o(40, 72), fname);
        mem::write(p + o(44, 80), f.mac);
        w32(p + o(52, 88), f.mac.len() as u32);
        // Flags: DDNS enabled, DHCP (the Ethernet), IPv4 and IPv6 enabled
        w32(p + o(56, 92), 0x1 | if f.loopback() { 0 } else { 0x4 } | 0x80 | 0x100);
        w32(p + o(60, 96), f.mtu);
        w32(p + o(64, 100), f.ty);
        w32(p + o(68, 104), if f.up { 1 } else { 2 });
        w32(p + o(72, 108), f.index); // Ipv6IfIndex
        w32(p + o(76, 112) + 4, f.index); // ZoneIndices[ScopeLevelInterface]
        w32(p + o(76, 112) + 8, f.index); // ZoneIndices[ScopeLevelLink]
        w64(p + o(144, 184), f.speed);
        w64(p + o(152, 192), f.speed);
        w32(p + o(168, 216), f.metric);
        w32(p + o(172, 220), f.metric);
        w64(p + o(176, 224), f.luid());
        w32(p + o(192, 248), 1); // CompartmentId
        w32(p + o(212, 268), 1); // NET_IF_CONNECTION_DEDICATED
        w32(p + o(216, 272), 0); // TunnelType
    }
    // link the adapters
    let mut offs = Vec::new();
    let mut at = 0u32;
    for f in v {
        offs.push(at);
        let mut sz = aa;
        if !skip_unicast {
            sz += addrs(fam).iter().filter(|a| a.iface.index == f.index).count() as u32 * (ua + 28);
        }
        if include_prefix {
            sz += routes(fam).iter().filter(|r| r.iface.index == f.index && (r.len == 8 || r.len == 128) && r.dest[0] != 0xff).count() as u32 * (pre + 28);
        }
        sz += (guid_string(&f.guid).len() as u32 + 2) & !1;
        sz += 2 + 2 * (f.descr.encode_utf16().count() as u32 + 1) + 2 * (if skip_friendly { 0 } else { f.alias.encode_utf16().count() as u32 } + 1);
        at = (at + sz + 7) & !7;
    }
    for w in offs.windows(2) {
        wp(buf + w[0] + 8, buf + w[1]); // Next, after the Length / IfIndex union
    }
    // (a larger size is left as it was)
    NO_ERROR
}

/// FIXED_INFO: the host name, no domain, no DNS servers
fn GetNetworkParams(c: &mut Ctx) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    if size == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let need = if mem::x64() { 600 } else { 584 };
    if buf == 0 || r32(size) < need {
        w32(size, need);
        return ERROR_BUFFER_OVERFLOW;
    }
    mem::write(buf, &vec![0; need as usize]);
    put_cfixed(buf, 132, "sake");
    let x64 = mem::x64();
    let o = |a: u32, b: u32| if x64 { b } else { a };
    put_addr_string(buf + o(268, 272), "", "", 0);
    w32(buf + o(308, 320), 1); // BROADCAST_NODETYPE
    w32(size, need);
    NO_ERROR
}

/// GetPerAdapterInfo(index, buffer, &size): IP_PER_ADAPTER_INFO, no DNS servers
fn GetPerAdapterInfo(c: &mut Ctx) -> u32 {
    let (i, buf, size) = (c.a(0), c.a(1), c.a(2));
    if size == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let need = if mem::x64() { 64 } else { 52 };
    if buf == 0 || r32(size) < need {
        w32(size, need);
        return ERROR_BUFFER_OVERFLOW;
    }
    let Some(f) = by_index(i) else { return ERROR_INVALID_PARAMETER };
    mem::write(buf, &vec![0; need as usize]);
    w32(buf, (!f.loopback()) as u32); // AutoconfigEnabled
    put_addr_string(buf + if mem::x64() { 16 } else { 12 }, "", "", 0);
    w32(size, need);
    NO_ERROR
}

/// GetInterfaceInfo(IP_INTERFACE_INFO*, &size): the adapters other than the loopback
fn GetInterfaceInfo(c: &mut Ctx) -> u32 {
    let (buf, size) = (c.a(0), c.a(1));
    if size == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let v: Vec<&Iface> = ifaces().iter().filter(|f| !f.loopback()).collect();
    let need = 4 + 260 * v.len() as u32;
    if buf == 0 || r32(size) < need {
        w32(size, need);
        return ERROR_INSUFFICIENT_BUFFER;
    }
    mem::write(buf, &vec![0; need as usize]);
    w32(buf, v.len() as u32);
    for (i, f) in v.iter().enumerate() {
        let p = buf + 4 + 260 * i as u32;
        w32(p, f.index);
        put_wfixed(p + 4, 128, &device_name(f));
    }
    NO_ERROR
}

/// GetAdapterIndex(L"\\DEVICE\\TCPIP_{guid}", &index)
fn GetAdapterIndex(c: &mut Ctx) -> u32 {
    let (n, out) = (c.a(0), c.a(1));
    if n == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let s = String::from_utf16_lossy(&mem::wstr(n));
    match ifaces().iter().find(|f| device_name(f).eq_ignore_ascii_case(&s)) {
        Some(f) => {
            w32(out, f.index);
            NO_ERROR
        }
        None => ERROR_INVALID_PARAMETER,
    }
}

// ---- best interface / route ----

fn reaches(a: &[u8]) -> Option<&'static Iface> {
    let lo = by_index(1);
    if a.len() == 4 && a[0] == 127 {
        return lo;
    }
    if a.len() == 16 && a[..15].iter().all(|&b| b == 0) && a[15] == 1 {
        return lo;
    }
    None
}
fn GetBestInterface(c: &mut Ctx) -> u32 {
    let (a, out) = (c.a(0), c.a(1));
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match reaches(&a.to_le_bytes()) {
        Some(f) => {
            w32(out, f.index);
            NO_ERROR
        }
        None => ERROR_NETWORK_UNREACHABLE,
    }
}
fn GetBestInterfaceEx(c: &mut Ctx) -> u32 {
    let (sa, out) = (c.a(0), c.a(1));
    if sa == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let a = match r16(sa) as u32 {
        AF_INET => mem::slice(sa + 4, 4).to_vec(),
        AF_INET6 => mem::slice(sa + 8, 16).to_vec(),
        _ => return ERROR_INVALID_PARAMETER,
    };
    if bad_scope(sa) {
        return ERROR_INVALID_PARAMETER;
    }
    match reaches(&a) {
        Some(f) => {
            w32(out, f.index);
            NO_ERROR
        }
        None => ERROR_NETWORK_UNREACHABLE,
    }
}
fn GetBestRoute(c: &mut Ctx) -> u32 {
    let (d, out) = (c.a(0), c.a(2));
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let b = d.to_le_bytes();
    if reaches(&b).is_none() {
        return ERROR_NETWORK_UNREACHABLE;
    }
    // the most specific of the loopback's routes
    let rt = routes(AF_INET)
        .into_iter()
        .filter(|r| {
            let m = v4_mask(r.len);
            u32::from_le_bytes(r.dest[..4].try_into().unwrap()) & m == d & m
        })
        .max_by_key(|r| r.len)
        .unwrap();
    mem::write(out, &[0; 56]);
    fill_forward_row(out, &rt);
    NO_ERROR
}
/// GetBestRoute2(luid, index, source, dest, options, bestRoute, bestSource)
fn GetBestRoute2(c: &mut Ctx) -> u32 {
    let (luid, index, dst, row, src) = (c.a(0), c.a(1), c.a(3), c.a(5), c.a(6));
    if dst == 0 || row == 0 || src == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    // (a failure leaves the row zeroed)
    mem::write(row, &[0; 104]);
    let fam = r16(dst) as u32;
    let a = match fam {
        AF_INET => mem::slice(dst + 4, 4).to_vec(),
        AF_INET6 => mem::slice(dst + 8, 16).to_vec(),
        _ => return ERROR_INVALID_PARAMETER,
    };
    if bad_scope(dst) {
        return ERROR_INVALID_PARAMETER;
    }
    // the interface asked for, by LUID or else by index
    let want = match (if luid != 0 { mem::r64(luid) } else { 0 }, index) {
        (0, 0) => None,
        (0, i) => Some(by_index(i).ok_or(ERROR_FILE_NOT_FOUND)),
        (l, _) => Some(by_luid(l).ok_or(ERROR_FILE_NOT_FOUND)),
    };
    let want = match want {
        Some(Err(e)) => return e,
        Some(Ok(f)) => Some(f),
        None => None,
    };
    let Some(f) = reaches(&a) else { return ERROR_NETWORK_UNREACHABLE };
    if want.is_some_and(|w| w.index != f.index) {
        return ERROR_INVALID_PARAMETER;
    }
    let len = if fam == AF_INET { 8 } else { 128 };
    let dest = if fam == AF_INET { vec![127, 0, 0, 0] } else { a.clone() };
    fill_forward_row2(row, &Route { iface: f, dest, len });
    // the best source: the interface's address, as a SOCKADDR_INET
    let ad = addrs(fam).into_iter().find(|x| x.iface.index == f.index).unwrap();
    mem::write(src, &[0; 28]);
    w16(src, fam as u16);
    mem::write(if fam == AF_INET { src + 4 } else { src + 8 }, &ad.a);
    NO_ERROR
}
/// An IPv6 sockaddr whose scope is set on an address that has no scope (not link-local)
fn bad_scope(sa: u32) -> bool {
    if r16(sa) as u32 != AF_INET6 || r32(sa + 24) == 0 {
        return false;
    }
    let a = mem::slice(sa + 8, 2);
    !(a[0] == 0xfe && a[1] & 0xc0 == 0x80)
}

// ---- conversions ----

fn ConvertInterfaceIndexToLuid(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(0), c.a(1));
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    w64(out, 0);
    match by_index(i) {
        Some(f) => {
            w64(out, f.luid());
            NO_ERROR
        }
        None => ERROR_FILE_NOT_FOUND,
    }
}
fn ConvertInterfaceLuidToIndex(c: &mut Ctx) -> u32 {
    let (l, out) = (c.a(0), c.a(1));
    if l == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match by_luid(mem::r64(l)) {
        Some(f) => {
            w32(out, f.index);
            NO_ERROR
        }
        None => {
            w32(out, 0);
            ERROR_FILE_NOT_FOUND
        }
    }
}
fn ConvertInterfaceLuidToGuid(c: &mut Ctx) -> u32 {
    let (l, out) = (c.a(0), c.a(1));
    if l == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match by_luid(mem::r64(l)) {
        Some(f) => {
            mem::write(out, &f.guid);
            NO_ERROR
        }
        None => {
            mem::write(out, &[0; 16]);
            ERROR_FILE_NOT_FOUND
        }
    }
}
fn ConvertInterfaceGuidToLuid(c: &mut Ctx) -> u32 {
    let (g, out) = (c.a(0), c.a(1));
    if g == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let g: [u8; 16] = mem::slice(g, 16).try_into().unwrap();
    match ifaces().iter().find(|f| f.guid == g) {
        Some(f) => {
            w64(out, f.luid());
            NO_ERROR
        }
        None => {
            w64(out, 0);
            ERROR_FILE_NOT_FOUND
        }
    }
}
fn luid_to_name(c: &mut Ctx, wide: bool) -> u32 {
    let (l, buf, len) = (c.a(0), c.a(1), c.a(2));
    if l == 0 || (wide && buf == 0) {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(f) = by_luid(mem::r64(l)) else { return ERROR_FILE_NOT_FOUND };
    let n = if_name(f);
    if buf == 0 || (len as usize) < n.len() + 1 {
        return ERROR_NOT_ENOUGH_MEMORY;
    }
    if wide {
        put_wfixed(buf, len, &n);
    } else {
        put_cfixed(buf, len, &n);
    }
    NO_ERROR
}
fn name_to_luid_guest(c: &mut Ctx, wide: bool) -> u32 {
    let (n, out) = (c.a(0), c.a(1));
    if n == 0 {
        // (the wide form checks for somewhere to put the LUID first)
        if wide {
            if out == 0 {
                return ERROR_INVALID_PARAMETER;
            }
            w64(out, 0);
        }
        return ERROR_INVALID_NAME;
    }
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let s = if wide { String::from_utf16_lossy(&mem::wstr(n)) } else { String::from_utf8_lossy(&mem::cstr(n)).into_owned() };
    w64(out, 0);
    match name_to_luid(&s) {
        Some(l) => {
            w64(out, l);
            NO_ERROR
        }
        None => ERROR_INVALID_NAME,
    }
}
fn ConvertInterfaceAliasToLuid(c: &mut Ctx) -> u32 {
    let (a, out) = (c.a(0), c.a(1));
    if a == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let s = String::from_utf16_lossy(&mem::wstr(a));
    w64(out, 0);
    match ifaces().iter().find(|f| f.alias.eq_ignore_ascii_case(&s)) {
        Some(f) => {
            w64(out, f.luid());
            NO_ERROR
        }
        None => ERROR_INVALID_PARAMETER,
    }
}
fn ConvertInterfaceLuidToAlias(c: &mut Ctx) -> u32 {
    let (l, buf, len) = (c.a(0), c.a(1), c.a(2));
    if l == 0 || buf == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(f) = by_luid(mem::r64(l)) else { return ERROR_FILE_NOT_FOUND };
    if (len as usize) < f.alias.encode_utf16().count() + 1 {
        return ERROR_NOT_ENOUGH_MEMORY;
    }
    put_wfixed(buf, len, f.alias);
    NO_ERROR
}
fn if_nametoindex(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return 0;
    }
    let s = String::from_utf8_lossy(&mem::cstr(c.a(0))).into_owned();
    name_to_luid(&s).and_then(by_luid).map_or(0, |f| f.index)
}
fn if_indextoname(c: &mut Ctx) -> u32 {
    let (i, buf) = (c.a(0), c.a(1));
    match by_index(i) {
        Some(f) if buf != 0 => {
            put_cfixed(buf, 256, &if_name(f));
            buf
        }
        _ => 0,
    }
}
fn ConvertLengthToIpv4Mask(c: &mut Ctx) -> u32 {
    let (len, out) = (c.a(0), c.a(1));
    if len > 32 {
        if out != 0 {
            w32(out, u32::MAX); // INADDR_NONE
        }
        return ERROR_INVALID_PARAMETER;
    }
    if out != 0 {
        w32(out, v4_mask(len as u8));
    }
    NO_ERROR
}
fn guid_to_string(c: &mut Ctx, wide: bool) -> u32 {
    let (g, buf, len) = (c.a(0), c.a(1), c.a(2));
    if g == 0 || buf == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    if len < 39 {
        return ERROR_NOT_ENOUGH_MEMORY;
    }
    let s = guid_string(&mem::slice(g, 16).try_into().unwrap());
    if wide {
        put_wfixed(buf, len, &s);
    } else {
        put_cfixed(buf, len, &s);
    }
    NO_ERROR
}
fn ConvertStringToGuidW(c: &mut Ctx) -> u32 {
    let (s, out) = (c.a(0), c.a(1));
    if s == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match parse_guid(&String::from_utf16_lossy(&mem::wstr(s))) {
        Some(g) => {
            mem::write(out, &g);
            NO_ERROR
        }
        None => ERROR_INVALID_PARAMETER,
    }
}

// ---- ICMP ----

const ERROR_INVALID_NETNAME: u32 = 1214;
const IP_GENERAL_FAILURE: u32 = 11050;

/// Where an echo goes: the destination (4 or 16 bytes), the source given (if any), the data,
/// the reply buffer, and how to complete (event, APC routine and its context)
struct Echo {
    v6: bool,
    dst: Vec<u8>,
    src: Option<Vec<u8>>,
    data: Vec<u8>,
    ttl: Option<u8>,
    reply: u32,
    rsize: u32,
    event: u32,
    apc: u32,
    apc_ctx: u32,
}

/// The reply header's size: ICMP_ECHO_REPLY (x86 28, x64 40) or ICMPV6_ECHO_REPLY (36: a packed
/// IPV6_ADDRESS_EX of 26 bytes, Status at 28, RoundTripTime at 32)
fn reply_hdr(v6: bool) -> u32 {
    if v6 {
        36
    } else if mem::x64() {
        40
    } else {
        28
    }
}

fn local_addr(a: &[u8]) -> bool {
    a.iter().all(|&b| b == 0) || reaches(a).is_some()
}

/// Sends an echo; the result is the number of replies (0 or 1) with the last error set as
/// Windows does. Only loopback answers; anything else times out (there is no network).
fn send_echo(c: &mut Ctx, h: u32, e: Echo) -> u32 {
    let fail = |err| {
        last_error(err);
        0
    };
    if h == 0 || h == u32::MAX || e.reply == 0 || e.rsize == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    if e.dst.iter().all(|&b| b == 0) || e.src.as_ref().is_some_and(|s| !local_addr(s)) {
        return fail(ERROR_INVALID_NETNAME);
    }
    let hdr = reply_hdr(e.v6);
    let n = e.data.len() as u32;
    let answer = reaches(&e.dst).is_some();
    // the status: the reply must hold the header and the echoed data; an IPv6 echo with a
    // TTL of 0 never leaves
    let status = if e.rsize < hdr + if answer { n } else { 0 } || e.v6 && e.ttl == Some(0) {
        IP_GENERAL_FAILURE
    } else if answer {
        0
    } else {
        IP_REQ_TIMED_OUT
    };
    let size = if status == 0 { n } else { 0 };
    mem::write(e.reply, &vec![0; hdr.min(e.rsize) as usize]);
    if e.rsize >= hdr {
        let d = e.reply + hdr;
        if e.v6 {
            // Address { sin6_port, sin6_flowinfo, sin6_addr, sin6_scope_id }, Status, RoundTripTime
            if status != IP_GENERAL_FAILURE {
                mem::write(e.reply + 6, &e.dst);
            }
            w32(e.reply + 28, status);
        } else {
            let k = ps();
            if status != IP_GENERAL_FAILURE {
                mem::write(e.reply, &e.dst);
            }
            w32(e.reply + 4, status);
            w16(e.reply + 12, size as u16);
            w16(e.reply + 14, (status == 0) as u16); // Reserved: the number of replies
            wp(e.reply + 16, if size != 0 { d } else { 0 });
            w8(e.reply + 16 + k, if status == 0 { e.ttl.filter(|&t| t != 0).unwrap_or(128) } else { 0 });
        }
        if size != 0 {
            mem::write(d, &e.data);
        }
    }
    let replies = (status == 0) as u32;
    if e.event == 0 && e.apc == 0 {
        if replies == 0 {
            last_error(status);
        } else {
            last_error(0);
        }
        if !e.v6 {
            w16(e.reply + 14, 0);
        }
        return replies;
    }
    // asynchronous: the echo is answered already, so it completes at once: the event is set,
    // or the APC is queued (APC(context, &iosb, 0)) for this thread's next alertable wait
    if e.event != 0 {
        crate::k32::set_event(e.event);
    } else {
        let k = ps();
        let iosb = mem::heap_alloc(2 * k, true);
        // STATUS_SUCCESS; an IPv6 echo with a TTL of 0 is STATUS_INVALID_PARAMETER
        wp(iosb, if e.v6 && e.ttl == Some(0) { 0xC000_000D } else { 0 });
        wp(iosb + k, hdr + size);
        crate::apc::queue_args(crate::cur().tid, e.apc, &[e.apc_ctx, iosb, 0]);
    }
    let _ = c;
    fail(ERROR_IO_PENDING)
}

fn echo_data(p: u32, n: u32) -> Vec<u8> {
    if p == 0 || n == 0 { Vec::new() } else { mem::slice(p, n).to_vec() }
}
/// IP_OPTION_INFORMATION's Ttl, if options are given
fn echo_ttl(o: u32) -> Option<u8> {
    (o != 0).then(|| mem::r8(o))
}

/// IcmpSendEcho(handle, dest, data, size, options, reply, replySize, timeout)
fn IcmpSendEcho(c: &mut Ctx) -> u32 {
    let (h, d, data, size, o, reply, rsize) = (c.a(0), c.a(1), c.a(2), c.a(3) & 0xffff, c.a(4), c.a(5), c.a(6));
    let e = Echo { v6: false, dst: d.to_le_bytes().to_vec(), src: None, data: echo_data(data, size), ttl: echo_ttl(o), reply, rsize, event: 0, apc: 0, apc_ctx: 0 };
    send_echo(c, h, e)
}
/// IcmpSendEcho2(handle, event, apc, ctx, dest, data, size, options, reply, replySize,
/// timeout); IcmpSendEcho2Ex has the source address before dest
fn IcmpSendEcho2(c: &mut Ctx, ex: bool) -> u32 {
    let s = ex as u32;
    let (h, event, apc, apc_ctx) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let src = ex.then(|| c.a(4)).filter(|&a| a != 0).map(|a| a.to_le_bytes().to_vec());
    let (d, data, size, o, reply, rsize) = (c.a(4 + s), c.a(5 + s), c.a(6 + s) & 0xffff, c.a(7 + s), c.a(8 + s), c.a(9 + s));
    let e = Echo { v6: false, dst: d.to_le_bytes().to_vec(), src, data: echo_data(data, size), ttl: echo_ttl(o), reply, rsize, event, apc, apc_ctx };
    send_echo(c, h, e)
}
/// Icmp6SendEcho2(handle, event, apc, ctx, &src sockaddr_in6, &dst, data, size, options, reply,
/// replySize, timeout)
fn Icmp6SendEcho2(c: &mut Ctx) -> u32 {
    let (h, event, apc, apc_ctx, sa, da) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let (data, size, o, reply, rsize) = (c.a(6), c.a(7) & 0xffff, c.a(8), c.a(9), c.a(10));
    if sa == 0 || da == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let src = mem::slice(sa + 8, 16).to_vec();
    let src = (!src.iter().all(|&b| b == 0)).then_some(src);
    let e = Echo { v6: true, dst: mem::slice(da + 8, 16).to_vec(), src, data: echo_data(data, size), ttl: echo_ttl(o), reply, rsize, event, apc, apc_ctx };
    send_echo(c, h, e)
}
/// IcmpParseReplies(reply, size): the reply count kept in Reserved (which it clears); with
/// none, the status becomes the last error
fn IcmpParseReplies(c: &mut Ctx) -> u32 {
    let reply = c.a(0);
    if reply == 0 {
        return 0;
    }
    let n = r16(reply + 14) as u32;
    w16(reply + 14, 0);
    if n == 0 {
        last_error(r32(reply + 4));
    }
    n
}
fn Icmp6ParseReplies(c: &mut Ctx) -> u32 {
    let reply = c.a(0);
    if reply == 0 {
        return 0;
    }
    match r32(reply + 28) {
        0 => 1,
        s => {
            last_error(s);
            0
        }
    }
}

// ---- notifications ----

/// Pending NotifyAddrChange / NotifyRouteChange overlapped waits: OVERLAPPED -> handle
static PENDING: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());

/// NotifyAddrChange / NotifyRouteChange(&handle, overlapped): no change ever comes; with an
/// OVERLAPPED the call is pending on a new handle until CancelIPChangeNotify
fn NotifyChange(c: &mut Ctx) -> u32 {
    let (hp, ov) = (c.a(0), c.a(1));
    if ov == 0 {
        // (a synchronous wait for a change that never comes would block forever)
        return ERROR_NOT_SUPPORTED;
    }
    let h = crate::obj::add(crate::obj::Obj::Other("ip change notify"));
    if hp != 0 {
        wp(hp, h);
    }
    let ev = crate::mem::rp(ov + 2 * crate::mem::ps() + 8);
    if ev != 0 {
        crate::k32::event_set(ev, false);
    }
    wp(ov, 0x103); // STATUS_PENDING in Internal
    wp(ov + crate::mem::ps(), 0);
    PENDING.lock().unwrap().push((ov, h));
    crate::k32::last_error(ERROR_IO_PENDING);
    ERROR_IO_PENDING
}
/// CancelIPChangeNotify(overlapped): the pending wait ends with STATUS_CANCELLED
fn CancelIPChangeNotify(c: &mut Ctx) -> u32 {
    let ov = c.a(0);
    let mut p = PENDING.lock().unwrap();
    let Some(i) = p.iter().position(|&(o, _)| o == ov && ov != 0) else { return 0 };
    let (_, h) = p.remove(i);
    drop(p);
    crate::iocp::complete(h, ov, 0xC000_0120, 0, false);
    1
}
/// Notify*Change2(family, callback, context, initialNotification, &handle)
fn NotifyChange2(c: &mut Ctx) -> u32 {
    let (fam, cb, ctx, init, out) = (c.a(0) & 0xffff, c.a(1), c.a(2), c.a(3) & 0xff, c.a(4));
    if !family_ok(fam) || cb == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x4d4e_0100);
    wp(out, NEXT.fetch_add(4, std::sync::atomic::Ordering::Relaxed));
    if init != 0 {
        // MibInitialNotification (3), no row
        c.call(cb, &[ctx, 0, 3]);
    }
    NO_ERROR
}

// ---- ParseNetworkString ----

/// NET_STRING_* types
const NS_IPV4_ADDRESS: u32 = 0x1;
const NS_IPV4_SERVICE: u32 = 0x2;
const NS_IPV4_NETWORK: u32 = 0x4;
const NS_IPV6_ADDRESS: u32 = 0x8;
const NS_IPV6_ADDRESS_NO_SCOPE: u32 = 0x10;
const NS_IPV6_SERVICE: u32 = 0x20;
const NS_IPV6_SERVICE_NO_SCOPE: u32 = 0x40;
const NS_IPV6_NETWORK: u32 = 0x80;
const ERROR_INVALID_NETWORK_STRING: u32 = 0;

/// A parsed network string: address, port, prefix length, scope
#[derive(Debug, PartialEq)]
pub enum Net {
    V4([u8; 4], Option<u16>, Option<u8>),
    V6([u8; 16], Option<u16>, Option<u8>, u32),
}

/// Strict dotted IPv4 (four decimal parts of 0-255)
fn v4(s: &str) -> Option<[u8; 4]> {
    let p: Vec<&str> = s.split('.').collect();
    if p.len() != 4 {
        return None;
    }
    let mut a = [0u8; 4];
    for (i, x) in p.iter().enumerate() {
        if x.is_empty() || x.len() > 3 || !x.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        a[i] = x.parse().ok()?;
    }
    Some(a)
}

/// ParseNetworkString's rules for the IP forms (no names): which `types` accept `s`
pub fn parse_net(s: &str, types: u32) -> Option<Net> {
    if let Some(a) = v4(s) {
        if types & NS_IPV4_ADDRESS != 0 {
            return Some(Net::V4(a, None, None));
        }
    }
    if let Some((h, p)) = s.rsplit_once(':') {
        if let (Some(a), Ok(port)) = (v4(h), p.parse::<u16>()) {
            if types & NS_IPV4_SERVICE != 0 && port != 0 && p.bytes().all(|b| b.is_ascii_digit()) {
                return Some(Net::V4(a, Some(port), None));
            }
        }
    }
    if let Some((h, l)) = s.split_once('/') {
        if let (Some(a), Ok(len)) = (v4(h), l.parse::<u8>()) {
            if types & NS_IPV4_NETWORK != 0 && len <= 32 {
                return Some(Net::V4(a, None, Some(len)));
            }
        }
    }
    // IPv6: addr, addr%scope, [addr]:port, [addr%scope]:port, addr/len
    let v6 = |t: &str| -> Option<([u8; 16], u32, bool)> {
        let (a, scope, has) = match t.split_once('%') {
            Some((a, sc)) => (a, sc.parse::<u32>().ok()?, true),
            None => (t, 0, false),
        };
        Some((a.parse::<std::net::Ipv6Addr>().ok()?.octets(), scope, has))
    };
    if let Some((a, sc, has)) = v6(s) {
        if types & NS_IPV6_ADDRESS != 0 || (types & NS_IPV6_ADDRESS_NO_SCOPE != 0 && !has) {
            return Some(Net::V6(a, None, None, sc));
        }
    }
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((inner, port)) = rest.split_once("]:") {
            if let (Some((a, sc, has)), Ok(p)) = (v6(inner), port.parse::<u16>()) {
                if types & NS_IPV6_SERVICE != 0 || (types & NS_IPV6_SERVICE_NO_SCOPE != 0 && !has) {
                    return Some(Net::V6(a, Some(p), None, sc));
                }
            }
        }
    }
    if let Some((h, l)) = s.split_once('/') {
        if let (Ok(a), Ok(len)) = (h.parse::<std::net::Ipv6Addr>(), l.parse::<u8>()) {
            if types & NS_IPV6_NETWORK != 0 && len <= 128 {
                return Some(Net::V6(a.octets(), None, Some(len), 0));
            }
        }
    }
    None
}

/// ParseNetworkString(string, types, NET_ADDRESS_INFO*, &port, &prefixLength)
fn ParseNetworkString(c: &mut Ctx) -> u32 {
    let (s, types, info, port, plen) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if s == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let st = String::from_utf16_lossy(&mem::wstr(s));
    let _ = ERROR_INVALID_NETWORK_STRING;
    let Some(n) = parse_net(&st, types) else {
        // a failure leaves only the format set (NET_ADDRESS_FORMAT_UNSPECIFIED)
        if info != 0 {
            w32(info, 0);
        }
        return ERROR_INVALID_PARAMETER;
    };
    // NET_ADDRESS_INFO { Format, union { NamedAddress { Address[256], Port[6] }, Ipv4Address,
    // Ipv6Address, IpAddress } }
    let (p, l) = match &n {
        Net::V4(a, p, l) => {
            if info != 0 {
                w32(info, 2); // NET_ADDRESS_IPV4
                put_sockaddr(info + 4, a, p.unwrap_or(0), 0);
                // (a SOCKADDR_IN is 16 bytes)
                mem::write(info + 20, &[0; 12]);
            }
            (*p, *l)
        }
        Net::V6(a, p, l, sc) => {
            if info != 0 {
                w32(info, 3); // NET_ADDRESS_IPV6
                put_sockaddr(info + 4, a, p.unwrap_or(0), *sc);
            }
            (*p, *l)
        }
    };
    if port != 0 {
        w16(port, p.unwrap_or(0));
    }
    if plen != 0 {
        w8(plen, l.unwrap_or(255));
    }
    NO_ERROR
}

/// CreateSortedAddressPairs(sources, nSources, dests, nDests, options, &pairs, &count): each
/// destination with the loopback address of its family as the source, in the given order
fn CreateSortedAddressPairs(c: &mut Ctx) -> u32 {
    let (src, nsrc, dst, n, opts, out, count) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    if src != 0 || nsrc != 0 || dst == 0 || n == 0 || opts != 0 || out == 0 || count == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let k = ps();
    let pair = 2 * k;
    let t = mem::heap_alloc(pair * n + 28 * 2 * n, true);
    for i in 0..n {
        let d = dst + 28 * i;
        let ps_ = t + pair * i;
        let sa_src = t + pair * n + 56 * i;
        let sa_dst = sa_src + 28;
        let v = mem::slice(d, 28).to_vec();
        mem::write(sa_dst, &v);
        // the source: ::1 (or ::ffff:127.0.0.1 for a mapped IPv4 destination)
        let mut s = [0u8; 28];
        s[0] = 23;
        let a = &v[8..24];
        if a[..10].iter().all(|&b| b == 0) && a[10] == 0xff && a[11] == 0xff {
            s[18] = 0xff;
            s[19] = 0xff;
            s[20..24].copy_from_slice(&[127, 0, 0, 1]);
        } else {
            s[23] = 1;
        }
        mem::write(sa_src, &s);
        wp(ps_, sa_src);
        wp(ps_ + k, sa_dst);
    }
    wp(out, t);
    w32(count, n);
    NO_ERROR
}

pub fn init() {
    let d = "iphlpapi";
    implement(d, "GetNumberOfInterfaces", GetNumberOfInterfaces);
    implement(d, "GetIfEntry", GetIfEntry);
    implement(d, "GetIfTable", GetIfTable);
    implement(d, "GetIfEntry2", GetIfEntry2);
    implement(d, "GetIfEntry2Ex", |c| {
        let p = c.a(1);
        if p == 0 {
            return ERROR_INVALID_PARAMETER;
        }
        let (l, i) = (mem::r64(p), r32(p + 8));
        match if l != 0 { by_luid(l) } else { by_index(i) } {
            Some(f) => {
                fill_ifrow2(p, f);
                NO_ERROR
            }
            None => ERROR_FILE_NOT_FOUND,
        }
    });
    implement(d, "GetIfTable2", GetIfTable2);
    implement(d, "GetIfTable2Ex", GetIfTable2Ex);
    implement(d, "FreeMibTable", |c| {
        if c.a(0) != 0 {
            mem::heap_free(c.a(0));
        }
        0
    });
    implement(d, "GetIpAddrTable", GetIpAddrTable);
    implement(d, "GetIpForwardTable", GetIpForwardTable);
    implement(d, "GetIpForwardTable2", GetIpForwardTable2);
    implement(d, "GetIpNetTable", GetIpNetTable);
    implement(d, "GetIpNetTable2", GetIpNetTable2);
    implement(d, "GetIcmpStatistics", |c| stats(c, 104, |_| {}));
    implement(d, "GetIcmpStatisticsEx", |c| stats_ex(c, 2064, |_| {}));
    implement(d, "GetIpStatistics", |c| stats(c, 92, |p| ip_stats(p, AF_INET)));
    implement(d, "GetIpStatisticsEx", |c| {
        let f = c.a(1);
        stats_ex(c, 92, |p| ip_stats(p, f))
    });
    implement(d, "SetIpStatistics", |_| ERROR_NOT_SUPPORTED);
    implement(d, "GetTcpStatistics", |c| stats(c, 60, tcp_stats));
    implement(d, "GetTcpStatisticsEx", |c| stats_ex(c, 60, tcp_stats));
    implement(d, "GetUdpStatistics", |c| stats(c, 20, |_| {}));
    implement(d, "GetUdpStatisticsEx", |c| stats_ex(c, 20, |_| {}));
    implement(d, "GetTcpTable", |c| plain_table(c, true, false, RowKind::Basic));
    implement(d, "GetTcp6Table", |c| plain_table(c, true, true, RowKind::Basic));
    implement(d, "GetTcpTable2", |c| plain_table(c, true, false, RowKind::Row2));
    implement(d, "GetTcp6Table2", |c| plain_table(c, true, true, RowKind::Row2));
    implement(d, "GetUdpTable", |c| plain_table(c, false, false, RowKind::Basic));
    implement(d, "GetUdp6Table", |c| plain_table(c, false, true, RowKind::Basic));
    implement(d, "GetExtendedTcpTable", |c| GetExtendedTable(c, true));
    implement(d, "GetExtendedUdpTable", |c| GetExtendedTable(c, false));
    implement(d, "AllocateAndGetIfTableFromStack", |c| alloc_from_stack(c, fill_if_table));
    implement(d, "AllocateAndGetIpAddrTableFromStack", |c| alloc_from_stack(c, fill_addr_table));
    implement(d, "AllocateAndGetIpForwardTableFromStack", |c| alloc_from_stack(c, fill_forward_table));
    implement(d, "AllocateAndGetIpNetTableFromStack", |c| alloc_from_stack(c, fill_empty));
    implement(d, "AllocateAndGetTcpTableFromStack", |c| alloc_from_stack(c, |b| conn_fill(b, true, false, RowKind::Basic)));
    implement(d, "AllocateAndGetUdpTableFromStack", |c| alloc_from_stack(c, |b| conn_fill(b, false, false, RowKind::Basic)));
    implement(d, "AllocateAndGetTcpExTableFromStack", |c| {
        let f = c.a(4);
        if f != AF_INET && f != AF_INET6 {
            return ERROR_INVALID_PARAMETER;
        }
        if f == AF_INET6 {
            // (Windows has no IPv6 form of this one)
            return ERROR_NOT_SUPPORTED;
        }
        alloc_from_stack(c, |b| conn_fill(b, true, false, RowKind::Pid))
    });
    implement(d, "GetUnicastIpAddressTable", GetUnicastIpAddressTable);
    implement(d, "GetUnicastIpAddressEntry", GetUnicastIpAddressEntry);
    implement(d, "GetAnycastIpAddressTable", |c| {
        if !family_ok(c.a(0) & 0xffff) {
            return ERROR_INVALID_PARAMETER;
        }
        table2(c.a(1), 48, 0, |_, _| {})
    });
    implement(d, "GetIpInterfaceTable", GetIpInterfaceTable);
    implement(d, "GetIpInterfaceEntry", GetIpInterfaceEntry);
    implement(d, "GetAdaptersInfo", GetAdaptersInfo);
    implement(d, "GetAdaptersAddresses", GetAdaptersAddresses);
    implement(d, "GetNetworkParams", GetNetworkParams);
    implement(d, "GetPerAdapterInfo", GetPerAdapterInfo);
    implement(d, "GetInterfaceInfo", GetInterfaceInfo);
    implement(d, "GetAdapterIndex", GetAdapterIndex);
    implement(d, "GetFriendlyIfIndex", |c| c.a(0));
    implement(d, "GetUniDirectionalAdapterInfo", |c| {
        let (b, size) = (c.a(0), c.a(1));
        if size == 0 {
            return ERROR_INVALID_PARAMETER;
        }
        if b == 0 || r32(size) < 8 {
            w32(size, 8);
            return ERROR_MORE_DATA;
        }
        w32(b, 0);
        NO_ERROR
    });
    implement(d, "GetBestInterface", GetBestInterface);
    implement(d, "GetBestInterfaceEx", GetBestInterfaceEx);
    implement(d, "GetBestRoute", GetBestRoute);
    implement(d, "GetBestRoute2", GetBestRoute2);
    implement(d, "ConvertInterfaceIndexToLuid", ConvertInterfaceIndexToLuid);
    implement(d, "ConvertInterfaceLuidToIndex", ConvertInterfaceLuidToIndex);
    implement(d, "ConvertInterfaceLuidToGuid", ConvertInterfaceLuidToGuid);
    implement(d, "ConvertInterfaceGuidToLuid", ConvertInterfaceGuidToLuid);
    implement(d, "ConvertInterfaceLuidToNameW", |c| luid_to_name(c, true));
    implement(d, "ConvertInterfaceLuidToNameA", |c| luid_to_name(c, false));
    implement(d, "ConvertInterfaceNameToLuidW", |c| name_to_luid_guest(c, true));
    implement(d, "ConvertInterfaceNameToLuidA", |c| name_to_luid_guest(c, false));
    implement(d, "ConvertInterfaceAliasToLuid", ConvertInterfaceAliasToLuid);
    implement(d, "ConvertInterfaceLuidToAlias", ConvertInterfaceLuidToAlias);
    implement(d, "if_nametoindex", if_nametoindex);
    implement(d, "if_indextoname", if_indextoname);
    implement(d, "ConvertLengthToIpv4Mask", ConvertLengthToIpv4Mask);
    implement(d, "ConvertGuidToStringA", |c| guid_to_string(c, false));
    implement(d, "ConvertGuidToStringW", |c| guid_to_string(c, true));
    implement(d, "ConvertStringToGuidW", ConvertStringToGuidW);
    implement(d, "ParseNetworkString", ParseNetworkString);
    implement(d, "CreateSortedAddressPairs", CreateSortedAddressPairs);
    implement(d, "GetCurrentThreadCompartmentId", |_| 1);
    implement(d, "SetCurrentThreadCompartmentId", |c| if c.a(0) == 1 { NO_ERROR } else { ERROR_INVALID_PARAMETER });
    // ICMP
    static ICMP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x1c40);
    implement(d, "IcmpCreateFile", |_| ICMP.fetch_add(4, std::sync::atomic::Ordering::Relaxed));
    implement(d, "Icmp6CreateFile", |_| ICMP.fetch_add(4, std::sync::atomic::Ordering::Relaxed));
    implement(d, "IcmpCloseHandle", |c| (c.a(0) != 0 && c.a(0) != u32::MAX) as u32);
    implement(d, "IcmpSendEcho", IcmpSendEcho);
    implement(d, "IcmpSendEcho2", |c| IcmpSendEcho2(c, false));
    implement(d, "IcmpSendEcho2Ex", |c| IcmpSendEcho2(c, true));
    implement(d, "IcmpParseReplies", IcmpParseReplies);
    implement(d, "Icmp6ParseReplies", Icmp6ParseReplies);
    implement(d, "Icmp6SendEcho2", Icmp6SendEcho2);
    // SetTcpEntry(row): only MIB_TCP_STATE_DELETE_TCB (12) is a change one may ask for; it
    // needs the connection
    implement(d, "SetTcpEntry", |c| {
        let r = c.a(0);
        if r == 0 || r32(r) != 12 {
            ERROR_INVALID_PARAMETER
        } else {
            317 // ERROR_MR_MID_NOT_FOUND
        }
    });
    // notifications
    implement(d, "NotifyAddrChange", NotifyChange);
    implement(d, "NotifyRouteChange", NotifyChange);
    implement(d, "CancelIPChangeNotify", CancelIPChangeNotify);
    for n in ["NotifyIpInterfaceChange", "NotifyRouteChange2", "NotifyUnicastIpAddressChange"] {
        implement(d, n, NotifyChange2);
    }
    implement(d, "CancelMibChangeNotify2", |c| if c.a(0) == 0 { ERROR_INVALID_HANDLE } else { NO_ERROR });
    // changes to the configuration are refused (no administrator, no network to change)
    for n in [
        "AddIPAddress", "DeleteIPAddress", "CreateIpForwardEntry", "DeleteIpForwardEntry", "SetIpForwardEntry", "CreateIpNetEntry", "DeleteIpNetEntry",
        "SetIpNetEntry", "FlushIpNetTable", "CreateProxyArpEntry", "DeleteProxyArpEntry", "SetIfEntry", "SetIpTTL", "EnableRouter",
        "UnenableRouter", "IpReleaseAddress", "IpRenewAddress", "SetPerTcpConnectionEStats", "SetPerTcp6ConnectionEStats",
    ] {
        implement(d, n, |_| ERROR_NOT_SUPPORTED);
    }
    implement(d, "SendARP", |_| 67); // ERROR_BAD_NET_NAME: nothing answers
    implement(d, "GetRTTAndHopCount", |c| {
        let _ = c;
        0
    });
    for n in ["GetPerTcpConnectionEStats", "GetPerTcp6ConnectionEStats"] {
        implement(d, n, |_| ERROR_NOT_FOUND);
    }
    implement(d, "GetOwnerModuleFromTcpEntry", |c| owner_module(c, 20));
    implement(d, "GetOwnerModuleFromTcp6Entry", |c| owner_module(c, 52));
    implement(d, "GetOwnerModuleFromUdpEntry", |c| owner_module(c, 8));
    implement(d, "GetOwnerModuleFromUdp6Entry", |c| owner_module(c, 24));
}
const ERROR_MORE_DATA: u32 = 234;
const ERROR_INVALID_HANDLE: u32 = 6;

#[cfg(test)]
mod tests {
    use super::*;

    /// The row sizes of the TCP / UDP tables (the SDK's structures, x86 and x64 alike)
    #[test]
    fn conn_row_sizes() {
        use RowKind::*;
        let tcp4 = [(Basic, 20), (Pid, 24), (Module, 160), (Row2, 28)];
        let tcp6 = [(Basic, 52), (Pid, 56), (Module, 192), (Row2, 60)];
        for (k, n) in tcp4 {
            assert_eq!(row_layout(true, false, k).1, n, "{k:?}");
        }
        for (k, n) in tcp6 {
            assert_eq!(row_layout(true, true, k).1, n, "{k:?}");
        }
        assert_eq!(row_layout(false, false, Module), (8, 160));
        assert_eq!(row_layout(false, true, Module), (8, 176));
        assert_eq!(row_layout(false, true, Pid), (4, 28));
        // the module rows hold a LARGE_INTEGER: 8-aligned
        for k in [Basic, Pid, Module] {
            for v6 in [false, true] {
                for tcp in [false, true] {
                    let (h, r) = row_layout(tcp, v6, k);
                    assert_eq!(r % if k == Module { 8 } else { 4 }, 0);
                    assert_eq!(h, if k == Module { 8 } else { 4 });
                }
            }
        }
    }

    #[test]
    fn guid_round_trip() {
        for f in ifaces() {
            let s = guid_string(&f.guid);
            assert_eq!(s.len(), 38);
            assert_eq!(parse_guid(&s), Some(f.guid));
            assert_eq!(parse_guid(&s.to_lowercase()), Some(f.guid));
        }
        assert_eq!(parse_guid("{00000001-0000-0000-0000-00000000000}"), None);
        assert_eq!(parse_guid("00000001-0000-0000-0000-000000000000"), None);
        assert_eq!(parse_guid("{0000000g-0000-0000-0000-000000000000}"), None);
    }

    #[test]
    fn names() {
        for f in ifaces() {
            assert_eq!(name_to_luid(&if_name(f)), Some(f.luid()));
        }
        assert_eq!(if_name(&ifaces()[1]), "loopback_0");
        assert_eq!(if_name(&ifaces()[0]), "ethernet_32769");
        assert_eq!(name_to_luid("nosuch_1"), None);
        assert_eq!(name_to_luid("ethernet_x"), None);
    }

    #[test]
    fn masks() {
        assert_eq!(v4_mask(0), 0);
        assert_eq!(v4_mask(8).to_le_bytes(), [255, 0, 0, 0]);
        assert_eq!(v4_mask(24).to_le_bytes(), [255, 255, 255, 0]);
        assert_eq!(v4_mask(32), u32::MAX);
    }

    #[test]
    fn network_strings() {
        assert_eq!(parse_net("192.168.1.1", NS_IPV4_ADDRESS), Some(Net::V4([192, 168, 1, 1], None, None)));
        assert_eq!(parse_net("192.168.1.1:80", NS_IPV4_SERVICE), Some(Net::V4([192, 168, 1, 1], Some(80), None)));
        // port 0 is no service
        assert_eq!(parse_net("1.2.3.4:0", NS_IPV4_SERVICE), None);
        assert_eq!(parse_net("192.168.1.1:80", NS_IPV4_ADDRESS), None);
        assert_eq!(parse_net("192.168.1.256", NS_IPV4_ADDRESS), None);
        assert_eq!(parse_net("1.2.3.4/24", NS_IPV4_NETWORK), Some(Net::V4([1, 2, 3, 4], None, Some(24))));
        assert_eq!(parse_net("1.2.3.4/33", NS_IPV4_NETWORK), None);
        let one = { let mut a = [0u8; 16]; a[15] = 1; a };
        assert_eq!(parse_net("::1", NS_IPV6_ADDRESS), Some(Net::V6(one, None, None, 0)));
        assert_eq!(parse_net("::1%5", NS_IPV6_ADDRESS_NO_SCOPE), None);
        assert_eq!(parse_net("[::1]:443", NS_IPV6_SERVICE), Some(Net::V6(one, Some(443), None, 0)));
        assert_eq!(parse_net("::1/64", NS_IPV6_NETWORK), Some(Net::V6(one, None, Some(64), 0)));
    }
}
