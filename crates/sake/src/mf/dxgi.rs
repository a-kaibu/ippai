//! IMFDXGIBuffer: media buffers over a Direct3D 11 texture (kura's), as MFCreateDXGISurfaceBuffer
//! makes them and as the Source Reader hands out frames when the program gave it a device
//! (MF_SOURCE_READER_D3D_MANAGER): IMFMediaBuffer and IMF2DBuffer on a copy of the texture's
//! bytes (Lock reads the texture back through a staging texture; the last Unlock writes changed
//! bytes back with UpdateSubresource, as Wine's dxgi buffer does), IMFDXGIBuffer for the
//! texture itself (GetResource, GetSubresourceIndex, Get/SetUnknown).
//!
//! The reader gives textures of NV12, RGB32 (B8G8R8X8) and ARGB32 (B8G8R8A8) frames (kura's
//! NV12: views of its planes as R8 and R8G8); the other YUV types stay memory buffers, as do
//! frames of odd sizes (NV12 textures are of even ones).
//!
//! The block: three interface slots (IMFMediaBuffer, IMFDXGIBuffer, IMF2DBuffer); the state in
//! DXS by the block's address.

use super::*;

/// ID3D11Texture2D {6f15aaf2-d208-4e89-9ab4-489535d34f9c}
const TEX2D: (u32, u16, u16) = (0x6f15_aaf2, 0xd208, 0x4e89);
const MF_E_NOT_FOUND: u32 = 0xc00d_36d5;
/// HRESULT_FROM_WIN32(ERROR_OBJECT_ALREADY_EXISTS)
const ALREADY: u32 = 0x8007_1392;
/// HRESULT_FROM_WIN32(ERROR_WAS_LOCKED), HRESULT_FROM_WIN32(ERROR_WAS_UNLOCKED)
const WAS_LOCKED: u32 = 0x8007_0717;
const WAS_UNLOCKED: u32 = 0x8007_0716;

// (vtable indexes: ID3D11Texture2D, ID3D11Device, ID3D11DeviceContext)
const TEX_GET_DEVICE: u32 = 3;
const TEX_GET_DESC: u32 = 10;
const DEV_CREATE_TEXTURE2D: u32 = 5;
const DEV_IMMEDIATE_CONTEXT: u32 = 40;
const CTX_MAP: u32 = 14;
const CTX_UNMAP: u32 = 15;
const CTX_COPY_REGION: u32 = 46;
const CTX_UPDATE: u32 = 48;

struct Dx {
    refs: u32,
    tex: u32,
    sub: u32,
    bottom_up: bool,
    /// the copy of the bytes (guest heap), rows of `pitch`
    data: u32,
    max: u32,
    len: u32,
    pitch: u32,
    rows: u32,
    w: u32,
    h: u32,
    fmt: u32,
    staging: u32,
    /// Lock / Lock2D depth; the bytes as the first one read them (to see what changed)
    locks: u32,
    read: Option<Vec<u8>>,
    unks: Vec<(Guid, u32)>,
}
static DXS: Mutex<Option<HashMap<u32, Dx>>> = Mutex::new(None);
fn dx<R>(b: u32, f: impl FnOnce(&mut Dx) -> R) -> Option<R> {
    DXS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
/// The block of an interface pointer of ours (any of its three slots).
fn base(this: u32) -> Option<u32> {
    let m = DXS.lock().unwrap_or_else(|e| e.into_inner());
    let m = m.as_ref()?;
    (0..3).map(|k| this.wrapping_sub(k * ps())).find(|b| m.contains_key(b))
}
fn with<R>(this: u32, f: impl FnOnce(&mut Dx) -> R) -> Option<R> {
    dx(base(this)?, f)
}

/// The bytes of a buffer of ours as last read (None: not one of ours), for samples that join
/// buffers (reader.rs).
pub fn cached(b: u32) -> Option<Vec<u8>> {
    with(b, |d| mem::slice(d.data, d.len).to_vec())
}
/// Its current length (None: not one of ours).
pub fn length(b: u32) -> Option<u32> {
    with(b, |d| d.len)
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let Some(b) = base(this) else { return E_INVALIDARG };
    let g = guid_at(c.a(1));
    // IMFMediaBuffer {045fa593-...}, IMFDXGIBuffer {e7174cfa-...}, IMF2DBuffer {7dc9d5f9-...}
    let slot = if is_unknown(&g) || is(&g, 0x045f_a593, 0x8799, 0x42b8) {
        0
    } else if is(&g, 0xe717_4cfa, 0x1c9e, 0x48b1) {
        1
    } else if is(&g, 0x7dc9_d5f9, 0x9ed9, 0x44ec) {
        2
    } else {
        putp(out, 0);
        return E_NOINTERFACE;
    };
    dx(b, |d| d.refs += 1);
    putp(out, b + slot * ps());
    0
}
fn add_ref_(c: &mut Ctx) -> u32 {
    with(c.a(0), |d| {
        d.refs += 1;
        d.refs
    })
    .unwrap_or(1)
}
fn release_(c: &mut Ctx) -> u32 {
    let Some(b) = base(c.a(0)) else { return 0 };
    let n = dx(b, |d| {
        d.refs = d.refs.saturating_sub(1);
        d.refs
    })
    .unwrap_or(0);
    if n == 0 {
        if let Some(d) = DXS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b) {
            release(c, d.tex);
            release(c, d.staging);
            for (_, u) in d.unks {
                release(c, u);
            }
            mem::heap_free(d.data);
            mem::heap_free(b);
        }
    }
    n
}

/// The texture's device and immediate context (references the caller releases).
fn device_context(c: &mut Ctx, tex: u32) -> Option<(u32, u32)> {
    let p = mem::heap_alloc(2 * ps(), true);
    com(c, tex, TEX_GET_DEVICE, &[p]);
    let dev = rp(p);
    if dev != 0 {
        com(c, dev, DEV_IMMEDIATE_CONTEXT, &[p + ps()]);
    }
    let ctx = rp(p + ps());
    mem::heap_free(p);
    if dev == 0 || ctx == 0 {
        release(c, dev);
        return None;
    }
    Some((dev, ctx))
}

/// A texture's D3D11_TEXTURE2D_DESC: width, height, mips, array size, format.
fn desc(c: &mut Ctx, tex: u32) -> [u32; 11] {
    let p = mem::heap_alloc(44, true);
    com(c, tex, TEX_GET_DESC, &[p]);
    let d = std::array::from_fn(|i| r32(p + 4 * i as u32));
    mem::heap_free(p);
    d
}

/// A new texture of `dev`: `w` x `h` of DXGI format `fmt`, `usage`/`bind`/`cpu` flags, the
/// first rows of `init` (pitch bytes each) in it if given; 0 if the device refuses it.
fn new_texture(c: &mut Ctx, dev: u32, w: u32, h: u32, fmt: u32, usage: u32, bind: u32, cpu: u32, init: Option<(u32, u32)>) -> u32 {
    // (the desc at 0, the D3D11_SUBRESOURCE_DATA at 48, the texture out at 80)
    let p = mem::heap_alloc(96, true);
    for (i, v) in [w, h, 1, 1, fmt, 1, 0, usage, bind, cpu, 0].into_iter().enumerate() {
        w32(p + 4 * i as u32, v);
    }
    let (sd, out) = (p + 48, p + 80);
    let mut arg = 0;
    if let Some((data, pitch)) = init {
        putp(sd, data);
        w32(sd + ps(), pitch);
        arg = sd;
    }
    let hr = com(c, dev, DEV_CREATE_TEXTURE2D, &[p, arg, out]);
    let t = if hr == 0 { rp(out) } else { 0 };
    mem::heap_free(p);
    t
}

/// The texture's bytes into the copy (through the staging texture); whether they could be read.
fn read_back(c: &mut Ctx, b: u32) -> bool {
    let Some((tex, sub, w, h, fmt, mut staging, data, pitch, rows)) = dx(b, |d| (d.tex, d.sub, d.w, d.h, d.fmt, d.staging, d.data, d.pitch, d.rows)) else {
        return false;
    };
    let Some((dev, ctx)) = device_context(c, tex) else { return false };
    if staging == 0 {
        // D3D11_USAGE_STAGING, CPU read | write
        staging = new_texture(c, dev, w, h, fmt, 3, 0, 0x30000, None);
        dx(b, |d| d.staging = staging);
    }
    let mut ok = false;
    if staging != 0 {
        com(c, ctx, CTX_COPY_REGION, &[staging, 0, 0, 0, 0, tex, sub, 0]);
        let m = mem::heap_alloc(2 * ps() + 8, true);
        // D3D11_MAP_READ
        if com(c, ctx, CTX_MAP, &[staging, 0, 1, 0, m]) == 0 {
            let (src, row) = (rp(m), r32(m + ps()));
            for y in 0..rows {
                mem::copy(data + y * pitch, src + y * row, pitch.min(row));
            }
            com(c, ctx, CTX_UNMAP, &[staging, 0]);
            ok = true;
        }
        mem::heap_free(m);
    }
    release(c, ctx);
    release(c, dev);
    ok
}

/// The copy's bytes into the texture.
fn write_back(c: &mut Ctx, b: u32) {
    let Some((tex, sub, data, pitch)) = dx(b, |d| (d.tex, d.sub, d.data, d.pitch)) else { return };
    let Some((dev, ctx)) = device_context(c, tex) else { return };
    com(c, ctx, CTX_UPDATE, &[tex, sub, 0, data, pitch, 0]);
    release(c, ctx);
    release(c, dev);
}

/// A lock: the bytes read on the first one (the copy as it was if they cannot be).
fn lock(c: &mut Ctx, b: u32) -> u32 {
    let first = dx(b, |d| {
        d.locks += 1;
        d.locks == 1
    })
    .unwrap_or(false);
    if first {
        read_back(c, b);
        dx(b, |d| d.read = Some(mem::slice(d.data, d.max).to_vec()));
    }
    dx(b, |d| d.data).unwrap_or(0)
}
/// An unlock: changed bytes back into the texture on the last one.
fn unlock(c: &mut Ctx, b: u32) -> u32 {
    let Some(last) = dx(b, |d| {
        if d.locks == 0 {
            return None;
        }
        d.locks -= 1;
        Some(d.locks == 0)
    })
    .flatten() else {
        return WAS_UNLOCKED;
    };
    if last {
        let changed = dx(b, |d| d.read.take().is_some_and(|r| r[..] != *mem::slice(d.data, d.max))).unwrap_or(false);
        if changed {
            write_back(c, b);
        }
    }
    0
}

// --- IMFMediaBuffer ---------------------------------------------------------------------------

/// Lock(BYTE **, DWORD *max, DWORD *current)
fn mb_lock(c: &mut Ctx) -> u32 {
    let (pp, pmax, plen) = (c.a(1), c.a(2), c.a(3));
    if pp == 0 {
        return E_POINTER;
    }
    let Some(b) = base(c.a(0)) else { return E_INVALIDARG };
    let data = lock(c, b);
    putp(pp, data);
    let (m, l) = dx(b, |d| (d.max, d.len)).unwrap_or_default();
    if pmax != 0 {
        w32(pmax, m);
    }
    if plen != 0 {
        w32(plen, l);
    }
    0
}
fn mb_unlock(c: &mut Ctx) -> u32 {
    let Some(b) = base(c.a(0)) else { return E_INVALIDARG };
    unlock(c, b)
}
fn mb_get_len(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), with(c.a(0), |d| d.len).unwrap_or(0));
    0
}
fn mb_set_len(c: &mut Ctx) -> u32 {
    let n = c.a(1);
    with(c.a(0), |d| {
        if n > d.max {
            E_INVALIDARG
        } else {
            d.len = n;
            0
        }
    })
    .unwrap_or(E_INVALIDARG)
}
fn mb_get_max(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), with(c.a(0), |d| d.max).unwrap_or(0));
    0
}

// --- IMFDXGIBuffer ----------------------------------------------------------------------------

/// GetResource(REFIID, void **): the texture, as the interface asked for
fn db_resource(c: &mut Ctx) -> u32 {
    let (iid, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let Some(tex) = with(c.a(0), |d| d.tex) else { return E_INVALIDARG };
    com(c, tex, 0, &[iid, out])
}
/// GetSubresourceIndex(UINT *)
fn db_sub(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), with(c.a(0), |d| d.sub).unwrap_or(0));
    0
}
/// GetUnknown(REFGUID, REFIID, void **): the object set with that key, as `riid`
fn db_get_unknown(c: &mut Ctx) -> u32 {
    let (key, iid, out) = (c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    let k = guid_at(key);
    let Some(u) = with(c.a(0), |d| d.unks.iter().find(|x| x.0 == k).map(|x| x.1)).flatten() else {
        putp(out, 0);
        return MF_E_NOT_FOUND;
    };
    com(c, u, 0, &[iid, out])
}
/// SetUnknown(REFGUID, IUnknown *): an object kept under a key (NULL: forget it)
fn db_set_unknown(c: &mut Ctx) -> u32 {
    let (key, u) = (guid_at(c.a(1)), c.a(2));
    let Some(b) = base(c.a(0)) else { return E_INVALIDARG };
    let had = dx(b, |d| d.unks.iter().position(|x| x.0 == key)).flatten();
    match (had, u) {
        (Some(_), u) if u != 0 => ALREADY,
        (Some(i), _) => {
            let old = dx(b, |d| d.unks.remove(i).1).unwrap_or(0);
            release(c, old);
            0
        }
        (None, 0) => 0,
        (None, u) => {
            add_ref(c, u);
            dx(b, |d| d.unks.push((key, u)));
            0
        }
    }
}

// --- IMF2DBuffer ------------------------------------------------------------------------------

/// The first row's address and the pitch (negative for a bottom-up buffer).
fn scan0(d: &Dx) -> (u32, i32) {
    if d.bottom_up {
        (d.data + (d.rows.max(1) - 1) * d.pitch, -(d.pitch as i32))
    } else {
        (d.data, d.pitch as i32)
    }
}
/// Lock2D(BYTE **scanline0, LONG *pitch)
fn b2_lock(c: &mut Ctx) -> u32 {
    let (pp, ppitch) = (c.a(1), c.a(2));
    if pp == 0 || ppitch == 0 {
        return E_POINTER;
    }
    let Some(b) = base(c.a(0)) else { return E_INVALIDARG };
    lock(c, b);
    let (s, p) = dx(b, |d| scan0(d)).unwrap_or_default();
    putp(pp, s);
    w32(ppitch, p as u32);
    0
}
fn b2_unlock(c: &mut Ctx) -> u32 {
    let Some(b) = base(c.a(0)) else { return E_INVALIDARG };
    unlock(c, b)
}
/// GetScanline0AndPitch(BYTE **, LONG *): only while locked
fn b2_scan0(c: &mut Ctx) -> u32 {
    let (pp, ppitch) = (c.a(1), c.a(2));
    if pp == 0 || ppitch == 0 {
        return E_POINTER;
    }
    let Some(x) = with(c.a(0), |d| (d.locks > 0).then(|| scan0(d))) else { return E_INVALIDARG };
    let Some((s, p)) = x else { return WAS_UNLOCKED };
    putp(pp, s);
    w32(ppitch, p as u32);
    0
}
/// IsContiguousFormat(BOOL *)
fn b2_contiguous(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), 0);
    0
}
/// GetContiguousLength(DWORD *)
fn b2_length(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), with(c.a(0), |d| d.max).unwrap_or(0));
    0
}
/// ContiguousCopyTo(BYTE *, DWORD): the rows top-down
fn b2_copy_to(c: &mut Ctx) -> u32 {
    let (dst, n) = (c.a(1), c.a(2));
    let Some(b) = base(c.a(0)) else { return E_INVALIDARG };
    let max = dx(b, |d| d.max).unwrap_or(0);
    if n < max {
        return E_INVALIDARG;
    }
    if dx(b, |d| d.locks > 0).unwrap_or(false) {
        return WAS_LOCKED;
    }
    let data = lock(c, b);
    mem::copy(dst, data, max);
    unlock(c, b);
    0
}
/// ContiguousCopyFrom(const BYTE *, DWORD)
fn b2_copy_from(c: &mut Ctx) -> u32 {
    let (src, n) = (c.a(1), c.a(2));
    let Some(b) = base(c.a(0)) else { return E_INVALIDARG };
    let max = dx(b, |d| d.max).unwrap_or(0);
    if src == 0 {
        return E_POINTER;
    }
    if dx(b, |d| d.locks > 0).unwrap_or(false) {
        return WAS_LOCKED;
    }
    let data = lock(c, b);
    mem::copy(data, src, n.min(max));
    unlock(c, b);
    0
}

fn vtables() -> [u32; 3] {
    static VT: OnceLock<[u32; 3]> = OnceLock::new();
    *VT.get_or_init(|| {
        [
            vtable(
                "IMFMediaBuffer (DXGI)",
                &[(qi as Handler, 12), (add_ref_, 4), (release_, 4), (mb_lock, 16), (mb_unlock, 4), (mb_get_len, 8), (mb_set_len, 8), (mb_get_max, 8)],
            ),
            vtable(
                "IMFDXGIBuffer",
                &[(qi as Handler, 12), (add_ref_, 4), (release_, 4), (db_resource, 12), (db_sub, 8), (db_get_unknown, 16), (db_set_unknown, 12)],
            ),
            vtable(
                "IMF2DBuffer (DXGI)",
                &[
                    (qi as Handler, 12),
                    (add_ref_, 4),
                    (release_, 4),
                    (b2_lock, 12),
                    (b2_unlock, 4),
                    (b2_scan0, 12),
                    (b2_contiguous, 8),
                    (b2_length, 8),
                    (b2_copy_to, 12),
                    (b2_copy_from, 12),
                ],
            ),
        ]
    })
}

/// The bytes and rows of a `w` x `h` picture of DXGI format `fmt` (None: not one we copy).
fn layout(fmt: u32, w: u32, h: u32) -> Option<(u32, u32)> {
    kura::d3d11::format::info(fmt)?;
    let size = kura::d3d11::format::texture_bytes(fmt, w, h, 1, 1, 1, 1);
    // (rows of blocks; NV12's luma then chroma rows)
    let rows = kura::d3d11::format::pitch_rows(fmt, w, h).1;
    (size > 0 && size <= MAX_BLOB as u64 && rows > 0).then(|| (size as u32, rows))
}

/// A buffer of ours over texture `tex` (whose reference it takes), subresource `sub`; `len`
/// its current length, `init` the copy (a heap block of its bytes it takes) or a new one. None: a
/// texture of a format we do not copy (`init` is the caller's then).
fn new_buffer(c: &mut Ctx, tex: u32, sub: u32, bottom_up: bool, len_is_max: bool, init: Option<u32>) -> Option<u32> {
    let d = desc(c, tex);
    let (w, h, fmt) = (d[0], d[1], d[4]);
    let (max, rows) = layout(fmt, w, h)?;
    let vt = vtables();
    let b = mem::heap_alloc(3 * ps(), true);
    for (k, v) in vt.into_iter().enumerate() {
        wp(b + k as u32 * ps(), v);
    }
    let data = init.unwrap_or_else(|| mem::heap_alloc(max, true));
    let dxo = Dx {
        refs: 1,
        tex,
        sub,
        bottom_up,
        data,
        max,
        len: if len_is_max { max } else { 0 },
        pitch: max / rows,
        rows,
        w,
        h,
        fmt,
        staging: 0,
        locks: 0,
        read: None,
        unks: Vec::new(),
    };
    DXS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, dxo);
    Some(b)
}

/// MFCreateDXGISurfaceBuffer(REFIID, IUnknown *surface, UINT subresource, BOOL bottomUp,
/// IMFMediaBuffer **)
fn MFCreateDXGISurfaceBuffer(c: &mut Ctx) -> u32 {
    let (iid, surface, sub, bottom_up, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    if iid == 0 || surface == 0 {
        return E_INVALIDARG;
    }
    let g = guid_at(iid);
    if !is(&g, TEX2D.0, TEX2D.1, TEX2D.2) {
        return E_INVALIDARG;
    }
    let p = mem::heap_alloc(ps(), true);
    let hr = com(c, surface, 0, &[iid, p]);
    let tex = rp(p);
    mem::heap_free(p);
    if hr != 0 || tex == 0 {
        return if hr != 0 { hr } else { E_NOINTERFACE };
    }
    let d = desc(c, tex);
    if sub >= d[2].max(1) * d[3].max(1) {
        release(c, tex);
        return E_INVALIDARG;
    }
    match new_buffer(c, tex, sub, bottom_up != 0, false, None) {
        Some(b) => {
            putp(out, b);
            0
        }
        None => {
            release(c, tex);
            E_INVALIDARG
        }
    }
}

/// The DXGI format the reader makes textures of for an output FOURCC (NV12, RGB32, ARGB32).
pub fn format_of(sub: u32) -> Option<u32> {
    match sub {
        source::RGB32 => Some(88),
        source::ARGB32 => Some(87),
        source::NV12 => Some(103),
        _ => None,
    }
}

/// A buffer over a new texture of `dev` holding a decoded frame (`w` x `h`, DXGI `fmt`,
/// top-down rows of `bytes`), as the reader hands it out; None: the device refused it.
pub fn frame_buffer(c: &mut Ctx, dev: u32, w: u32, h: u32, fmt: u32, bytes: &[u8]) -> Option<u32> {
    let (max, rows) = layout(fmt, w, h)?;
    if bytes.len() < max as usize {
        return None;
    }
    // (the initial data stays the buffer's copy: kura may read it after CreateTexture2D)
    let data = mem::heap_alloc(max, false);
    mem::write(data, &bytes[..max as usize]);
    // D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, and D3D11_BIND_RENDER_TARGET for RGB
    // (the video processor's output; NV12 is the decoder's, which kura draws no target of)
    let bind = if fmt == 103 { 0x8 } else { 0x28 };
    let tex = new_texture(c, dev, w, h, fmt, 0, bind, 0, Some((data, max / rows)));
    let b = if tex != 0 { new_buffer(c, tex, 0, false, true, Some(data)) } else { None };
    if b.is_none() {
        release(c, tex);
        mem::heap_free(data);
    }
    b
}

/// The device of IMFDXGIDeviceManager `m` of ours (a reference the caller releases), or 0.
pub fn manager_device(c: &mut Ctx, m: u32) -> u32 {
    let dev = manager(m, |x| x.device).unwrap_or(0);
    add_ref(c, dev);
    dev
}

pub fn init() {
    implement("mfplat", "MFCreateDXGISurfaceBuffer", MFCreateDXGISurfaceBuffer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn d(w: u32, h: u32, bottom_up: bool) -> Dx {
        let (max, rows) = layout(87, w, h).unwrap();
        Dx { refs: 1, tex: 0, sub: 0, bottom_up, data: 0x1000, max, len: 0, pitch: max / rows, rows, w, h, fmt: 87, staging: 0, locks: 0, read: None, unks: Vec::new() }
    }

    proptest! {
        /// The copy of a texture: 4-byte formats w * 4 a row, h rows; block-compressed ones a row
        /// of blocks (BC1: 8 bytes a 4x4 block); IMF2DBuffer's first row and pitch either way up.
        #[test]
        fn rows_of_a_texture(w in 1u32..2048, h in 1u32..2048, bottom_up in any::<bool>()) {
            for fmt in [28, 87, 88] {
                prop_assert_eq!(layout(fmt, w, h), Some((w * h * 4, h)));
            }
            prop_assert_eq!(layout(71, w, h), Some((w.div_ceil(4) * h.div_ceil(4) * 8, h.div_ceil(4))));
            let x = d(w, h, bottom_up);
            let (s, p) = scan0(&x);
            if bottom_up {
                prop_assert_eq!((s, p), (0x1000 + (h - 1) * w * 4, -((w * 4) as i32)));
            } else {
                prop_assert_eq!((s, p), (0x1000, (w * 4) as i32));
            }
            // (the last row ends at the copy's end either way)
            prop_assert_eq!((s as i64 + if bottom_up { 0 } else { p as i64 * (h as i64 - 1) }) + (w * 4) as i64, if bottom_up { 0x1000 + x.max as i64 } else { 0x1000 + x.max as i64 });
        }
    }

    /// The reader's textures: RGB32 as B8G8R8X8, ARGB32 as B8G8R8A8, the YUV types none.
    #[test]
    fn reader_formats() {
        assert_eq!(format_of(source::RGB32), Some(88));
        assert_eq!(format_of(source::ARGB32), Some(87));
        assert_eq!(format_of(source::NV12), Some(103));
        // (NV12: w x h luma, w x h / 2 chroma rows, w a row)
        assert_eq!(layout(103, 64, 48), Some((64 * 72, 72)));
        for f in source::DECODED.iter().filter(|&&f| !matches!(f, source::RGB32 | source::ARGB32 | source::NV12)) {
            assert_eq!(format_of(*f), None);
        }
        assert_eq!(layout(0, 4, 4), None, "an unknown format");
    }
}
