//! d3dcompiler_33..47: D3DCompile and friends on sake's own HLSL compiler (crate::hlsl), blobs,
//! signature parts.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, r32, w32};

// ---- ID3DBlob: {vtable, refs, data, size} (fields past a pointer-sized vtable slot) ----

fn blob_ptr(c: &mut Ctx) -> u32 {
    r32(c.a(0) + 12)
}
fn blob_size(c: &mut Ctx) -> u32 {
    r32(c.a(0) + 16)
}
fn blob_qi(c: &mut Ctx) -> u32 {
    // IUnknown and ID3D10Blob {8BA5FB08-5195-40e2-AC58-0D989C3A0102}
    let g = mem::slice(c.a(1), 16).to_vec();
    const UNK: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
    const BLOB: [u8; 16] = [0x08, 0xfb, 0xa5, 0x8b, 0x95, 0x51, 0xe2, 0x40, 0xac, 0x58, 0x0d, 0x98, 0x9c, 0x3a, 0x01, 0x02];
    if g == UNK || g == BLOB {
        blob_add_ref(c);
        mem::putp(c.a(2), c.a(0));
        return 0;
    }
    mem::putp(c.a(2), 0);
    0x8000_4002
}
fn blob_add_ref(c: &mut Ctx) -> u32 {
    let n = r32(c.a(0) + 8) + 1;
    w32(c.a(0) + 8, n);
    n
}
fn blob_release(c: &mut Ctx) -> u32 {
    let n = r32(c.a(0) + 8).saturating_sub(1);
    w32(c.a(0) + 8, n);
    n
}

/// A new ID3DBlob holding `data`.
pub fn blob(data: &[u8]) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        crate::api::vtable("ID3DBlob", &[(blob_qi, 12), (blob_add_ref, 4), (blob_release, 4), (blob_ptr, 4), (blob_size, 4)])
    });
    let o = mem::heap_alloc(24, true);
    let p = mem::heap_alloc((data.len() as u32).max(4), true);
    mem::write(p, data);
    mem::wp(o, vt);
    w32(o + 8, 1);
    w32(o + 12, p);
    w32(o + 16, data.len() as u32);
    o
}

fn text_blob(s: &str) -> u32 {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    blob(&b)
}

/// D3D_SHADER_MACRO array (name, definition pairs, NULL-terminated)
fn defines(p: u32) -> Vec<(String, String)> {
    let mut v = Vec::new();
    if p == 0 {
        return v;
    }
    let k = mem::ps();
    let mut at = p;
    loop {
        let n = mem::rp(at);
        if n == 0 || v.len() > 1024 {
            break;
        }
        let d = mem::rp(at + k);
        let name = crate::k32::arg_str(n, false);
        let def = if d == 0 { "1".to_string() } else { crate::k32::arg_str(d, false) };
        v.push((name, def));
        at += 2 * k;
    }
    v
}

fn read_file(name: &str) -> Option<Vec<u8>> {
    crate::files::resolve(name).and_then(|h| kura::fs::read(h).ok())
}

fn dir_of(name: &str) -> String {
    match name.rfind(['\\', '/']) {
        Some(i) => name[..=i].to_string(),
        None => String::new(),
    }
}

/// An include handler: the guest's ID3DInclude, or files beside `base` for
/// D3D_COMPILE_STANDARD_FILE_INCLUDE (1).
fn open_include(c: &mut Ctx, inc: u32, base: &str, name: &str, system: bool) -> Option<String> {
    if inc == 0 {
        return None;
    }
    if inc == 1 {
        let data = read_file(&format!("{base}{name}")).or_else(|| read_file(name))?;
        return Some(String::from_utf8_lossy(&data).into_owned());
    }
    // Open(this, type, name, parent, &data, &bytes); Close(this, data)
    let k = mem::ps();
    let scratch = mem::heap_alloc(name.len() as u32 + 1 + 2 * k, true);
    mem::write(scratch + 2 * k, name.as_bytes());
    let open = mem::rp(mem::rp(inc));
    let close = mem::rp(mem::rp(inc) + k);
    let hr = c.call(open, &[inc, system as u32, scratch + 2 * k, 0, scratch, scratch + k]);
    if hr & 0x8000_0000 != 0 {
        return None;
    }
    let data = mem::rp(scratch);
    let n = r32(scratch + k);
    let text = String::from_utf8_lossy(mem::slice(data, n)).into_owned();
    c.call(close, &[inc, data]);
    Some(text)
}

#[allow(clippy::too_many_arguments)]
fn compile(c: &mut Ctx, src: &[u8], name: &str, defs: u32, inc: u32, entry: u32, target: u32, flags: u32, out: u32, errs: u32) -> u32 {
    let src = String::from_utf8_lossy(src).into_owned();
    let src = src.trim_end_matches('\0');
    let entry = if entry == 0 { "main".to_string() } else { crate::k32::arg_str(entry, false) };
    let target = crate::k32::arg_str(target, false);
    let defines = defines(defs);
    let base = dir_of(name);
    let mut include = |n: &str, sys: bool| open_include(c, inc, &base, n, sys);
    let r = crate::hlsl::compile(src, &defines, &mut include, &entry, &target, flags);
    mem::putp(errs, 0);
    match r {
        Ok(code) => {
            mem::putp(out, blob(&code));
            0
        }
        Err(e) => {
            let msg = format!("{}: error X3000: {e}", if name.is_empty() { "memory" } else { name });
            eprintln!("sake: D3DCompile({entry}, {target}): {msg}");
            if std::env::var_os("SAKE_HLSL_DUMP").is_some() {
                eprintln!("{src}");
            }
            mem::putp(out, 0);
            mem::putp(errs, text_blob(&msg));
            0x8000_4005
        }
    }
}

/// D3DCompile(data, size, name, defines, include, entry, target, flags1, flags2, code, errors)
fn D3DCompile(c: &mut Ctx) -> u32 {
    let src = mem::slice(c.a(0), c.a(1)).to_vec();
    let name = if c.a(2) == 0 { String::new() } else { crate::k32::arg_str(c.a(2), false) };
    let (d, i, e, t, f, o, er) = (c.a(3), c.a(4), c.a(5), c.a(6), c.a(7), c.a(9), c.a(10));
    compile(c, &src, &name, d, i, e, t, f, o, er)
}

/// D3DCompile2(.., flags2, secondary flags, secondary data, its size, code, errors)
fn D3DCompile2(c: &mut Ctx) -> u32 {
    let src = mem::slice(c.a(0), c.a(1)).to_vec();
    let name = if c.a(2) == 0 { String::new() } else { crate::k32::arg_str(c.a(2), false) };
    let (d, i, e, t, f, o, er) = (c.a(3), c.a(4), c.a(5), c.a(6), c.a(7), c.a(12), c.a(13));
    compile(c, &src, &name, d, i, e, t, f, o, er)
}

/// D3DCompileFromFile(file, defines, include, entry, target, flags1, flags2, code, errors)
fn D3DCompileFromFile(c: &mut Ctx) -> u32 {
    let name = crate::k32::arg_str(c.a(0), true);
    let Some(src) = read_file(&name) else {
        mem::putp(c.a(7), 0);
        mem::putp(c.a(8), 0);
        return 0x8007_0002;
    };
    let (d, i, e, t, f, o, er) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(7), c.a(8));
    compile(c, &src, &name, d, i, e, t, f, o, er)
}

/// D3DPreprocess(data, size, name, defines, include, text, errors)
fn D3DPreprocess(c: &mut Ctx) -> u32 {
    let src = String::from_utf8_lossy(mem::slice(c.a(0), c.a(1))).into_owned();
    let name = if c.a(2) == 0 { String::new() } else { crate::k32::arg_str(c.a(2), false) };
    let (defs, inc, out, errs) = (c.a(3), c.a(4), c.a(5), c.a(6));
    let defines = defines(defs);
    let base = dir_of(&name);
    let mut include = |n: &str, sys: bool| open_include(c, inc, &base, n, sys);
    let r = crate::hlsl::preprocess(src.trim_end_matches('\0'), &defines, &mut include);
    mem::putp(errs, 0);
    match r {
        Ok(toks) => {
            let mut s = String::new();
            let mut line = 0;
            for t in toks {
                if t.line != line {
                    s.push('\n');
                    line = t.line;
                } else {
                    s.push(' ');
                }
                s.push_str(&crate::hlsl::pp::text(&t));
            }
            s.push('\n');
            mem::putp(out, text_blob(&s));
            0
        }
        Err(e) => {
            mem::putp(out, 0);
            mem::putp(errs, text_blob(&e));
            0x8000_4005
        }
    }
}

fn D3DCreateBlob(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), blob(&vec![0; c.a(0) as usize]));
    0
}

/// A DXBC container of the chunks `tags` of `code`.
fn part(code: &[u8], tags: &[&[u8; 4]]) -> Option<Vec<u8>> {
    let cont = kura::d3d11::dxbc::Container::parse(code).ok()?;
    let mut chunks = Vec::new();
    for t in tags {
        chunks.push((**t, cont.chunk(t)?.to_vec()));
    }
    Some(crate::hlsl::sm4::container(&chunks))
}

fn sig_blob(c: &mut Ctx, tags: &[&[u8; 4]], out: u32) -> u32 {
    let code = mem::slice(c.a(0), c.a(1)).to_vec();
    match part(&code, tags) {
        Some(b) => {
            mem::putp(out, blob(&b));
            0
        }
        None => {
            mem::putp(out, 0);
            0x8007_0057
        }
    }
}

fn D3DGetInputSignatureBlob(c: &mut Ctx) -> u32 {
    let o = c.a(2);
    sig_blob(c, &[b"ISGN"], o)
}
fn D3DGetOutputSignatureBlob(c: &mut Ctx) -> u32 {
    let o = c.a(2);
    sig_blob(c, &[b"OSGN"], o)
}
fn D3DGetInputAndOutputSignatureBlob(c: &mut Ctx) -> u32 {
    let o = c.a(2);
    sig_blob(c, &[b"ISGN", b"OSGN"], o)
}

/// D3DGetBlobPart(data, size, part, flags, blob): 0 input, 1 output, 2 both signatures, 5 debug
/// info, 8 private data...
fn D3DGetBlobPart(c: &mut Ctx) -> u32 {
    let o = c.a(4);
    match c.a(2) {
        0 => sig_blob(c, &[b"ISGN"], o),
        1 => sig_blob(c, &[b"OSGN"], o),
        2 => sig_blob(c, &[b"ISGN", b"OSGN"], o),
        _ => {
            mem::putp(o, 0);
            0x8000_4005
        }
    }
}

/// D3DStripShader(data, size, flags, blob): nothing of ours to strip
fn D3DStripShader(c: &mut Ctx) -> u32 {
    let code = mem::slice(c.a(0), c.a(1)).to_vec();
    mem::putp(c.a(3), blob(&code));
    0
}

fn D3DReadFileToBlob(c: &mut Ctx) -> u32 {
    let name = crate::k32::arg_str(c.a(0), true);
    match read_file(&name) {
        Some(d) => {
            mem::putp(c.a(1), blob(&d));
            0
        }
        None => {
            mem::putp(c.a(1), 0);
            0x8007_0002
        }
    }
}

/// D3DDisassemble(data, size, flags, comments, text)
fn D3DDisassemble(c: &mut Ctx) -> u32 {
    mem::putp(c.a(4), text_blob("// (no disassembly in sake)\n"));
    0
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("D3DCompile", D3DCompile),
        ("D3DCompile2", D3DCompile2),
        ("D3DCompileFromFile", D3DCompileFromFile),
        ("D3DPreprocess", D3DPreprocess),
        ("D3DCreateBlob", D3DCreateBlob),
        ("D3DGetInputSignatureBlob", D3DGetInputSignatureBlob),
        ("D3DGetOutputSignatureBlob", D3DGetOutputSignatureBlob),
        ("D3DGetInputAndOutputSignatureBlob", D3DGetInputAndOutputSignatureBlob),
        ("D3DGetBlobPart", D3DGetBlobPart),
        ("D3DStripShader", D3DStripShader),
        ("D3DReadFileToBlob", D3DReadFileToBlob),
        ("D3DDisassemble", D3DDisassemble),
        ("D3DReflect", crate::d3dreflect::D3DReflect),
    ];
    for v in 33..=47 {
        let dll = format!("d3dcompiler_{v}");
        for (n, f) in fns {
            implement(&dll, n, *f);
        }
    }
}
