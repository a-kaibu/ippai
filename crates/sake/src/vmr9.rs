//! The Video Mixing Renderer 9 (CLSID_VideoMixingRenderer9) as a DirectShow filter of its own:
//! the interfaces Windows' has in each rendering mode (windowed, windowless, renderless, mixer
//! mode with its streams), COM aggregation, its input pins ("VMR Input0".., one per stream) with
//! their media types (video, VIDEOINFOHEADER(2), the RGB and YUV formats it takes), connection
//! and disconnection, its memory allocator and samples, and the filter states.
//!
//! One guest block of pointer-sized words holds the filter's interfaces (word k is interface
//! k's vtable); each pin has a block of its own. The IBaseFilter pointer is the key of the
//! picture's placement (dshow.rs's `Evr`): IVMRFilterConfig9 and IVMRWindowlessControl9 sit
//! one and two words after it, as dshow.rs's windowless control calls expect.
//!
//! Behaviour follows Wine's quartz tests (dlls/quartz/tests/vmr9.c) where they say what
//! Windows answers.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::Mutex;

use crate::api::{vtable, Ctx, Handler};
use crate::mem::{self, ps, put32, r32, rp, w32, wp};

const S_FALSE: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_FAIL: u32 = 0x8000_4005;
const E_UNEXPECTED: u32 = 0x8000_ffff;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_PROP_ID_UNSUPPORTED: u32 = 0x8007_0490;
const VFW_S_STATE_INTERMEDIATE: u32 = 0x0004_0237;
const VFW_E_ALREADY_CONNECTED: u32 = 0x8004_0204;
const VFW_E_NOT_CONNECTED: u32 = 0x8004_0209;
const VFW_E_BADALIGN: u32 = 0x8004_020e;
const VFW_E_ALREADY_COMMITTED: u32 = 0x8004_020f;
const VFW_E_BUFFERS_OUTSTANDING: u32 = 0x8004_0210;
const VFW_E_NOT_COMMITTED: u32 = 0x8004_0211;
const VFW_E_SIZENOTSET: u32 = 0x8004_0212;
const VFW_E_NOT_FOUND: u32 = 0x8004_0216;
const VFW_E_NOT_STOPPED: u32 = 0x8004_0224;
const VFW_E_WRONG_STATE: u32 = 0x8004_0227;
const VFW_E_TYPE_NOT_ACCEPTED: u32 = 0x8004_022a;
const VFW_E_TIMEOUT: u32 = 0x8004_022e;
const VFW_E_VMR_NOT_IN_MIXER_MODE: u32 = 0x8004_0296;

type Guid = [u8; 16];

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}
const QZ: [u8; 8] = [0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70];
const COM: [u8; 8] = [0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const IID_IUNKNOWN: Guid = guid(0, 0, 0, COM);
const IID_IPERSIST: Guid = guid(0x0000_010c, 0, 0, COM);
const IID_IMEDIAFILTER: Guid = guid(0x56a8_6899, 0x0ad4, 0x11ce, QZ);
const IID_IBASEFILTER: Guid = guid(0x56a8_6895, 0x0ad4, 0x11ce, QZ);
const IID_IPIN: Guid = guid(0x56a8_6891, 0x0ad4, 0x11ce, QZ);
const IID_IENUMPINS: Guid = guid(0x56a8_6892, 0x0ad4, 0x11ce, QZ);
const IID_IMEMINPUTPIN: Guid = guid(0x56a8_689d, 0x0ad4, 0x11ce, QZ);
const IID_IMEMALLOCATOR: Guid = guid(0x56a8_689c, 0x0ad4, 0x11ce, QZ);
const IID_IMEDIASAMPLE: Guid = guid(0x56a8_689a, 0x0ad4, 0x11ce, QZ);
const IID_IMEDIASAMPLE2: Guid = guid(0x36b7_3884, 0xc2c8, 0x11cf, [0x8b, 0x46, 0x00, 0x80, 0x5f, 0x6c, 0xef, 0x60]);
const IID_IOVERLAY: Guid = guid(0x56a8_68a1, 0x0ad4, 0x11ce, QZ);
const IID_IQUALITYCONTROL: Guid = guid(0x56a8_68a5, 0x0ad4, 0x11ce, QZ);
const IID_IMEDIAPOSITION: Guid = guid(0x56a8_68b2, 0x0ad4, 0x11ce, QZ);
const IID_IVIDEOWINDOW: Guid = guid(0x56a8_68b4, 0x0ad4, 0x11ce, QZ);
const IID_IBASICVIDEO: Guid = guid(0x56a8_68b5, 0x0ad4, 0x11ce, QZ);
const IID_IBASICVIDEO2: Guid = guid(0x329b_b360, 0xf6ea, 0x11d1, [0x90, 0x38, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x98]);
const IID_IMEDIASEEKING: Guid = guid(0x36b7_3880, 0xc2c8, 0x11cf, [0x8b, 0x46, 0x00, 0x80, 0x5f, 0x6c, 0xef, 0x60]);
const IID_IENUMMEDIATYPES: Guid = guid(0x89c3_1040, 0x846b, 0x11ce, [0x97, 0xd3, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a]);
const IID_IAMFILTERMISCFLAGS: Guid = guid(0x2dd7_4950, 0xa890, 0x11d1, [0xab, 0xe8, 0x00, 0xa0, 0xc9, 0x05, 0xf3, 0x75]);
const IID_IAMCERTIFIEDOUTPUTPROTECTION: Guid = guid(0x6fed_ed3e, 0x0ff1, 0x4901, [0xa2, 0xf1, 0x43, 0xf7, 0x01, 0x2c, 0x85, 0x15]);
const IID_IKSPROPERTYSET: Guid = guid(0x31ef_ac30, 0x515c, 0x11d0, [0xa9, 0xaa, 0x00, 0xaa, 0x00, 0x61, 0xbe, 0x93]);
const IID_IQUALPROP: Guid = guid(0x1bd0_ecb0, 0xf8e2, 0x11ce, [0xaa, 0xc6, 0x00, 0x20, 0xaf, 0x0b, 0x99, 0xa3]);
const IID_IAMVIDEOACCELERATOR: Guid = guid(0x256a_6a22, 0xfbad, 0x11d1, [0x82, 0xbf, 0x00, 0xa0, 0xc9, 0x69, 0x6c, 0x8f]);
const IID_IVMRFILTERCONFIG9: Guid = guid(0x5a80_4648, 0x4f66, 0x4867, [0x9c, 0x43, 0x4f, 0x5c, 0x82, 0x2c, 0xf1, 0xb8]);
const IID_IVMRWINDOWLESSCONTROL9: Guid = guid(0x8f53_7d09, 0xf85e, 0x4414, [0xb2, 0x3b, 0x50, 0x2e, 0x54, 0xc7, 0x99, 0x27]);
const IID_IVMRASPECTRATIOCONTROL9: Guid = guid(0x00d9_6c29, 0xbbde, 0x4efc, [0x99, 0x01, 0xbb, 0x50, 0x36, 0x39, 0x21, 0x46]);
const IID_IVMRMIXERBITMAP9: Guid = guid(0xced1_75e5, 0x1935, 0x4820, [0x81, 0xbd, 0xff, 0x6a, 0xd0, 0x0c, 0x91, 0x08]);
const IID_IVMRSURFACEALLOCATORNOTIFY9: Guid = guid(0xdca3_f5df, 0xbb3a, 0x4d03, [0xbd, 0x81, 0x84, 0x61, 0x4b, 0xfb, 0xfa, 0x0c]);
const IID_IVMRMIXERCONTROL9: Guid = guid(0x1a77_7eaa, 0x47c8, 0x4930, [0xb2, 0xc9, 0x8f, 0xee, 0x1c, 0x1b, 0x0f, 0x3b]);
const IID_IVMRMONITORCONFIG9: Guid = guid(0x46c2_e457, 0x8ba0, 0x4eef, [0xb8, 0x0b, 0x06, 0x80, 0xf0, 0x97, 0x87, 0x49]);
const IID_IVMRDEINTERLACECONTROL9: Guid = guid(0xa215_fb8d, 0x13c2, 0x4f7f, [0x99, 0x3c, 0x00, 0x3d, 0x62, 0x71, 0xa4, 0x59]);
/// CLSID_MemoryAllocator {1e651cc0-b199-11d0-8212-00c04fc32c45}: the allocator the VMR-9 hands out
const CLSID_MEMORY_ALLOCATOR: Guid = guid(0x1e65_1cc0, 0xb199, 0x11d0, [0x82, 0x12, 0x00, 0xc0, 0x4f, 0xc3, 0x2c, 0x45]);
pub const CLSID_VMR9: Guid = guid(0x51b4_abf3, 0x748f, 0x4e3b, [0xa2, 0x76, 0xc8, 0x28, 0x33, 0x0e, 0x92, 0x6a]);

const MS: [u8; 8] = [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
const MEDIATYPE_VIDEO: Guid = guid(0x7364_6976, 0, 0x10, MS);
const FORMAT_VIDEOINFO: Guid = guid(0x0558_9f80, 0xc356, 0x11ce, [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a]);
const FORMAT_VIDEOINFO2: Guid = guid(0xf72a_76a0, 0xeb0a, 0x11d0, [0xac, 0xe4, 0x00, 0x00, 0xc0, 0xcc, 0x16, 0xba]);
const RGB: [u8; 8] = [0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70];
/// the subtypes it takes: RGB565, RGB555, RGB24, RGB32, ARGB32 and the YUV ones (FOURCC
/// subtypes: YUY2 UYVY YV12 NV12 IYUV)
fn subtype_ok(g: &Guid) -> bool {
    let rgb = [0xe436_eb7b, 0xe436_eb7c, 0xe436_eb7d, 0xe436_eb7e].iter().any(|&d| *g == guid(d, 0x524f, 0x11ce, RGB));
    let argb = *g == guid(0x773c_9ac0, 0x3274, 0x11d0, [0xb7, 0x24, 0x00, 0xaa, 0x00, 0x6c, 0x1a, 0x01]);
    let fourcc = [b"YUY2", b"UYVY", b"YV12", b"NV12", b"IYUV"].iter().any(|f| *g == guid(u32::from_le_bytes(**f), 0, 0x10, MS));
    rgb || argb || fourcc
}

fn read_guid(p: u32) -> Guid {
    let mut g = [0u8; 16];
    for (i, b) in g.iter_mut().enumerate() {
        *b = mem::r8(p + i as u32);
    }
    g
}
fn put_guid(p: u32, g: &Guid) {
    mem::write(p, g);
}

// ---------------------------------------------------------------------------------------------
// AM_MEDIA_TYPE { majortype, subtype, bFixedSizeSamples, bTemporalCompression, lSampleSize,
// formattype, pUnk, cbFormat, pbFormat }

fn mt_size() -> u32 {
    if mem::x64() { 88 } else { 72 }
}
fn mt_unk() -> u32 {
    if mem::x64() { 64 } else { 60 }
}
fn mt_cb() -> u32 {
    if mem::x64() { 72 } else { 64 }
}
fn mt_pb() -> u32 {
    if mem::x64() { 80 } else { 68 }
}

/// A media type: its fixed part and its format block
#[derive(Clone, Debug, PartialEq)]
pub struct MediaType {
    pub head: Vec<u8>,
    pub format: Vec<u8>,
}
impl MediaType {
    fn read(p: u32) -> MediaType {
        let mut head = vec![0u8; mt_size() as usize];
        for (i, b) in head.iter_mut().enumerate() {
            *b = mem::r8(p + i as u32);
        }
        let (cb, pb) = (r32(p + mt_cb()), rp(p + mt_pb()));
        let format = if pb != 0 && cb < 1 << 20 { (0..cb).map(|i| mem::r8(pb + i)).collect() } else { Vec::new() };
        MediaType { head, format }
    }
    fn major(&self) -> Guid {
        self.head[0..16].try_into().unwrap()
    }
    fn sub(&self) -> Guid {
        self.head[16..32].try_into().unwrap()
    }
    fn formattype(&self) -> Guid {
        self.head[44..60].try_into().unwrap()
    }
    /// into a caller's AM_MEDIA_TYPE: its format block a copy of ours (CoTaskMemAlloc'd)
    fn write(&self, p: u32) {
        mem::write(p, &self.head);
        wp(p + mt_unk(), 0);
        let pb = if self.format.is_empty() { 0 } else { mem::heap_alloc(self.format.len() as u32, false) };
        mem::write(pb, &self.format);
        w32(p + mt_cb(), self.format.len() as u32);
        wp(p + mt_pb(), pb);
    }
    /// the picture size its VIDEOINFOHEADER(2) gives
    pub fn size(&self) -> (i32, i32) {
        let bih = if self.formattype() == FORMAT_VIDEOINFO2 { 72 } else { 48 };
        let at = |o: usize| self.format.get(bih + o..bih + o + 4).map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap()));
        (at(4), at(8).abs())
    }
}

/// Whether the VMR-9 takes a media type: video, a VIDEOINFOHEADER or VIDEOINFOHEADER2, one of
/// the formats it renders (S_OK) or not (S_FALSE)
pub fn accepts(mt: &MediaType) -> bool {
    mt.major() == MEDIATYPE_VIDEO && (mt.formattype() == FORMAT_VIDEOINFO || mt.formattype() == FORMAT_VIDEOINFO2) && subtype_ok(&mt.sub())
}

// ---------------------------------------------------------------------------------------------
// the filter

// its interfaces, by word of its block
const UNK: u32 = 0;
const BF: u32 = 1;
const FC: u32 = 2;
const WL: u32 = 3;
const COP: u32 = 4;
const MISC: u32 = 5;
const MP: u32 = 6;
const MSK: u32 = 7;
const QC: u32 = 8;
const ARC: u32 = 9;
const MB: u32 = 10;
const SAN: u32 = 11;
const MIX: u32 = 12;
const MON: u32 = 13;
const VW: u32 = 14;
const BV: u32 = 15;
const KS: u32 = 16;
const QP: u32 = 17;
const DI: u32 = 18;
const N: u32 = 19;
const CLASS_E_NOAGGREGATION: u32 = 0x8004_0110;
// (dshow's windowless calls find the filter two slots below IVMRWindowlessControl9)
const _: () = assert!(WL - 2 == BF);

const WINDOWED: u32 = 1;
const WINDOWLESS: u32 = 2;
const RENDERLESS: u32 = 4;

struct Vmr {
    refs: u32,
    /// the controlling IUnknown of an aggregate (0: none)
    outer: u32,
    mode: u32,
    mode_set: bool,
    /// mixer mode: its streams (0: not in mixer mode)
    streams: u32,
    /// 0 stopped, 1 paused, 2 running
    state: u32,
    /// a sample came since it was paused (until then a pause is not complete)
    sample: bool,
    clock: u32,
    graph: u32,
    name: Vec<u16>,
    pins: Vec<u32>,
    /// bumped when the pins change: enumerators made before are out of step
    version: u32,
    prefs: u32,
    mixing_prefs: u32,
    aspect: u32,
    background: u32,
    alpha_bitmap: Vec<u8>,
    /// the Direct3D 9 device a renderless VMR's presenter gave it (SetD3DDevice; one reference)
    device: u32,
}

struct Pin {
    filter: u32,
    index: u32,
    peer: u32,
    mt: Option<MediaType>,
    alloc: u32,
    flushing: bool,
}

static VMRS: Mutex<Option<HashMap<u32, Vmr>>> = Mutex::new(None);
static PINS: Mutex<Option<HashMap<u32, Pin>>> = Mutex::new(None);

fn vmr_with<R>(b: u32, f: impl FnOnce(&mut Vmr) -> R) -> Option<R> {
    VMRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
fn pin_with<R>(p: u32, f: impl FnOnce(&mut Pin) -> R) -> Option<R> {
    PINS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&p).map(f)
}

/// a method `slot` of COM object `obj` (in the guest) with `args` after `this`
fn com_call(c: &mut Ctx, obj: u32, slot: u32, args: &[u32]) -> u32 {
    let f = rp(rp(obj) + ps() * slot);
    let mut a = vec![obj];
    a.extend_from_slice(args);
    c.call(f, &a)
}
fn guest_add_ref(c: &mut Ctx, obj: u32) {
    if obj != 0 {
        com_call(c, obj, 1, &[]);
    }
}
fn guest_release(c: &mut Ctx, obj: u32) {
    if obj != 0 {
        com_call(c, obj, 2, &[]);
    }
}

/// The block of interface `k` of the filter `this` points into
fn block(c: &Ctx, k: u32) -> u32 {
    c.a(0) - ps() * k
}

/// the filter's own QueryInterface (the inner one when aggregated)
fn inner_qi(b: u32, iid: &Guid) -> Option<u32> {
    let (mode, streams) = vmr_with(b, |v| (v.mode, v.streams))?;
    let k = match *iid {
        IID_IUNKNOWN => UNK,
        IID_IPERSIST | IID_IMEDIAFILTER | IID_IBASEFILTER => BF,
        IID_IVMRFILTERCONFIG9 => FC,
        IID_IAMCERTIFIEDOUTPUTPROTECTION => COP,
        IID_IAMFILTERMISCFLAGS => MISC,
        IID_IMEDIAPOSITION => MP,
        IID_IMEDIASEEKING => MSK,
        IID_IQUALITYCONTROL => QC,
        IID_IVMRASPECTRATIOCONTROL9 => ARC,
        IID_IVMRMIXERBITMAP9 => MB,
        IID_IKSPROPERTYSET => KS,
        IID_IQUALPROP => QP,
        IID_IVMRDEINTERLACECONTROL9 => DI,
        IID_IVMRWINDOWLESSCONTROL9 if mode == WINDOWLESS => WL,
        IID_IVMRSURFACEALLOCATORNOTIFY9 if mode == RENDERLESS => SAN,
        IID_IVMRMIXERCONTROL9 if streams > 0 => MIX,
        IID_IVMRMONITORCONFIG9 if mode != RENDERLESS => MON,
        IID_IVIDEOWINDOW if mode == WINDOWED => VW,
        IID_IBASICVIDEO | IID_IBASICVIDEO2 if mode == WINDOWED => BV,
        _ => return None,
    };
    Some(b + ps() * k)
}
fn inner_add_ref(b: u32) -> u32 {
    vmr_with(b, |v| {
        v.refs += 1;
        v.refs
    })
    .unwrap_or(1)
}
fn inner_release(c: &mut Ctx, b: u32) -> u32 {
    let n = vmr_with(b, |v| {
        v.refs = v.refs.saturating_sub(1);
        v.refs
    })
    .unwrap_or(0);
    if n == 0 {
        // the pins let their peers and allocators go; the memory stays (a stale pointer still
        // finds a vtable)
        let pins = vmr_with(b, |v| std::mem::take(&mut v.pins)).unwrap_or_default();
        let clock = vmr_with(b, |v| std::mem::take(&mut v.clock)).unwrap_or(0);
        for p in pins {
            let (peer, alloc) = pin_with(p, |p| (std::mem::take(&mut p.peer), std::mem::take(&mut p.alloc))).unwrap_or((0, 0));
            guest_release(c, peer);
            alloc_release(alloc);
            PINS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&p);
        }
        guest_release(c, clock);
        VMRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
    }
    n
}

/// QueryInterface through any interface: the aggregate's, or the filter's own
fn qi(c: &mut Ctx, b: u32) -> u32 {
    let (iid, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let outer = vmr_with(b, |v| v.outer).unwrap_or(0);
    if outer != 0 {
        return com_call(c, outer, 0, &[iid, out]);
    }
    qi_inner_out(c, b, iid, out)
}
fn qi_inner_out(c: &mut Ctx, b: u32, iid: u32, out: u32) -> u32 {
    match inner_qi(b, &read_guid(iid)) {
        Some(p) => {
            // (an interface other than the inner IUnknown counts on the outer object's count)
            if p == b + ps() * UNK {
                inner_add_ref(b);
            } else {
                add_ref(c, b);
            }
            mem::putp(out, p);
            0
        }
        None => {
            mem::putp(out, 0);
            E_NOINTERFACE
        }
    }
}
fn add_ref(c: &mut Ctx, b: u32) -> u32 {
    let outer = vmr_with(b, |v| v.outer).unwrap_or(0);
    if outer != 0 {
        return com_call(c, outer, 1, &[]);
    }
    inner_add_ref(b)
}
fn release(c: &mut Ctx, b: u32) -> u32 {
    let outer = vmr_with(b, |v| v.outer).unwrap_or(0);
    if outer != 0 {
        return com_call(c, outer, 2, &[]);
    }
    inner_release(c, b)
}

macro_rules! iunknown {
    ($k:expr) => {{
        fn q(c: &mut Ctx) -> u32 {
            qi(c, block(c, $k))
        }
        fn a(c: &mut Ctx) -> u32 {
            add_ref(c, block(c, $k))
        }
        fn r(c: &mut Ctx) -> u32 {
            release(c, block(c, $k))
        }
        vec![(q as Handler, 12), (a as Handler, 4), (r as Handler, 4)]
    }};
}

fn ok(_: &mut Ctx) -> u32 {
    0
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}

// the non-delegating IUnknown
fn unk_qi(c: &mut Ctx) -> u32 {
    let (b, iid, out) = (block(c, UNK), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    qi_inner_out(c, b, iid, out)
}
fn unk_add_ref(c: &mut Ctx) -> u32 {
    inner_add_ref(block(c, UNK))
}
fn unk_release(c: &mut Ctx) -> u32 {
    let b = block(c, UNK);
    inner_release(c, b)
}

// IBaseFilter
fn get_class_id(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put_guid(c.a(1), &CLSID_VMR9);
    0
}
fn set_state(c: &mut Ctx, s: u32) -> u32 {
    let b = block(c, BF);
    let old = vmr_with(b, |v| {
        let o = v.state;
        v.state = s;
        if s != 2 && o != 1 {
            v.sample = false;
        }
        if s == 0 {
            v.sample = false;
        }
        o
    })
    .unwrap_or(0);
    if s == 0 && old != 0 {
        // (stopping decommits the allocators and ends any flush)
        for p in vmr_with(b, |v| v.pins.clone()).unwrap_or_default() {
            let a = pin_with(p, |p| {
                p.flushing = false;
                p.alloc
            })
            .unwrap_or(0);
            alloc_with(a, |a| a.committed = false);
        }
    }
    0
}
fn bf_stop(c: &mut Ctx) -> u32 {
    set_state(c, 0)
}
fn bf_pause(c: &mut Ctx) -> u32 {
    set_state(c, 1)
}
fn bf_run(c: &mut Ctx) -> u32 {
    set_state(c, 2)
}
/// GetState(timeout, &state): paused with a connected pin and no sample yet is still on its way
fn bf_get_state(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, BF), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let (s, sample, pins) = vmr_with(b, |v| (v.state, v.sample, v.pins.clone())).unwrap_or((0, false, Vec::new()));
    put32(out, s);
    let connected = pins.iter().any(|&p| pin_with(p, |p| p.peer != 0).unwrap_or(false));
    if s == 1 && connected && !sample { VFW_S_STATE_INTERMEDIATE } else { 0 }
}
fn bf_set_sync(c: &mut Ctx) -> u32 {
    let (b, clock) = (block(c, BF), c.a(1));
    guest_add_ref(c, clock);
    let old = vmr_with(b, |v| std::mem::replace(&mut v.clock, clock)).unwrap_or(0);
    guest_release(c, old);
    0
}
fn bf_get_sync(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, BF), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let clock = vmr_with(b, |v| v.clock).unwrap_or(0);
    guest_add_ref(c, clock);
    mem::putp(out, clock);
    0
}
fn bf_enum_pins(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, BF), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let e = enum_pins_new(c, b, 0);
    mem::putp(out, e);
    0
}
fn pin_name(i: u32) -> String {
    format!("VMR Input{i}")
}
fn bf_find_pin(c: &mut Ctx) -> u32 {
    let (b, id, out) = (block(c, BF), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let want = if id == 0 { String::new() } else { String::from_utf16_lossy(&mem::wstr(id)) };
    let pins = vmr_with(b, |v| v.pins.clone()).unwrap_or_default();
    match pins.iter().enumerate().find(|(i, _)| pin_name(*i as u32) == want) {
        Some((_, &p)) => {
            add_ref(c, b);
            mem::putp(out, p);
            0
        }
        None => {
            mem::putp(out, 0);
            VFW_E_NOT_FOUND
        }
    }
}
/// QueryFilterInfo(&info): FILTER_INFO { WCHAR achName[128], IFilterGraph *pGraph }
fn bf_query_info(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, BF), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let (name, graph) = vmr_with(b, |v| (v.name.clone(), v.graph)).unwrap_or_default();
    mem::fill(out, 256, 0);
    for (i, ch) in name.iter().take(127).enumerate() {
        mem::w16(out + 2 * i as u32, *ch);
    }
    guest_add_ref(c, graph);
    wp(out + 256, graph);
    0
}
/// JoinFilterGraph(graph, name): the graph is not held (it holds the filter)
fn bf_join(c: &mut Ctx) -> u32 {
    let (b, graph, name) = (block(c, BF), c.a(1), c.a(2));
    let name = if name == 0 { Vec::new() } else { mem::wstr(name) };
    vmr_with(b, |v| {
        v.graph = graph;
        v.name = name;
    });
    0
}

// IEnumPins: { vtable, filter block, position, version } (its own references)
static ENUMS: Mutex<Option<HashMap<u32, (u32, u32, u32, u32)>>> = Mutex::new(None);
fn enums<R>(f: impl FnOnce(&mut HashMap<u32, (u32, u32, u32, u32)>) -> R) -> R {
    f(ENUMS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}
fn enum_pins_new(c: &mut Ctx, b: u32, pos: u32) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let v = *V.get_or_init(|| {
        fn e_qi(c: &mut Ctx) -> u32 {
            let (e, iid, out) = (c.a(0), c.a(1), c.a(2));
            if out == 0 {
                return E_POINTER;
            }
            let g = read_guid(iid);
            if g == IID_IUNKNOWN || g == IID_IENUMPINS {
                enums(|m| m.get_mut(&e).map(|x| x.0 += 1));
                mem::putp(out, e);
                0
            } else {
                mem::putp(out, 0);
                E_NOINTERFACE
            }
        }
        fn e_add(c: &mut Ctx) -> u32 {
            let e = c.a(0);
            enums(|m| m.get_mut(&e).map_or(0, |x| {
                x.0 += 1;
                x.0
            }))
        }
        fn e_release(c: &mut Ctx) -> u32 {
            let e = c.a(0);
            let (n, b) = enums(|m| match m.get_mut(&e) {
                Some(x) => {
                    x.0 = x.0.saturating_sub(1);
                    let r = (x.0, x.1);
                    if x.0 == 0 {
                        m.remove(&e);
                    }
                    r
                }
                None => (0, 0),
            });
            if n == 0 && b != 0 {
                release(c, b);
            }
            n
        }
        /// Next(n, pins, &fetched)
        fn e_next(c: &mut Ctx) -> u32 {
            let (e, n, out, fetched) = (c.a(0), c.a(1), c.a(2), c.a(3));
            if out == 0 {
                return E_POINTER;
            }
            if n > 1 && fetched == 0 {
                return E_INVALIDARG;
            }
            let Some((_, b, pos, ver)) = enums(|m| m.get(&e).copied()) else { return E_POINTER };
            let (pins, now) = vmr_with(b, |v| (v.pins.clone(), v.version)).unwrap_or_default();
            let mut got = 0;
            if ver == now {
                while got < n && ((pos + got) as usize) < pins.len() {
                    let p = pins[(pos + got) as usize];
                    add_ref(c, b);
                    mem::putp(out + ps() * got, p);
                    got += 1;
                }
                enums(|m| m.get_mut(&e).map(|x| x.2 += got));
            }
            if fetched != 0 {
                put32(fetched, got);
            }
            if got == n { 0 } else { S_FALSE }
        }
        fn e_skip(c: &mut Ctx) -> u32 {
            let (e, n) = (c.a(0), c.a(1));
            let Some((_, b, pos, ver)) = enums(|m| m.get(&e).copied()) else { return E_POINTER };
            let (count, now) = vmr_with(b, |v| (v.pins.len() as u32, v.version)).unwrap_or_default();
            if ver != now || pos + n > count {
                return S_FALSE;
            }
            enums(|m| m.get_mut(&e).map(|x| x.2 += n));
            0
        }
        fn e_reset(c: &mut Ctx) -> u32 {
            let e = c.a(0);
            let b = enums(|m| m.get(&e).map_or(0, |x| x.1));
            let now = vmr_with(b, |v| v.version).unwrap_or(0);
            enums(|m| {
                if let Some(x) = m.get_mut(&e) {
                    x.2 = 0;
                    x.3 = now;
                }
            });
            0
        }
        fn e_clone(c: &mut Ctx) -> u32 {
            let (e, out) = (c.a(0), c.a(1));
            if out == 0 {
                return E_POINTER;
            }
            let Some((_, b, pos, ver)) = enums(|m| m.get(&e).copied()) else { return E_POINTER };
            let n = enum_pins_new(c, b, pos);
            enums(|m| m.get_mut(&n).map(|x| x.3 = ver));
            mem::putp(out, n);
            0
        }
        vtable("VMR9::IEnumPins", &[(e_qi, 12), (e_add, 4), (e_release, 4), (e_next, 16), (e_skip, 8), (e_reset, 4), (e_clone, 8)])
    });
    let e = mem::heap_alloc(ps(), true);
    wp(e, v);
    let ver = vmr_with(b, |v| v.version).unwrap_or(0);
    add_ref(c, b);
    enums(|m| m.insert(e, (1, b, pos, ver)));
    e
}

// IVMRFilterConfig9
fn fc_set_streams(c: &mut Ctx) -> u32 {
    let (b, n) = (block(c, FC), c.a(1));
    if n == 0 || n > 16 {
        return E_INVALIDARG;
    }
    let busy = vmr_with(b, |v| v.streams != 0).unwrap_or(true);
    if busy {
        return VFW_E_WRONG_STATE;
    }
    enter_mixer(b, n);
    0
}
/// IVMRWindowlessControl9::SetVideoClippingWindow(hwnd): a windowless VMR not in mixer mode
/// takes it up with four streams then
fn vmr_set_clipping_window(c: &mut Ctx) -> u32 {
    let r = crate::dshow::wl_set_window(c);
    let b = block(c, WL);
    if r == 0 && vmr_with(b, |v| v.streams == 0).unwrap_or(false) {
        enter_mixer(b, 4);
    }
    r
}
fn enter_mixer(b: u32, n: u32) {
    let have = vmr_with(b, |v| v.pins.len() as u32).unwrap_or(0);
    let new: Vec<u32> = (have..n).map(|i| pin_new(b, i)).collect();
    vmr_with(b, |v| {
        v.streams = n;
        v.pins.extend(new);
        v.version += 1;
    });
}
fn fc_get_streams(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, FC), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    match vmr_with(b, |v| v.streams).unwrap_or(0) {
        0 => VFW_E_VMR_NOT_IN_MIXER_MODE,
        n => {
            put32(out, n);
            0
        }
    }
}
fn fc_set_prefs(c: &mut Ctx) -> u32 {
    let (b, p) = (block(c, FC), c.a(1));
    vmr_with(b, |v| v.prefs = p);
    0
}
fn fc_get_prefs(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, FC), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    put32(out, vmr_with(b, |v| v.prefs).unwrap_or(0));
    0
}
/// SetRenderingMode(mode): once (again: VFW_E_WRONG_STATE), windowed, windowless or renderless
fn fc_set_mode(c: &mut Ctx) -> u32 {
    let (b, m) = (block(c, FC), c.a(1));
    if !matches!(m, WINDOWED | WINDOWLESS | RENDERLESS) {
        return E_INVALIDARG;
    }
    let connected = vmr_with(b, |v| v.pins.clone()).unwrap_or_default().iter().any(|&p| pin_with(p, |p| p.peer != 0).unwrap_or(false));
    let set = vmr_with(b, |v| v.mode_set).unwrap_or(true);
    if set || connected {
        return VFW_E_WRONG_STATE;
    }
    vmr_with(b, |v| {
        v.mode = m;
        v.mode_set = true;
    });
    crate::dshow::vmr_evr(b + ps() * BF, Some(m == WINDOWLESS));
    0
}
fn fc_get_mode(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, FC), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    put32(out, vmr_with(b, |v| v.mode).unwrap_or(WINDOWED));
    0
}

// IAMFilterMiscFlags::GetMiscFlags: AM_FILTER_MISC_FLAGS_IS_RENDERER
fn misc_flags(_: &mut Ctx) -> u32 {
    1
}

// IVMRAspectRatioControl9
fn arc_get(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, ARC), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    put32(out, vmr_with(b, |v| v.aspect).unwrap_or(0));
    0
}
fn arc_set(c: &mut Ctx) -> u32 {
    let (b, m) = (block(c, ARC), c.a(1));
    if m > 1 {
        return E_INVALIDARG;
    }
    vmr_with(b, |v| v.aspect = m);
    crate::dshow::vmr_evr(b + ps() * BF, None);
    0
}

// IVMRMixerBitmap9: VMR9AlphaBitmap kept as it was given
fn mb_set(c: &mut Ctx) -> u32 {
    let (b, p) = (block(c, MB), c.a(1));
    if p == 0 {
        return E_POINTER;
    }
    let size = if mem::x64() { 64 } else { 56 };
    let v: Vec<u8> = (0..size).map(|i| mem::r8(p + i)).collect();
    vmr_with(b, |x| x.alpha_bitmap = v);
    0
}
fn mb_get(c: &mut Ctx) -> u32 {
    let (b, p) = (block(c, MB), c.a(1));
    if p == 0 {
        return E_POINTER;
    }
    let v = vmr_with(b, |x| x.alpha_bitmap.clone()).unwrap_or_default();
    let size = if mem::x64() { 64 } else { 56 };
    mem::fill(p, size, 0);
    mem::write(p, &v);
    0
}

// IVMRMixerControl9 (mixer mode): the background colour and mixing preferences kept
fn mix_set_bg(c: &mut Ctx) -> u32 {
    let (b, v) = (block(c, MIX), c.a(1));
    vmr_with(b, |x| x.background = v);
    0
}
fn mix_get_bg(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, MIX), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    put32(out, vmr_with(b, |x| x.background).unwrap_or(0));
    0
}
fn mix_set_prefs(c: &mut Ctx) -> u32 {
    let (b, v) = (block(c, MIX), c.a(1));
    vmr_with(b, |x| x.mixing_prefs = v);
    0
}
fn mix_get_prefs(c: &mut Ctx) -> u32 {
    let (b, out) = (block(c, MIX), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    put32(out, vmr_with(b, |x| x.mixing_prefs).unwrap_or(0));
    0
}

// IVideoWindow / IBasicVideo on the filter: nothing to show until a pin is connected
fn not_connected(_: &mut Ctx) -> u32 {
    VFW_E_NOT_CONNECTED
}
// IKsPropertySet
fn ks_unsupported(_: &mut Ctx) -> u32 {
    E_PROP_ID_UNSUPPORTED
}
// IQualProp getters: 0
fn get0(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), 0);
    0
}

// ---------------------------------------------------------------------------------------------
// the input pins: { IPin, IMemInputPin, IOverlay, IAMVideoAccelerator, IQualityControl }

const P_PIN: u32 = 0;
const P_MIP: u32 = 1;
const P_OV: u32 = 2;
const P_VA: u32 = 3;
const P_QC: u32 = 4;
const P_N: u32 = 5;

fn pin_block(c: &Ctx, k: u32) -> u32 {
    c.a(0) - ps() * k
}
fn pin_filter(p: u32) -> u32 {
    pin_with(p, |p| p.filter).unwrap_or(0)
}
fn pin_qi(c: &mut Ctx, p: u32) -> u32 {
    let (iid, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let k = match read_guid(iid) {
        IID_IUNKNOWN | IID_IPIN => P_PIN,
        IID_IMEMINPUTPIN => P_MIP,
        IID_IOVERLAY => P_OV,
        IID_IAMVIDEOACCELERATOR => P_VA,
        IID_IQUALITYCONTROL => P_QC,
        _ => {
            mem::putp(out, 0);
            return E_NOINTERFACE;
        }
    };
    add_ref(c, pin_filter(p));
    mem::putp(out, p + ps() * k);
    0
}
macro_rules! pin_iunknown {
    ($k:expr) => {{
        fn q(c: &mut Ctx) -> u32 {
            pin_qi(c, pin_block(c, $k))
        }
        fn a(c: &mut Ctx) -> u32 {
            add_ref(c, pin_filter(pin_block(c, $k)))
        }
        fn r(c: &mut Ctx) -> u32 {
            release(c, pin_filter(pin_block(c, $k)))
        }
        vec![(q as Handler, 12), (a as Handler, 4), (r as Handler, 4)]
    }};
}

fn pin_new(b: u32, index: u32) -> u32 {
    static V: std::sync::OnceLock<[u32; P_N as usize]> = std::sync::OnceLock::new();
    let v = V.get_or_init(|| {
        let mut pin = pin_iunknown!(P_PIN);
        pin.extend([
            (pin_connect as Handler, 12),
            (pin_receive_connection, 12),
            (pin_disconnect, 4),
            (pin_connected_to, 8),
            (pin_connection_mt, 8),
            (pin_query_info, 8),
            (pin_query_direction, 8),
            (pin_query_id, 8),
            (pin_query_accept, 8),
            (pin_enum_mt, 8),
            (notimpl, 12), // QueryInternalConnections
            (pin_eos, 4),
            (pin_begin_flush, 4),
            (pin_end_flush, 4),
            (ok, 28), // NewSegment(start, stop, rate)
        ]);
        let mut mip = pin_iunknown!(P_MIP);
        mip.extend([(mip_get_allocator as Handler, 8), (mip_notify_allocator, 12), (notimpl, 8), (mip_receive, 8), (mip_receive_multiple, 16), (ok, 4)]);
        let mut ov = pin_iunknown!(P_OV);
        ov.extend([(notimpl as Handler, 12), (notimpl, 16), (notimpl, 8), (notimpl, 8), (notimpl, 16), (notimpl, 8), (notimpl, 8), (notimpl, 12), (notimpl, 12), (notimpl, 8), (notimpl, 8), (notimpl, 12), (notimpl, 8)]);
        let mut va = pin_iunknown!(P_VA);
        va.extend([(notimpl as Handler, 12), (notimpl, 16), (notimpl, 12), (notimpl, 16), (notimpl, 12), (notimpl, 8), (notimpl, 8), (notimpl, 20), (notimpl, 12), (notimpl, 28), (notimpl, 16), (notimpl, 12)]);
        let mut qc = pin_iunknown!(P_QC);
        qc.extend([(ok as Handler, 32), (ok, 8)]);
        [vtable("VMR9::IPin", &pin), vtable("VMR9::IMemInputPin", &mip), vtable("VMR9::IOverlay", &ov), vtable("VMR9::IAMVideoAccelerator", &va), vtable("VMR9::IQualityControl(pin)", &qc)]
    });
    let p = mem::heap_alloc(ps() * P_N, true);
    for (k, t) in v.iter().enumerate() {
        wp(p + ps() * k as u32, *t);
    }
    PINS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(p, Pin { filter: b, index, peer: 0, mt: None, alloc: 0, flushing: false });
    p
}

/// (an input pin is connected to: Connect is the output pin's)
fn pin_connect(_: &mut Ctx) -> u32 {
    E_UNEXPECTED
}
/// ReceiveConnection(connector, mt)
fn pin_receive_connection(c: &mut Ctx) -> u32 {
    let (p, peer, mtp) = (pin_block(c, P_PIN), c.a(1), c.a(2));
    if peer == 0 || mtp == 0 {
        return E_POINTER;
    }
    let b = pin_filter(p);
    if vmr_with(b, |v| v.state).unwrap_or(0) != 0 {
        return VFW_E_NOT_STOPPED;
    }
    if pin_with(p, |p| p.peer != 0).unwrap_or(true) {
        return VFW_E_ALREADY_CONNECTED;
    }
    let mt = MediaType::read(mtp);
    if !accepts(&mt) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    guest_add_ref(c, peer);
    pin_with(p, |p| {
        p.peer = peer;
        p.mt = Some(mt);
    });
    0
}
fn pin_disconnect(c: &mut Ctx) -> u32 {
    let p = pin_block(c, P_PIN);
    let peer = pin_with(p, |p| {
        p.mt = None;
        std::mem::take(&mut p.peer)
    })
    .unwrap_or(0);
    if peer == 0 {
        return S_FALSE;
    }
    guest_release(c, peer);
    0
}
fn pin_connected_to(c: &mut Ctx) -> u32 {
    let (p, out) = (pin_block(c, P_PIN), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let peer = pin_with(p, |p| p.peer).unwrap_or(0);
    mem::putp(out, peer);
    if peer == 0 {
        return VFW_E_NOT_CONNECTED;
    }
    guest_add_ref(c, peer);
    0
}
fn pin_connection_mt(c: &mut Ctx) -> u32 {
    let (p, out) = (pin_block(c, P_PIN), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    match pin_with(p, |p| p.mt.clone()).flatten() {
        Some(mt) => {
            mt.write(out);
            0
        }
        None => {
            mem::fill(out, mt_size(), 0);
            VFW_E_NOT_CONNECTED
        }
    }
}
/// QueryPinInfo(&info): PIN_INFO { IBaseFilter *pFilter, PIN_DIRECTION dir, WCHAR achName[128] }
fn pin_query_info(c: &mut Ctx) -> u32 {
    let (p, out) = (pin_block(c, P_PIN), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let (b, i) = pin_with(p, |p| (p.filter, p.index)).unwrap_or((0, 0));
    add_ref(c, b);
    wp(out, b + ps() * BF);
    w32(out + ps(), 0);
    let name: Vec<u16> = pin_name(i).encode_utf16().collect();
    mem::put_wstr(out + ps() + 4, 128, &name);
    0
}
fn pin_query_direction(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), 0);
    0
}
fn pin_query_id(c: &mut Ctx) -> u32 {
    let (p, out) = (pin_block(c, P_PIN), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let i = pin_with(p, |p| p.index).unwrap_or(0);
    let name: Vec<u16> = pin_name(i).encode_utf16().collect();
    let s = mem::heap_alloc(2 * name.len() as u32 + 2, false);
    mem::put_wstr(s, name.len() as u32 + 1, &name);
    mem::putp(out, s);
    0
}
fn pin_query_accept(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    if accepts(&MediaType::read(c.a(1))) { 0 } else { S_FALSE }
}
/// EnumMediaTypes: none (it takes what it is offered)
fn pin_enum_mt(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::putp(c.a(1), empty_enum_mt());
    0
}
fn pin_eos(c: &mut Ctx) -> u32 {
    let p = pin_block(c, P_PIN);
    let b = pin_filter(p);
    if pin_with(p, |p| p.flushing).unwrap_or(false) {
        return S_FALSE;
    }
    let graph = vmr_with(b, |v| v.graph).unwrap_or(0);
    let _ = graph;
    0
}
fn pin_begin_flush(c: &mut Ctx) -> u32 {
    pin_with(pin_block(c, P_PIN), |p| p.flushing = true);
    0
}
fn pin_end_flush(c: &mut Ctx) -> u32 {
    pin_with(pin_block(c, P_PIN), |p| p.flushing = false);
    0
}

/// an IEnumMediaTypes with nothing in it (its references are not counted)
fn empty_enum_mt() -> u32 {
    static E: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *E.get_or_init(|| {
        fn q(c: &mut Ctx) -> u32 {
            let (iid, out) = (c.a(1), c.a(2));
            if out == 0 {
                return E_POINTER;
            }
            let g = read_guid(iid);
            if g == IID_IUNKNOWN || g == IID_IENUMMEDIATYPES {
                mem::putp(out, c.a(0));
                0
            } else {
                mem::putp(out, 0);
                E_NOINTERFACE
            }
        }
        fn one(_: &mut Ctx) -> u32 {
            1
        }
        fn next(c: &mut Ctx) -> u32 {
            if c.a(2) == 0 {
                return E_POINTER;
            }
            if c.a(3) != 0 {
                put32(c.a(3), 0);
            }
            if c.a(1) == 0 { 0 } else { S_FALSE }
        }
        fn skip(c: &mut Ctx) -> u32 {
            if c.a(1) == 0 { 0 } else { S_FALSE }
        }
        fn clone(c: &mut Ctx) -> u32 {
            if c.a(1) == 0 {
                return E_POINTER;
            }
            mem::putp(c.a(1), c.a(0));
            0
        }
        let v = vtable("VMR9::IEnumMediaTypes", &[(q, 12), (one, 4), (one, 4), (next, 16), (skip, 8), (ok, 4), (clone, 8)]);
        let e = mem::heap_alloc(ps(), true);
        wp(e, v);
        e
    })
}

// IMemInputPin
fn mip_get_allocator(c: &mut Ctx) -> u32 {
    let (p, out) = (pin_block(c, P_MIP), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let a = match pin_with(p, |p| p.alloc).unwrap_or(0) {
        0 => {
            let a = alloc_new();
            pin_with(p, |p| p.alloc = a);
            a
        }
        a => a,
    };
    alloc_with(a, |x| x.refs += 1);
    mem::putp(out, a);
    0
}
/// NotifyAllocator(allocator, read only): its own allocator only
fn mip_notify_allocator(c: &mut Ctx) -> u32 {
    let (p, a) = (pin_block(c, P_MIP), c.a(1));
    let mine = pin_with(p, |p| p.alloc).unwrap_or(0);
    if a != 0 && a == mine { 0 } else { E_FAIL }
}
/// Receive(sample): not while stopped; the frame is the picture now
fn mip_receive(c: &mut Ctx) -> u32 {
    let p = pin_block(c, P_MIP);
    let b = pin_filter(p);
    let s = c.a(1);
    if s == 0 {
        return E_POINTER;
    }
    let (state, flushing) = (vmr_with(b, |v| v.state).unwrap_or(0), pin_with(p, |p| p.flushing).unwrap_or(false));
    if flushing {
        return S_FALSE;
    }
    if state == 0 {
        return VFW_E_WRONG_STATE;
    }
    vmr_with(b, |v| v.sample = true);
    0
}
fn mip_receive_multiple(c: &mut Ctx) -> u32 {
    let (samples, n, done) = (c.a(1), c.a(2), c.a(3));
    let mut k = 0;
    let mut r = 0;
    while k < n {
        let s = rp(samples + ps() * k);
        // (each one as IMemInputPin::Receive takes it: through the vtable)
        let this = c.a(0);
        r = c.call(rp(rp(this) + ps() * 6), &[this, s]);
        if r != 0 {
            break;
        }
        k += 1;
    }
    if done != 0 {
        put32(done, k);
    }
    r
}

// ---------------------------------------------------------------------------------------------
// IVMRSurfaceAllocatorNotify9: the surfaces of a renderless VMR, made on its presenter's device

/// SetD3DDevice / ChangeD3DDevice(device, monitor)
fn san_set_device(c: &mut Ctx) -> u32 {
    let (b, dev) = (block(c, SAN), c.a(1));
    if dev == 0 {
        return E_POINTER;
    }
    guest_add_ref(c, dev);
    let old = vmr_with(b, |v| std::mem::replace(&mut v.device, dev)).unwrap_or(0);
    if old != 0 {
        com_call(c, old, 2, &[]);
    }
    0
}

/// VMR9AllocFlag_*
const ALLOC_3D_RENDER_TARGET: u32 = 1;
const ALLOC_TEXTURE: u32 = 4;
const D3DUSAGE_DYNAMIC: u32 = 0x200;
const D3DFMT_X8R8G8B8: u32 = 22;

/// AllocateSurfaceHelper(info, &count, surfaces): `count` surfaces as `info` asks for (fewer
/// than its MinBuffers: none), each a plain offscreen surface, a texture's top level or a
/// render target; on a failure none, *count 0
fn san_allocate(c: &mut Ctx) -> u32 {
    let (b, info, count, out) = (block(c, SAN), c.a(1), c.a(2), c.a(3));
    let dev = vmr_with(b, |v| v.device).unwrap_or(0);
    if dev == 0 {
        return E_FAIL;
    }
    if info == 0 || count == 0 || out == 0 {
        return E_POINTER;
    }
    let [flags, w, h, format, pool, min] = std::array::from_fn(|i| r32(info + 4 * i as u32));
    let n = r32(count);
    if n < min {
        put32(count, 0);
        return 0;
    }
    let tmp = mem::heap_alloc(2 * ps(), true);
    let mut made = Vec::new();
    let mut hr = 0;
    for _ in 0..n {
        mem::putp(tmp, 0);
        hr = if flags & ALLOC_TEXTURE != 0 {
            // CreateTexture(w, h, 1 level, D3DUSAGE_DYNAMIC, format, pool, &texture, NULL)
            let r = com_call(c, dev, 23, &[w, h, 1, D3DUSAGE_DYNAMIC, format, pool, tmp, 0]);
            let tex = rp(tmp);
            if r == 0 && tex != 0 {
                // GetSurfaceLevel(0, &surface); the surface holds its texture
                let r = com_call(c, tex, 18, &[0, tmp + ps()]);
                com_call(c, tex, 2, &[]);
                mem::putp(tmp, if r == 0 { rp(tmp + ps()) } else { 0 });
                r
            } else {
                r
            }
        } else if flags & ALLOC_3D_RENDER_TARGET != 0 {
            // CreateRenderTarget(w, h, format (none: the display's), no multisampling, 0,
            // not lockable, &surface, NULL)
            let f = if format == 0 { D3DFMT_X8R8G8B8 } else { format };
            com_call(c, dev, 28, &[w, h, f, 0, 0, 0, tmp, 0])
        } else {
            // CreateOffscreenPlainSurface(w, h, format, pool, &surface, NULL)
            com_call(c, dev, 36, &[w, h, format, pool, tmp, 0])
        };
        if hr != 0 || rp(tmp) == 0 {
            if hr == 0 {
                hr = E_FAIL;
            }
            break;
        }
        made.push(rp(tmp));
    }
    if hr != 0 {
        for s in made {
            com_call(c, s, 2, &[]);
        }
        put32(count, 0);
        return hr;
    }
    for (i, s) in made.into_iter().enumerate() {
        mem::putp(out + ps() * i as u32, s);
    }
    0
}

// ---------------------------------------------------------------------------------------------
// the memory allocator and its samples (IMemAllocator, IMediaSample2)

struct Alloc {
    refs: u32,
    props: [u32; 4],
    committed: bool,
    free: Vec<u32>,
    out: u32,
}
struct Sample {
    alloc: u32,
    refs: u32,
    data: u32,
    size: u32,
    len: u32,
    time: Option<(u64, u64)>,
    media_time: Option<(u64, u64)>,
    sync: bool,
    preroll: bool,
    discontinuity: bool,
    mt: Option<MediaType>,
}
static ALLOCS: Mutex<Option<HashMap<u32, Alloc>>> = Mutex::new(None);
static SAMPLES: Mutex<Option<HashMap<u32, Sample>>> = Mutex::new(None);
fn alloc_with<R>(a: u32, f: impl FnOnce(&mut Alloc) -> R) -> Option<R> {
    ALLOCS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&a).map(f)
}
fn sample_with<R>(s: u32, f: impl FnOnce(&mut Sample) -> R) -> Option<R> {
    SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&s).map(f)
}
fn alloc_release(a: u32) {
    alloc_with(a, |x| x.refs = x.refs.saturating_sub(1));
}

fn alloc_new() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let v = *V.get_or_init(|| {
        fn q(c: &mut Ctx) -> u32 {
            let (a, iid, out) = (c.a(0), c.a(1), c.a(2));
            if out == 0 {
                return E_POINTER;
            }
            let g = read_guid(iid);
            if g == IID_IUNKNOWN || g == IID_IMEMALLOCATOR {
                alloc_with(a, |x| x.refs += 1);
                mem::putp(out, a);
                0
            } else {
                mem::putp(out, 0);
                E_NOINTERFACE
            }
        }
        fn add(c: &mut Ctx) -> u32 {
            alloc_with(c.a(0), |x| {
                x.refs += 1;
                x.refs
            })
            .unwrap_or(1)
        }
        fn rel(c: &mut Ctx) -> u32 {
            alloc_with(c.a(0), |x| {
                x.refs = x.refs.saturating_sub(1);
                x.refs
            })
            .unwrap_or(0)
        }
        /// SetProperties(&request, &actual)
        fn set_props(c: &mut Ctx) -> u32 {
            let (a, req, act) = (c.a(0), c.a(1), c.a(2));
            if req == 0 || act == 0 {
                return E_POINTER;
            }
            let p = [r32(req), r32(req + 4), r32(req + 8), r32(req + 12)];
            let r = alloc_with(a, |x| {
                if x.committed {
                    return VFW_E_ALREADY_COMMITTED;
                }
                if x.out != 0 {
                    return VFW_E_BUFFERS_OUTSTANDING;
                }
                if p[2] == 0 {
                    return VFW_E_BADALIGN;
                }
                x.props = p;
                0
            })
            .unwrap_or(E_POINTER);
            if r == 0 {
                for (i, v) in p.iter().enumerate() {
                    w32(act + 4 * i as u32, *v);
                }
            }
            r
        }
        fn get_props(c: &mut Ctx) -> u32 {
            let (a, out) = (c.a(0), c.a(1));
            if out == 0 {
                return E_POINTER;
            }
            let p = alloc_with(a, |x| x.props).unwrap_or_default();
            for (i, v) in p.iter().enumerate() {
                w32(out + 4 * i as u32, *v);
            }
            0
        }
        fn commit(c: &mut Ctx) -> u32 {
            let a = c.a(0);
            let Some((props, committed, have)) = alloc_with(a, |x| (x.props, x.committed, x.free.len() as u32 + x.out)) else { return E_POINTER };
            if committed {
                return 0;
            }
            if props[0] == 0 || props[1] == 0 {
                return VFW_E_SIZENOTSET;
            }
            let new: Vec<u32> = (have..props[0]).map(|_| sample_new(a, props[1] + props[3])).collect();
            alloc_with(a, |x| {
                x.committed = true;
                x.free.extend(new);
            });
            0
        }
        fn decommit(c: &mut Ctx) -> u32 {
            alloc_with(c.a(0), |x| x.committed = false);
            0
        }
        /// GetBuffer(&sample, start, end, flags)
        fn get_buffer(c: &mut Ctx) -> u32 {
            let (a, out) = (c.a(0), c.a(1));
            if out == 0 {
                return E_POINTER;
            }
            mem::putp(out, 0);
            let r = alloc_with(a, |x| {
                if !x.committed {
                    return Err(VFW_E_NOT_COMMITTED);
                }
                match x.free.pop() {
                    Some(s) => {
                        x.out += 1;
                        Ok(s)
                    }
                    None => Err(VFW_E_TIMEOUT),
                }
            })
            .unwrap_or(Err(E_POINTER));
            match r {
                Ok(s) => {
                    sample_with(s, |x| {
                        x.refs = 1;
                        x.len = 0;
                        x.time = None;
                        x.media_time = None;
                        x.sync = false;
                        x.preroll = false;
                        x.discontinuity = false;
                        x.mt = None;
                    });
                    mem::putp(out, s);
                    0
                }
                Err(e) => e,
            }
        }
        vtable("VMR9::IMemAllocator", &[(q, 12), (add, 4), (rel, 4), (set_props, 12), (get_props, 8), (commit, 4), (decommit, 4), (get_buffer, 20), (ok, 8)])
    });
    let a = mem::heap_alloc(ps(), true);
    wp(a, v);
    ALLOCS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(a, Alloc { refs: 1, props: [0; 4], committed: false, free: Vec::new(), out: 0 });
    a
}

fn sample_new(alloc: u32, size: u32) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let v = *V.get_or_init(|| {
        fn q(c: &mut Ctx) -> u32 {
            let (s, iid, out) = (c.a(0), c.a(1), c.a(2));
            if out == 0 {
                return E_POINTER;
            }
            let g = read_guid(iid);
            if g == IID_IUNKNOWN || g == IID_IMEDIASAMPLE || g == IID_IMEDIASAMPLE2 {
                sample_with(s, |x| x.refs += 1);
                mem::putp(out, s);
                0
            } else {
                mem::putp(out, 0);
                E_NOINTERFACE
            }
        }
        fn add(c: &mut Ctx) -> u32 {
            sample_with(c.a(0), |x| {
                x.refs += 1;
                x.refs
            })
            .unwrap_or(1)
        }
        /// the last reference goes: back to its allocator
        fn rel(c: &mut Ctx) -> u32 {
            let s = c.a(0);
            let (n, a) = sample_with(s, |x| {
                x.refs = x.refs.saturating_sub(1);
                (x.refs, x.alloc)
            })
            .unwrap_or((0, 0));
            if n == 0 {
                alloc_with(a, |x| {
                    x.out = x.out.saturating_sub(1);
                    if !x.free.contains(&s) {
                        x.free.push(s);
                    }
                });
            }
            n
        }
        fn get_pointer(c: &mut Ctx) -> u32 {
            if c.a(1) == 0 {
                return E_POINTER;
            }
            mem::putp(c.a(1), sample_with(c.a(0), |x| x.data).unwrap_or(0));
            0
        }
        fn get_size(c: &mut Ctx) -> u32 {
            sample_with(c.a(0), |x| x.size).unwrap_or(0)
        }
        /// GetTime(&start, &end): VFW_E_SAMPLE_TIME_NOT_SET without one
        fn get_time(c: &mut Ctx) -> u32 {
            let (s, a, b) = (c.a(0), c.a(1), c.a(2));
            match sample_with(s, |x| x.time).flatten() {
                Some((t0, t1)) => {
                    if a != 0 {
                        mem::put64(a, t0);
                    }
                    if b != 0 {
                        mem::put64(b, t1);
                    }
                    0
                }
                None => 0x8004_0249,
            }
        }
        fn set_time(c: &mut Ctx) -> u32 {
            let (s, a, b) = (c.a(0), c.a(1), c.a(2));
            let t = (a != 0).then(|| (mem::r64(a), if b != 0 { mem::r64(b) } else { mem::r64(a) }));
            sample_with(s, |x| x.time = t);
            0
        }
        fn is_sync(c: &mut Ctx) -> u32 {
            if sample_with(c.a(0), |x| x.sync).unwrap_or(false) { 0 } else { S_FALSE }
        }
        fn set_sync(c: &mut Ctx) -> u32 {
            let v = c.a(1) != 0;
            sample_with(c.a(0), |x| x.sync = v);
            0
        }
        fn is_preroll(c: &mut Ctx) -> u32 {
            if sample_with(c.a(0), |x| x.preroll).unwrap_or(false) { 0 } else { S_FALSE }
        }
        fn set_preroll(c: &mut Ctx) -> u32 {
            let v = c.a(1) != 0;
            sample_with(c.a(0), |x| x.preroll = v);
            0
        }
        fn get_len(c: &mut Ctx) -> u32 {
            sample_with(c.a(0), |x| x.len).unwrap_or(0)
        }
        fn set_len(c: &mut Ctx) -> u32 {
            let n = c.a(1);
            sample_with(c.a(0), |x| {
                if n > x.size {
                    return 0x8007_0057;
                }
                x.len = n;
                0
            })
            .unwrap_or(E_POINTER)
        }
        fn get_mt(c: &mut Ctx) -> u32 {
            let out = c.a(1);
            if out == 0 {
                return E_POINTER;
            }
            match sample_with(c.a(0), |x| x.mt.clone()).flatten() {
                Some(mt) => {
                    let p = mem::heap_alloc(mt_size(), true);
                    mt.write(p);
                    mem::putp(out, p);
                    0
                }
                None => {
                    mem::putp(out, 0);
                    S_FALSE
                }
            }
        }
        fn set_mt(c: &mut Ctx) -> u32 {
            let p = c.a(1);
            let mt = (p != 0).then(|| MediaType::read(p));
            sample_with(c.a(0), |x| x.mt = mt);
            0
        }
        fn is_disc(c: &mut Ctx) -> u32 {
            if sample_with(c.a(0), |x| x.discontinuity).unwrap_or(false) { 0 } else { S_FALSE }
        }
        fn set_disc(c: &mut Ctx) -> u32 {
            let v = c.a(1) != 0;
            sample_with(c.a(0), |x| x.discontinuity = v);
            0
        }
        fn get_media_time(c: &mut Ctx) -> u32 {
            let (s, a, b) = (c.a(0), c.a(1), c.a(2));
            match sample_with(s, |x| x.media_time).flatten() {
                Some((t0, t1)) => {
                    if a != 0 {
                        mem::put64(a, t0);
                    }
                    if b != 0 {
                        mem::put64(b, t1);
                    }
                    0
                }
                None => 0x8004_0251,
            }
        }
        fn set_media_time(c: &mut Ctx) -> u32 {
            let (s, a, b) = (c.a(0), c.a(1), c.a(2));
            let t = (a != 0 && b != 0).then(|| (mem::r64(a), mem::r64(b)));
            sample_with(s, |x| x.media_time = t);
            0
        }
        vtable(
            "VMR9::IMediaSample",
            &[
                (q, 12),
                (add, 4),
                (rel, 4),
                (get_pointer, 8),
                (get_size, 4),
                (get_time, 12),
                (set_time, 12),
                (is_sync, 4),
                (set_sync, 8),
                (is_preroll, 4),
                (set_preroll, 8),
                (get_len, 4),
                (set_len, 8),
                (get_mt, 8),
                (set_mt, 8),
                (is_disc, 4),
                (set_disc, 8),
                (get_media_time, 12),
                (set_media_time, 12),
                (notimpl, 12), // GetProperties
                (notimpl, 12), // SetProperties
            ],
        )
    });
    let s = mem::heap_alloc(ps(), true);
    wp(s, v);
    let data = mem::heap_alloc(size.max(1), true);
    SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(
        s,
        Sample { alloc, refs: 0, data, size, len: 0, time: None, media_time: None, sync: false, preroll: false, discontinuity: false, mt: None },
    );
    s
}

// ---------------------------------------------------------------------------------------------

fn vtables() -> &'static [u32; N as usize] {
    static V: std::sync::OnceLock<[u32; N as usize]> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let unk: Vec<(Handler, u32)> = vec![(unk_qi, 12), (unk_add_ref, 4), (unk_release, 4)];
        let mut bf = iunknown!(BF);
        bf.extend([
            (get_class_id as Handler, 8),
            (bf_stop, 4),
            (bf_pause, 4),
            (bf_run, 12),
            (bf_get_state, 12),
            (bf_set_sync, 8),
            (bf_get_sync, 8),
            (bf_enum_pins, 8),
            (bf_find_pin, 12),
            (bf_query_info, 8),
            (bf_join, 12),
            (notimpl, 8), // QueryVendorInfo
        ]);
        let mut fc = iunknown!(FC);
        fc.extend([(ok as Handler, 8), (fc_set_streams, 8), (fc_get_streams, 8), (fc_set_prefs, 8), (fc_get_prefs, 8), (fc_set_mode, 8), (fc_get_mode, 8)]);
        let mut wl = iunknown!(WL);
        {
            use crate::dshow::*;
            wl.extend([
                (wl_native as Handler, 20),
                (wl_min_ideal, 12),
                (wl_max_ideal, 12),
                (wl_set_pos, 12),
                (wl_get_pos, 12),
                (wl_get_mode, 8),
                (wl_set_mode, 8),
                (vmr_set_clipping_window, 8),
                (ok, 12), // RepaintVideo
                (ok, 4),  // DisplayModeChanged
                (wl_image, 8),
                (wl_set_border, 8),
                (wl_get_border, 8),
            ]);
        }
        let mut cop = iunknown!(COP);
        cop.extend([(notimpl as Handler, 16), (notimpl, 12), (notimpl, 12), (notimpl, 12)]);
        let mut misc = iunknown!(MISC);
        misc.push((misc_flags, 4));
        // (IMediaPosition and IMediaSeeking: nothing upstream to pass them to)
        let mut mp = iunknown!(MP);
        mp.extend([(notimpl as Handler, 8), (notimpl, 16), (notimpl, 24), (notimpl, 36)]);
        mp.extend([(notimpl as Handler, 8), (notimpl, 12), (notimpl, 8), (notimpl, 8), (notimpl, 12), (notimpl, 8), (notimpl, 12), (notimpl, 12), (notimpl, 8), (notimpl, 8), (notimpl, 8)]);
        let mut msk = iunknown!(MSK);
        msk.extend([
            (notimpl as Handler, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 8),
            (notimpl, 24),
            (notimpl, 20),
            (notimpl, 12),
            (notimpl, 12),
            (notimpl, 12),
            (notimpl, 8),
            (notimpl, 8),
        ]);
        let mut qc = iunknown!(QC);
        qc.extend([(ok as Handler, 32), (ok, 8)]);
        let mut arc = iunknown!(ARC);
        arc.extend([(arc_get as Handler, 8), (arc_set, 8)]);
        let mut mb = iunknown!(MB);
        mb.extend([(mb_set as Handler, 8), (mb_set, 8), (mb_get, 8)]);
        let mut san = iunknown!(SAN);
        // AdviseSurfaceAllocator, SetD3DDevice, ChangeD3DDevice, AllocateSurfaceHelper, NotifyEvent
        san.extend([(ok as Handler, 12), (san_set_device, 12), (san_set_device, 12), (san_allocate, 16), (ok, 16)]);
        let mut mix = iunknown!(MIX);
        mix.extend([
            (ok as Handler, 12),
            (notimpl, 12),
            (ok, 12),
            (notimpl, 12),
            (ok, 12),
            (notimpl, 12),
            (mix_set_bg, 8),
            (mix_get_bg, 8),
            (mix_set_prefs, 8),
            (mix_get_prefs, 8),
            (notimpl, 12),
            (notimpl, 12),
            (notimpl, 12),
        ]);
        let mut mon = iunknown!(MON);
        mon.extend([(ok as Handler, 8), (get0, 8), (ok, 8), (get0, 8), (notimpl, 16)]);
        let mut vw = iunknown!(VW);
        vw.extend([(notimpl as Handler, 8), (notimpl, 16), (notimpl, 24), (notimpl, 36)]);
        vw.extend(std::iter::repeat((not_connected as Handler, 8)).take(30));
        vw.extend([(not_connected as Handler, 8), (not_connected, 20), (not_connected, 20), (not_connected, 20), (not_connected, 12), (not_connected, 12), (not_connected, 20), (not_connected, 8), (not_connected, 8)]);
        let mut bv = iunknown!(BV);
        bv.extend([(notimpl as Handler, 8), (notimpl, 16), (notimpl, 24), (notimpl, 36)]);
        bv.extend(std::iter::repeat((not_connected as Handler, 8)).take(21));
        bv.extend([(not_connected as Handler, 20), (not_connected, 20), (not_connected, 4), (not_connected, 20), (not_connected, 20), (not_connected, 4), (not_connected, 12), (not_connected, 20), (not_connected, 12), (not_connected, 4), (not_connected, 4), (not_connected, 12)]);
        let mut ks = iunknown!(KS);
        ks.extend([(ks_unsupported as Handler, 28), (ks_unsupported, 32), (ks_unsupported, 16)]);
        let mut qp = iunknown!(QP);
        qp.extend(std::iter::repeat((get0 as Handler, 8)).take(6));
        let mut di = iunknown!(DI);
        di.extend(std::iter::repeat((notimpl as Handler, 12)).take(8));
        let t = |n: &str, v: &[(Handler, u32)]| vtable(n, v);
        [
            t("VMR9::IUnknown", &unk),
            t("VMR9::IBaseFilter", &bf),
            t("VMR9::IVMRFilterConfig9", &fc),
            t("VMR9::IVMRWindowlessControl9", &wl),
            t("VMR9::IAMCertifiedOutputProtection", &cop),
            t("VMR9::IAMFilterMiscFlags", &misc),
            t("VMR9::IMediaPosition", &mp),
            t("VMR9::IMediaSeeking", &msk),
            t("VMR9::IQualityControl", &qc),
            t("VMR9::IVMRAspectRatioControl9", &arc),
            t("VMR9::IVMRMixerBitmap9", &mb),
            t("VMR9::IVMRSurfaceAllocatorNotify9", &san),
            t("VMR9::IVMRMixerControl9", &mix),
            t("VMR9::IVMRMonitorConfig9", &mon),
            t("VMR9::IVideoWindow", &vw),
            t("VMR9::IBasicVideo", &bv),
            t("VMR9::IKsPropertySet", &ks),
            t("VMR9::IQualProp", &qp),
            t("VMR9::IVMRDeinterlaceControl9", &di),
        ]
    })
}

/// CoCreateInstance(CLSID_VideoMixingRenderer9, outer, ..., iid, &out); None: not this class
pub fn create_instance(c: &mut Ctx, clsid: u32, outer: u32, iid: u32, out: u32) -> Option<u32> {
    if clsid != 0 && read_guid(clsid) == CLSID_MEMORY_ALLOCATOR {
        if out == 0 {
            return Some(E_POINTER);
        }
        let g = read_guid(iid);
        if outer != 0 || (g != IID_IUNKNOWN && g != IID_IMEMALLOCATOR) {
            mem::putp(out, 0);
            return Some(if outer != 0 { CLASS_E_NOAGGREGATION } else { E_NOINTERFACE });
        }
        mem::putp(out, alloc_new());
        return Some(0);
    }
    if clsid == 0 || read_guid(clsid) != CLSID_VMR9 {
        return None;
    }
    if out == 0 {
        return Some(E_POINTER);
    }
    // (aggregated: only its IUnknown can be asked for)
    if outer != 0 && read_guid(iid) != IID_IUNKNOWN {
        mem::putp(out, 0);
        return Some(E_NOINTERFACE);
    }
    let b = mem::heap_alloc(ps() * N, true);
    for (k, v) in vtables().iter().enumerate() {
        wp(b + ps() * k as u32, *v);
    }
    let pin = pin_new(b, 0);
    VMRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(
        b,
        Vmr {
            refs: 0,
            outer,
            mode: WINDOWED,
            mode_set: false,
            streams: 0,
            state: 0,
            sample: false,
            clock: 0,
            graph: 0,
            name: Vec::new(),
            pins: vec![pin],
            version: 0,
            prefs: 0,
            mixing_prefs: 0,
            aspect: 0,
            background: 0,
            alpha_bitmap: Vec::new(),
            device: 0,
        },
    );
    crate::dshow::vmr_evr(b + ps() * BF, Some(false));
    let r = qi_inner_out(c, b, iid, out);
    if r != 0 {
        vmr_with(b, |v| v.refs = 1);
        inner_release(c, b);
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mt(major: Guid, sub: Guid, fmt: Guid, w: i32, h: i32) -> MediaType {
        let mut head = vec![0u8; 72];
        head[0..16].copy_from_slice(&major);
        head[16..32].copy_from_slice(&sub);
        head[44..60].copy_from_slice(&fmt);
        let mut format = vec![0u8; 88];
        format[48..52].copy_from_slice(&40u32.to_le_bytes());
        format[52..56].copy_from_slice(&w.to_le_bytes());
        format[56..60].copy_from_slice(&h.to_le_bytes());
        MediaType { head, format }
    }
    fn rgb(d: u32) -> Guid {
        guid(d, 0x524f, 0x11ce, RGB)
    }

    #[test]
    fn media_types_taken() {
        for s in [0xe436_eb7b, 0xe436_eb7c, 0xe436_eb7d, 0xe436_eb7e] {
            assert!(accepts(&mt(MEDIATYPE_VIDEO, rgb(s), FORMAT_VIDEOINFO, 32, 16)));
            assert!(accepts(&mt(MEDIATYPE_VIDEO, rgb(s), FORMAT_VIDEOINFO2, 32, 16)));
        }
        assert!(accepts(&mt(MEDIATYPE_VIDEO, guid(u32::from_le_bytes(*b"YUY2"), 0, 0x10, MS), FORMAT_VIDEOINFO, 2, 2)));
        // RGB8, no subtype, no major type, no format: not
        assert!(!accepts(&mt(MEDIATYPE_VIDEO, rgb(0xe436_eb7a), FORMAT_VIDEOINFO, 32, 16)));
        assert!(!accepts(&mt(MEDIATYPE_VIDEO, [0; 16], FORMAT_VIDEOINFO, 32, 16)));
        assert!(!accepts(&mt([0; 16], rgb(0xe436_eb7e), FORMAT_VIDEOINFO, 32, 16)));
        assert!(!accepts(&mt(MEDIATYPE_VIDEO, rgb(0xe436_eb7e), [0; 16], 32, 16)));
    }

    #[test]
    fn picture_size() {
        assert_eq!(mt(MEDIATYPE_VIDEO, rgb(0xe436_eb7e), FORMAT_VIDEOINFO, 600, -400).size(), (600, 400));
    }

    #[test]
    fn guids_as_windows_lays_them_out() {
        // {51b4abf3-748f-4e3b-a276-c828330e926a}
        assert_eq!(&CLSID_VMR9[..8], &[0xf3, 0xab, 0xb4, 0x51, 0x8f, 0x74, 0x3b, 0x4e]);
        assert_eq!(&IID_IBASEFILTER[..4], &[0x95, 0x68, 0xa8, 0x56]);
    }

    proptest::proptest! {
        /// a type is taken exactly when it is video, of a format with a VIDEOINFO(2) header
        /// and of a subtype the mixer draws; its size whatever the height's sign
        #[test]
        fn takes_only_drawable_video(major in 0usize..3, sub in 0usize..14, fmt in 0usize..4, w in 1i32..4096, h in -4096i32..4096) {
            let majors = [MEDIATYPE_VIDEO, [0; 16], guid(0x7364_7561, 0, 0x10, MS)];
            let fourcc = |t: &[u8; 4]| guid(u32::from_le_bytes(*t), 0, 0x10, MS);
            let subs = [rgb(0xe436_eb7b), rgb(0xe436_eb7c), rgb(0xe436_eb7d), rgb(0xe436_eb7e), guid(0x773c_9ac0, 0x3274, 0x11d0, [0xb7, 0x24, 0x00, 0xaa, 0x00, 0x6c, 0x1a, 0x01]),
                fourcc(b"YUY2"), fourcc(b"UYVY"), fourcc(b"YV12"), fourcc(b"NV12"), fourcc(b"IYUV"),
                rgb(0xe436_eb7a), rgb(0xe436_eb79), fourcc(b"MJPG"), [0; 16]];
            let fmts = [FORMAT_VIDEOINFO, FORMAT_VIDEOINFO2, [0; 16], guid(0x05589f81, 0xc356, 0x11ce, [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a])];
            let m = mt(majors[major], subs[sub], fmts[fmt], w, h);
            proptest::prop_assert_eq!(accepts(&m), major == 0 && sub < 10 && fmt < 2);
            if fmt == 0 {
                proptest::prop_assert_eq!(m.size(), (w, h.abs()));
            }
        }
    }
}
