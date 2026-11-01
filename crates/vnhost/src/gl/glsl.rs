//! GLSL programs (OpenGL 2.x, GLSL 1.10-1.30 with the compatibility built-ins): the sources are
//! rewritten into Vulkan-style GLSL 450 (locations, sets and bindings, uniforms gathered into
//! one block per stage, samplers split into texture + sampler, built-ins as our own inputs,
//! outputs and uniforms), naga makes WGSL of that. Uniform values are kept by name and laid out
//! into each stage's block (offsets from naga) at every draw.

use super::*;
use std::fmt::Write as _;

pub(crate) struct Shader {
    pixel: bool,
    source: String,
    ok: bool,
    log: String,
    deleted: bool,
}

#[derive(Default)]
pub(crate) struct Program {
    shaders: Vec<u32>,
    bound_attribs: HashMap<String, u32>,
    linked: Option<Linked>,
    log: String,
    /// uniform values by name: raw 32-bit words
    values: HashMap<String, Vec<u32>>,
    /// glGetUniformLocation: index -> (name, element)
    locations: Vec<(String, u32)>,
}

struct Stage {
    shader: u32,
    /// the uniform block: (member name, offset, type), size
    block: Option<(Vec<(String, u32, naga::TypeInner, Vec<naga::TypeInner>)>, u32)>,
    /// sampler uniforms in binding order
    samplers: Vec<String>,
}

struct Linked {
    vs: Stage,
    ps: Stage,
    /// vertex inputs: (name, location)
    attribs: Vec<(String, u32)>,
    /// uniform name -> words per element
    sizes: HashMap<String, u32>,
}

const GL_VERTEX_SHADER: u32 = 0x8B31;

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if b[i] == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Replaces identifiers by `f` (None: keep).
fn map_idents(s: &str, f: &mut dyn FnMut(&str, &str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if (b[i].is_ascii_alphabetic() || b[i] == '_') && (i == 0 || !is_ident(b[i - 1])) {
            let st = i;
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            let id: String = b[st..i].iter().collect();
            let rest: String = b[i..b.len().min(i + 4)].iter().collect();
            match f(&id, &rest) {
                Some(r) => {
                    out.push_str(&r);
                    // gl_TexCoord[n] -> vn_TexCoordn eats "[n]"
                    if id == "gl_TexCoord" || id == "gl_TextureMatrix" || id == "gl_FragData" || id == "gl_MultiTexCoord" {
                        if let Some(close) = b[i..].iter().position(|&c| c == ']') {
                            i += close + 1;
                        }
                    }
                }
                None => out.push_str(&id),
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

struct Decl {
    qual: String,
    ty: String,
    names: Vec<String>,
}

/// A global declaration "qualifiers type a, b[2];" (None: something else).
fn parse_decl(stmt: &str) -> Option<Decl> {
    let words: Vec<&str> = stmt.split_whitespace().collect();
    let quals = ["attribute", "varying", "uniform", "in", "out"];
    let skip = ["highp", "mediump", "lowp", "centroid", "flat", "smooth", "noperspective", "invariant", "const"];
    let mut q = None;
    let mut rest = Vec::new();
    for w in &words {
        if quals.contains(w) && q.is_none() {
            q = Some(w.to_string());
        } else if !skip.contains(w) {
            rest.push(*w);
        }
    }
    let q = q?;
    if rest.first()?.starts_with("layout") {
        return None;
    }
    let ty = rest.first()?.to_string();
    let names = rest[1..].join(" ");
    if names.contains('(') || names.contains('{') || names.contains('=') {
        return None;
    }
    let names = names.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
    Some(Decl { qual: q, ty, names })
}

fn base_name(n: &str) -> &str {
    n.split('[').next().unwrap_or(n).trim()
}

/// Rewrites one stage. `attribs`: vertex input locations by name (assigned as found when
/// missing); `varyings`: locations by name, shared by both stages.
fn rewrite(src: &str, pixel: bool, attribs: &mut Vec<(String, u32)>, bound: &HashMap<String, u32>, varyings: &mut Vec<String>) -> (String, Vec<String>) {
    let set = pixel as u32;
    let src = strip_comments(src);
    let mut head = String::from("#version 450\n");
    let mut body = String::new();
    let mut block = String::new();
    let mut samplers: Vec<String> = Vec::new();
    let mut extra_in: Vec<(String, String)> = Vec::new();
    let mut extra_out: Vec<(String, String)> = Vec::new();
    let mut extra_uni: Vec<(String, String)> = Vec::new();
    let mut frag_out = false;

    let att_loc = |name: &str, attribs: &mut Vec<(String, u32)>| -> u32 {
        if let Some(a) = attribs.iter().find(|a| a.0 == name) {
            return a.1;
        }
        let l = bound.get(name).copied().unwrap_or_else(|| (0..16).find(|l| !attribs.iter().any(|a| a.1 == *l) && !bound.values().any(|b| b == l)).unwrap_or(15));
        attribs.push((name.to_string(), l));
        l
    };
    let var_loc = |name: &str, varyings: &mut Vec<String>| -> usize {
        match varyings.iter().position(|v| v == name) {
            Some(i) => i,
            None => {
                varyings.push(name.to_string());
                varyings.len() - 1
            }
        }
    };

    // built-ins
    let src = map_idents(&src, &mut |id, rest| {
        let digit = rest.chars().nth(1).filter(|c| c.is_ascii_digit());
        Some(match id {
            "gl_Vertex" if !pixel => {
                extra_in.push(("vec4".into(), "vn_Vertex".into()));
                "vn_Vertex".into()
            }
            "gl_Color" if !pixel => {
                extra_in.push(("vec4".into(), "vn_Color".into()));
                "vn_Color".into()
            }
            "gl_Normal" if !pixel => {
                extra_in.push(("vec3".into(), "vn_Normal".into()));
                "vn_Normal".into()
            }
            _ if id.starts_with("gl_MultiTexCoord") && !pixel => {
                let n = format!("vn_{}", &id[3..]);
                extra_in.push(("vec4".into(), n.clone()));
                n
            }
            "gl_Color" | "gl_FrontColor" | "gl_BackColor" => {
                let n = "vn_FrontColor".to_string();
                if pixel { extra_in.push(("vec4".into(), n.clone())) } else { extra_out.push(("vec4".into(), n.clone())) }
                n
            }
            "gl_TexCoord" => {
                let n = format!("vn_TexCoord{}", digit.unwrap_or('0'));
                if pixel { extra_in.push(("vec4".into(), n.clone())) } else { extra_out.push(("vec4".into(), n.clone())) }
                n
            }
            "gl_FragColor" | "gl_FragData" => {
                frag_out = true;
                "vn_FragColor".into()
            }
            "gl_ModelViewProjectionMatrix" => {
                extra_uni.push(("mat4".into(), "vn_MVP".into()));
                "vn_MVP".into()
            }
            "gl_ModelViewMatrix" => {
                extra_uni.push(("mat4".into(), "vn_MV".into()));
                "vn_MV".into()
            }
            "gl_ProjectionMatrix" => {
                extra_uni.push(("mat4".into(), "vn_P".into()));
                "vn_P".into()
            }
            "gl_NormalMatrix" => {
                extra_uni.push(("mat3".into(), "vn_NM".into()));
                "vn_NM".into()
            }
            "gl_TextureMatrix" => {
                let n = format!("vn_TM{}", digit.unwrap_or('0'));
                extra_uni.push(("mat4".into(), n.clone()));
                n
            }
            "texture2D" | "textureCube" | "texture1D" | "shadow2D" => "texture".into(),
            "texture2DProj" => "textureProj".into(),
            "texture2DLod" => "textureLod".into(),
            "highp" | "mediump" | "lowp" => String::new(),
            _ => return None,
        })
    });

    // global statements
    let chars: Vec<char> = src.chars().collect();
    let mut depth = 0;
    let mut stmt = String::new();
    let mut i = 0;
    let mut line_start = true;
    while i < chars.len() {
        let ch = chars[i];
        if depth == 0 && line_start && ch == '#' {
            let end = chars[i..].iter().position(|&c| c == '\n').map_or(chars.len(), |p| i + p);
            let line: String = chars[i..end].iter().collect();
            if !line.trim_start_matches('#').trim_start().starts_with("version") && !line.contains("extension") {
                body.push_str(&line);
                body.push('\n');
            }
            i = end;
            continue;
        }
        line_start = ch == '\n' || (line_start && ch.is_whitespace());
        match ch {
            '{' => {
                depth += 1;
                stmt.push(ch);
            }
            '}' => {
                depth -= 1;
                stmt.push(ch);
                if depth == 0 {
                    body.push_str(&stmt);
                    stmt.clear();
                }
            }
            ';' if depth == 0 => {
                let s = stmt.trim().to_string();
                stmt.clear();
                if s.starts_with("precision") {
                } else if let Some(d) = parse_decl(&s) {
                    for n in &d.names {
                        let b = base_name(n);
                        match d.qual.as_str() {
                            "uniform" if d.ty.starts_with("sampler") => samplers.push(b.to_string()),
                            "uniform" => {
                                let _ = writeln!(block, "  {} {};", d.ty, n);
                            }
                            "attribute" | "in" if !pixel => {
                                let l = att_loc(b, attribs);
                                let _ = writeln!(head, "layout(location = {l}) in {} {};", d.ty, n);
                            }
                            "varying" | "out" if !pixel => {
                                let l = var_loc(b, varyings);
                                let flat = if d.ty.starts_with('i') || d.ty.starts_with('u') { "flat " } else { "" };
                                let _ = writeln!(head, "layout(location = {l}) {flat}out {} {};", d.ty, n);
                            }
                            "varying" | "in" => {
                                let l = var_loc(b, varyings);
                                let flat = if d.ty.starts_with('i') || d.ty.starts_with('u') { "flat " } else { "" };
                                let _ = writeln!(head, "layout(location = {l}) {flat}in {} {};", d.ty, n);
                            }
                            _ => {
                                // fragment output (GLSL 1.30)
                                let _ = writeln!(head, "layout(location = 0) out {} {};", d.ty, n);
                            }
                        }
                    }
                } else {
                    body.push_str(&s);
                    body.push_str(";\n");
                }
            }
            _ => stmt.push(ch),
        }
        i += 1;
    }
    body.push_str(&stmt);

    // built-in declarations
    let mut seen = HashSet::new();
    for (ty, n) in &extra_in {
        if seen.insert(n.clone()) {
            if pixel {
                let l = var_loc(n, varyings);
                let _ = writeln!(head, "layout(location = {l}) in {ty} {n};");
            } else {
                let l = att_loc(n, attribs);
                let _ = writeln!(head, "layout(location = {l}) in {ty} {n};");
            }
        }
    }
    for (ty, n) in &extra_out {
        if seen.insert(n.clone()) {
            let l = var_loc(n, varyings);
            let _ = writeln!(head, "layout(location = {l}) out {ty} {n};");
        }
    }
    for (ty, n) in &extra_uni {
        if seen.insert(n.clone()) {
            let _ = writeln!(block, "  {ty} {n};");
        }
    }
    if frag_out {
        head.push_str("layout(location = 0) out vec4 vn_FragColor;\n");
    }
    if !block.is_empty() {
        let _ = write!(head, "layout(set = {set}, binding = 0) uniform VnU{set} {{\n{block}}};\n");
    }
    // samplers: texture + sampler, uses as sampler2D(t, s)
    for (k, s) in samplers.iter().enumerate() {
        let _ = writeln!(head, "layout(set = {set}, binding = {}) uniform texture2D {s}_vnt;", 16 + k);
        let _ = writeln!(head, "layout(set = {set}, binding = {}) uniform sampler {s}_vns;", 160 + k);
    }
    let body = map_idents(&body, &mut |id, _| {
        if samplers.iter().any(|s| s == id) {
            Some(format!("sampler2D({id}_vnt, {id}_vns)"))
        } else if id == "main" && !pixel {
            Some("vn_main".into())
        } else {
            None
        }
    });
    let mut out = head;
    out.push_str(&body);
    if !pixel {
        out.push_str("\nvoid main() {\n  vn_main();\n  gl_Position.y = -gl_Position.y;\n  gl_Position.z = (gl_Position.z + gl_Position.w) * 0.5;\n}\n");
    }
    (out, samplers)
}

/// GLSL 450 -> (WGSL, module)
fn compile(src: &str, pixel: bool) -> Result<(String, naga::Module), String> {
    let stage = if pixel { naga::ShaderStage::Fragment } else { naga::ShaderStage::Vertex };
    let m = naga::front::glsl::Frontend::default()
        .parse(&naga::front::glsl::Options::from(stage), src)
        .map_err(|e| format!("{e:?}"))?;
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&m)
        .map_err(|e| format!("naga validation: {e:?}"))?;
    let text = naga::back::wgsl::write_string(&m, &info, naga::back::wgsl::WriterFlags::empty()).map_err(|e| format!("naga wgsl-out: {e}"))?;
    Ok((format!("diagnostic(off, derivative_uniformity);\n{text}"), m))
}

fn block_of(m: &naga::Module) -> Option<(Vec<(String, u32, naga::TypeInner, Vec<naga::TypeInner>)>, u32)> {
    for (_, g) in m.global_variables.iter() {
        if g.space != naga::AddressSpace::Uniform {
            continue;
        }
        if let naga::TypeInner::Struct { members, span } = &m.types[g.ty].inner {
            let ms = members
                .iter()
                .map(|x| {
                    let inner = m.types[x.ty].inner.clone();
                    // (element type of arrays)
                    let mut chain = Vec::new();
                    if let naga::TypeInner::Array { base, .. } = &inner {
                        chain.push(m.types[*base].inner.clone());
                    }
                    (x.name.clone().unwrap_or_default(), x.offset, inner, chain)
                })
                .collect();
            return Some((ms, *span));
        }
    }
    None
}

/// 32-bit words one element of a value takes in GL (glUniform data)
fn words(t: &naga::TypeInner) -> u32 {
    match t {
        naga::TypeInner::Vector { size, .. } => *size as u32,
        naga::TypeInner::Matrix { columns, rows, .. } => *columns as u32 * *rows as u32,
        _ => 1,
    }
}

/// Writes GL-packed `v` (elements of `t`) into a std140-style block at `off`.
fn put(out: &mut [u8], off: u32, t: &naga::TypeInner, elem: Option<&naga::TypeInner>, stride: u32, v: &[u32]) {
    let mut w = |o: u32, x: u32| {
        if let Some(d) = out.get_mut(o as usize..o as usize + 4) {
            d.copy_from_slice(&x.to_le_bytes());
        }
    };
    let one = |t: &naga::TypeInner, base: u32, v: &[u32], w: &mut dyn FnMut(u32, u32)| match t {
        naga::TypeInner::Matrix { columns, rows, .. } => {
            let (c, r) = (*columns as u32, *rows as u32);
            let cstride = if r == 2 { 8 } else { 16 };
            for ci in 0..c {
                for ri in 0..r {
                    if let Some(&x) = v.get((ci * r + ri) as usize) {
                        w(base + ci * cstride + ri * 4, x);
                    }
                }
            }
        }
        _ => {
            for (k, &x) in v.iter().take(words(t) as usize).enumerate() {
                w(base + k as u32 * 4, x);
            }
        }
    };
    match (t, elem) {
        (naga::TypeInner::Array { size, .. }, Some(e)) => {
            let n = match size {
                naga::ArraySize::Constant(n) => n.get(),
                _ => 1,
            };
            let ew = words(e) as usize;
            for k in 0..n {
                let s = &v[(k as usize * ew).min(v.len())..((k as usize + 1) * ew).min(v.len())];
                one(e, off + k * stride, s, &mut w);
            }
        }
        _ => one(t, off, v, &mut w),
    }
}

fn link(g: &mut Gl, p: &mut Program) -> Result<Linked, String> {
    let find = |pixel: bool| p.shaders.iter().copied().find(|s| g.shaders.get(s).is_some_and(|x| x.pixel == pixel));
    let (vs, ps) = (find(false).ok_or("no vertex shader")?, find(true).ok_or("no fragment shader")?);
    let mut attribs = Vec::new();
    let mut varyings = Vec::new();
    let (vsrc, vsamp) = rewrite(&g.shaders[&vs].source, false, &mut attribs, &p.bound_attribs, &mut varyings);
    let (psrc, psamp) = rewrite(&g.shaders[&ps].source, true, &mut attribs, &p.bound_attribs, &mut varyings);
    let (vw, vm) = compile(&vsrc, false).map_err(|e| format!("vertex shader: {e}\n{vsrc}"))?;
    let (pw, pm) = compile(&psrc, true).map_err(|e| format!("fragment shader: {e}\n{psrc}"))?;
    let mut sizes = HashMap::new();
    for m in [&vm, &pm] {
        if let Some((ms, _)) = block_of(m) {
            for (n, _, t, chain) in ms {
                sizes.insert(n, words(chain.first().unwrap_or(&t)));
            }
        }
    }
    for s in vsamp.iter().chain(&psamp) {
        sizes.insert(s.clone(), 1);
    }
    Ok(Linked {
        vs: Stage { shader: shader(&vw), block: block_of(&vm), samplers: vsamp },
        ps: Stage { shader: shader(&pw), block: block_of(&pm), samplers: psamp },
        attribs,
        sizes,
    })
}

// ---- the GL entries ----

pub(crate) fn proc_of(n: &str) -> Option<*const c_void> {
    procs!(n,
        glCreateShader, glShaderSource, glCompileShader, glGetShaderiv, glGetShaderInfoLog, glDeleteShader,
        glIsShader, glCreateProgram, glAttachShader, glDetachShader, glBindAttribLocation, glLinkProgram,
        glGetProgramiv, glGetProgramInfoLog, glUseProgram, glDeleteProgram, glIsProgram, glValidateProgram,
        glGetUniformLocation, glGetAttribLocation, glUniform1i, glUniform2i, glUniform3i, glUniform4i, glUniform1f,
        glUniform2f, glUniform3f, glUniform4f, glUniform1iv, glUniform2iv, glUniform3iv, glUniform4iv, glUniform1fv,
        glUniform2fv, glUniform3fv, glUniform4fv, glUniformMatrix2fv, glUniformMatrix3fv, glUniformMatrix4fv,
        glVertexAttribPointer, glEnableVertexAttribArray, glDisableVertexAttribArray, glVertexAttrib1f,
        glVertexAttrib2f, glVertexAttrib3f, glVertexAttrib4f, glVertexAttrib4fv, glVertexAttrib2fv,
        glVertexAttrib3fv, glCreateShaderObject, glCreateProgramObject, glUseProgramObject, glAttachObject,
        glDeleteObject, glGetObjectParameteriv, glGetInfoLog, glGetShaderSource, glGetAttachedShaders,
    )
}

fn gl() -> std::sync::MutexGuard<'static, Option<Gl>> {
    lock()
}

extern "C" fn glCreateShader(ty: u32) -> u32 {
    let mut g = gl();
    let g = g.as_mut().unwrap();
    let id = g.next_name;
    g.next_name += 1;
    g.shaders.insert(id, Shader { pixel: ty != GL_VERTEX_SHADER, source: String::new(), ok: false, log: String::new(), deleted: false });
    id
}
extern "C" fn glCreateShaderObject(ty: u32) -> u32 {
    glCreateShader(ty)
}
unsafe extern "C" fn glShaderSource(sh: u32, count: i32, strings: *const *const c_char, lengths: *const i32) {
    let mut s = String::new();
    for k in 0..count.max(0) as usize {
        let p = *strings.add(k);
        if p.is_null() {
            continue;
        }
        let len = if lengths.is_null() || *lengths.add(k) < 0 { CStr::from_ptr(p).to_bytes().len() } else { *lengths.add(k) as usize };
        s.push_str(&String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, len)));
    }
    if let Some(x) = gl().as_mut().unwrap().shaders.get_mut(&sh) {
        x.source = s;
    }
}
unsafe extern "C" fn glGetShaderSource(sh: u32, max: i32, len: *mut i32, out: *mut c_char) {
    let s = gl().as_ref().unwrap().shaders.get(&sh).map(|x| x.source.clone()).unwrap_or_default();
    copy_str(&s, max, len, out)
}
extern "C" fn glCompileShader(sh: u32) {
    let mut g = gl();
    let Some(x) = g.as_mut().unwrap().shaders.get_mut(&sh) else { return };
    // (checked on its own here; linking does it again with the other stage)
    let (src, _) = rewrite(&x.source, x.pixel, &mut Vec::new(), &HashMap::new(), &mut Vec::new());
    match compile(&src, x.pixel) {
        Ok(_) => {
            x.ok = true;
            x.log.clear();
        }
        Err(e) => {
            x.ok = false;
            x.log = e;
            eprintln!("gl: shader {sh} does not compile: {}\n{src}", x.log);
        }
    }
}
unsafe extern "C" fn glGetShaderiv(sh: u32, p: u32, out: *mut i32) {
    let g = gl();
    let Some(x) = g.as_ref().unwrap().shaders.get(&sh) else { return };
    *out = match p {
        0x8B4F => if x.pixel { 0x8B30 } else { 0x8B31 },
        0x8B80 => x.deleted as i32,
        0x8B81 => x.ok as i32,
        0x8B84 => x.log.len() as i32 + 1,
        0x8B88 => x.source.len() as i32 + 1,
        _ => 0,
    }
}
unsafe fn copy_str(s: &str, max: i32, len: *mut i32, out: *mut c_char) {
    let n = s.len().min((max.max(1) - 1) as usize);
    if !out.is_null() && max > 0 {
        std::ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, out, n);
        *out.add(n) = 0;
    }
    if !len.is_null() {
        *len = n as i32;
    }
}
unsafe extern "C" fn glGetShaderInfoLog(sh: u32, max: i32, len: *mut i32, out: *mut c_char) {
    let s = gl().as_ref().unwrap().shaders.get(&sh).map(|x| x.log.clone()).unwrap_or_default();
    copy_str(&s, max, len, out)
}
extern "C" fn glDeleteShader(sh: u32) {
    let mut g = gl();
    if let Some(x) = g.as_mut().unwrap().shaders.get_mut(&sh) {
        x.deleted = true;
    }
}
extern "C" fn glIsShader(sh: u32) -> u8 {
    gl().as_ref().unwrap().shaders.contains_key(&sh) as u8
}
extern "C" fn glCreateProgram() -> u32 {
    let mut g = gl();
    let g = g.as_mut().unwrap();
    let id = g.next_name;
    g.next_name += 1;
    g.programs.insert(id, Program::default());
    id
}
extern "C" fn glCreateProgramObject() -> u32 {
    glCreateProgram()
}
extern "C" fn glAttachShader(p: u32, sh: u32) {
    if let Some(x) = gl().as_mut().unwrap().programs.get_mut(&p) {
        if !x.shaders.contains(&sh) {
            x.shaders.push(sh);
        }
    }
}
extern "C" fn glAttachObject(p: u32, sh: u32) {
    glAttachShader(p, sh)
}
extern "C" fn glDetachShader(p: u32, sh: u32) {
    if let Some(x) = gl().as_mut().unwrap().programs.get_mut(&p) {
        x.shaders.retain(|&s| s != sh);
    }
}
unsafe extern "C" fn glGetAttachedShaders(p: u32, max: i32, count: *mut i32, out: *mut u32) {
    let v = gl().as_ref().unwrap().programs.get(&p).map(|x| x.shaders.clone()).unwrap_or_default();
    let n = v.len().min(max.max(0) as usize);
    for (k, s) in v.iter().take(n).enumerate() {
        *out.add(k) = *s;
    }
    if !count.is_null() {
        *count = n as i32;
    }
}
unsafe extern "C" fn glBindAttribLocation(p: u32, idx: u32, name: *const c_char) {
    let n = CStr::from_ptr(name).to_string_lossy().into_owned();
    if let Some(x) = gl().as_mut().unwrap().programs.get_mut(&p) {
        x.bound_attribs.insert(n, idx);
    }
}
extern "C" fn glLinkProgram(p: u32) {
    let mut g = gl();
    let g = g.as_mut().unwrap();
    let Some(mut x) = g.programs.remove(&p) else { return };
    match link(g, &mut x) {
        Ok(l) => {
            x.linked = Some(l);
            x.log.clear();
        }
        Err(e) => {
            eprintln!("gl: program {p} does not link: {e}");
            x.linked = None;
            x.log = e;
        }
    }
    x.locations.clear();
    g.programs.insert(p, x);
}
extern "C" fn glValidateProgram(_p: u32) {}
unsafe extern "C" fn glGetProgramiv(p: u32, pname: u32, out: *mut i32) {
    let g = gl();
    let Some(x) = g.as_ref().unwrap().programs.get(&p) else { return };
    *out = match pname {
        0x8B82 | 0x8B83 => x.linked.is_some() as i32,
        0x8B84 => x.log.len() as i32 + 1,
        0x8B85 => x.shaders.len() as i32,
        0x8B86 => x.linked.as_ref().map_or(0, |l| l.sizes.len() as i32),
        0x8B89 => x.linked.as_ref().map_or(0, |l| l.attribs.len() as i32),
        0x8B87 | 0x8B8A => 64,
        _ => 0,
    }
}
unsafe extern "C" fn glGetObjectParameteriv(obj: u32, pname: u32, out: *mut i32) {
    let is_shader = gl().as_ref().unwrap().shaders.contains_key(&obj);
    let pname = if pname == 0x8B80 && !is_shader { pname } else { pname };
    if is_shader {
        glGetShaderiv(obj, pname, out)
    } else {
        glGetProgramiv(obj, pname, out)
    }
}
unsafe extern "C" fn glGetProgramInfoLog(p: u32, max: i32, len: *mut i32, out: *mut c_char) {
    let s = gl().as_ref().unwrap().programs.get(&p).map(|x| x.log.clone()).unwrap_or_default();
    copy_str(&s, max, len, out)
}
unsafe extern "C" fn glGetInfoLog(obj: u32, max: i32, len: *mut i32, out: *mut c_char) {
    if gl().as_ref().unwrap().shaders.contains_key(&obj) {
        glGetShaderInfoLog(obj, max, len, out)
    } else {
        glGetProgramInfoLog(obj, max, len, out)
    }
}
extern "C" fn glUseProgram(p: u32) {
    with(|_, c| c.program = p)
}
extern "C" fn glUseProgramObject(p: u32) {
    glUseProgram(p)
}
extern "C" fn glDeleteProgram(p: u32) {
    let _ = p;
}
extern "C" fn glDeleteObject(_o: u32) {}
extern "C" fn glIsProgram(p: u32) -> u8 {
    gl().as_ref().unwrap().programs.contains_key(&p) as u8
}
unsafe extern "C" fn glGetUniformLocation(p: u32, name: *const c_char) -> i32 {
    let n = CStr::from_ptr(name).to_string_lossy().into_owned();
    let mut g = gl();
    let Some(x) = g.as_mut().unwrap().programs.get_mut(&p) else { return -1 };
    let Some(l) = &x.linked else { return -1 };
    let (base, elem) = match n.find('[') {
        Some(i) => (n[..i].to_string(), n[i + 1..].trim_end_matches(']').parse().unwrap_or(0)),
        None => (n.clone(), 0),
    };
    if !l.sizes.contains_key(&base) {
        return -1;
    }
    let key = (base, elem);
    let idx = match x.locations.iter().position(|k| *k == key) {
        Some(i) => i,
        None => {
            x.locations.push(key);
            x.locations.len() - 1
        }
    };
    idx as i32
}
unsafe extern "C" fn glGetAttribLocation(p: u32, name: *const c_char) -> i32 {
    let n = CStr::from_ptr(name).to_string_lossy().into_owned();
    let g = gl();
    let Some(x) = g.as_ref().unwrap().programs.get(&p) else { return -1 };
    x.linked.as_ref().and_then(|l| l.attribs.iter().find(|a| a.0 == n)).map_or(-1, |a| a.1 as i32)
}

/// glUniform*: `n` words per element, `count` elements.
fn uniform(loc: i32, n: usize, count: usize, v: &[u32]) {
    if loc < 0 {
        return;
    }
    with(|g, c| {
        let Some(x) = g.programs.get_mut(&c.program) else { return c.err(GL_INVALID_OPERATION) };
        let Some((name, elem)) = x.locations.get(loc as usize).cloned() else { return };
        let per = x.linked.as_ref().and_then(|l| l.sizes.get(&name)).copied().unwrap_or(n as u32) as usize;
        let val = x.values.entry(name).or_default();
        let at = elem as usize * per;
        for k in 0..count {
            let dst = at + k * per;
            if val.len() < dst + per {
                val.resize(dst + per, 0);
            }
            val[dst..dst + n.min(per)].copy_from_slice(&v[k * n..k * n + n.min(per)]);
        }
    })
}
extern "C" fn glUniform1i(l: i32, a: i32) {
    uniform(l, 1, 1, &[a as u32])
}
extern "C" fn glUniform2i(l: i32, a: i32, b: i32) {
    uniform(l, 2, 1, &[a as u32, b as u32])
}
extern "C" fn glUniform3i(l: i32, a: i32, b: i32, c: i32) {
    uniform(l, 3, 1, &[a as u32, b as u32, c as u32])
}
extern "C" fn glUniform4i(l: i32, a: i32, b: i32, c: i32, d: i32) {
    uniform(l, 4, 1, &[a as u32, b as u32, c as u32, d as u32])
}
extern "C" fn glUniform1f(l: i32, a: f32) {
    uniform(l, 1, 1, &[a.to_bits()])
}
extern "C" fn glUniform2f(l: i32, a: f32, b: f32) {
    uniform(l, 2, 1, &[a.to_bits(), b.to_bits()])
}
extern "C" fn glUniform3f(l: i32, a: f32, b: f32, c: f32) {
    uniform(l, 3, 1, &[a.to_bits(), b.to_bits(), c.to_bits()])
}
extern "C" fn glUniform4f(l: i32, a: f32, b: f32, c: f32, d: f32) {
    uniform(l, 4, 1, &[a.to_bits(), b.to_bits(), c.to_bits(), d.to_bits()])
}
unsafe fn uv(l: i32, n: usize, count: i32, p: *const u32) {
    let k = count.max(0) as usize;
    uniform(l, n, k, std::slice::from_raw_parts(p, n * k))
}
unsafe extern "C" fn glUniform1iv(l: i32, c: i32, p: *const i32) {
    uv(l, 1, c, p as _)
}
unsafe extern "C" fn glUniform2iv(l: i32, c: i32, p: *const i32) {
    uv(l, 2, c, p as _)
}
unsafe extern "C" fn glUniform3iv(l: i32, c: i32, p: *const i32) {
    uv(l, 3, c, p as _)
}
unsafe extern "C" fn glUniform4iv(l: i32, c: i32, p: *const i32) {
    uv(l, 4, c, p as _)
}
unsafe extern "C" fn glUniform1fv(l: i32, c: i32, p: *const f32) {
    uv(l, 1, c, p as _)
}
unsafe extern "C" fn glUniform2fv(l: i32, c: i32, p: *const f32) {
    uv(l, 2, c, p as _)
}
unsafe extern "C" fn glUniform3fv(l: i32, c: i32, p: *const f32) {
    uv(l, 3, c, p as _)
}
unsafe extern "C" fn glUniform4fv(l: i32, c: i32, p: *const f32) {
    uv(l, 4, c, p as _)
}
unsafe fn umat(l: i32, n: usize, count: i32, transpose: u8, p: *const f32) {
    let k = count.max(0) as usize;
    let mut v: Vec<u32> = std::slice::from_raw_parts(p as *const u32, n * n * k).to_vec();
    if transpose != 0 {
        for m in 0..k {
            for i in 0..n {
                for j in i + 1..n {
                    v.swap(m * n * n + i * n + j, m * n * n + j * n + i);
                }
            }
        }
    }
    uniform(l, n * n, k, &v)
}
unsafe extern "C" fn glUniformMatrix2fv(l: i32, c: i32, t: u8, p: *const f32) {
    umat(l, 2, c, t, p)
}
unsafe extern "C" fn glUniformMatrix3fv(l: i32, c: i32, t: u8, p: *const f32) {
    umat(l, 3, c, t, p)
}
unsafe extern "C" fn glUniformMatrix4fv(l: i32, c: i32, t: u8, p: *const f32) {
    umat(l, 4, c, t, p)
}
extern "C" fn glVertexAttribPointer(i: u32, size: i32, ty: u32, norm: u8, stride: i32, ptr: u32) {
    with(|_, c| {
        if let Some(a) = c.attribs.get_mut(i as usize) {
            *a = Array { on: a.on, size, ty, stride, ptr, buffer: c.array_buffer, normalized: norm != 0 };
        }
    })
}
fn attrib_on(i: u32, on: bool) {
    with(|_, c| {
        if let Some(a) = c.attribs.get_mut(i as usize) {
            a.on = on;
        }
    })
}
extern "C" fn glEnableVertexAttribArray(i: u32) {
    attrib_on(i, true)
}
extern "C" fn glDisableVertexAttribArray(i: u32) {
    attrib_on(i, false)
}
fn attrib(i: u32, v: [f32; 4]) {
    with(|_, c| {
        if let Some(a) = c.attrib_values.get_mut(i as usize) {
            *a = v;
        }
    })
}
extern "C" fn glVertexAttrib1f(i: u32, x: f32) {
    attrib(i, [x, 0.0, 0.0, 1.0])
}
extern "C" fn glVertexAttrib2f(i: u32, x: f32, y: f32) {
    attrib(i, [x, y, 0.0, 1.0])
}
extern "C" fn glVertexAttrib3f(i: u32, x: f32, y: f32, z: f32) {
    attrib(i, [x, y, z, 1.0])
}
extern "C" fn glVertexAttrib4f(i: u32, x: f32, y: f32, z: f32, w: f32) {
    attrib(i, [x, y, z, w])
}
unsafe extern "C" fn glVertexAttrib4fv(i: u32, v: *const f32) {
    attrib(i, [*v, *v.add(1), *v.add(2), *v.add(3)])
}
unsafe extern "C" fn glVertexAttrib3fv(i: u32, v: *const f32) {
    attrib(i, [*v, *v.add(1), *v.add(2), 1.0])
}
unsafe extern "C" fn glVertexAttrib2fv(i: u32, v: *const f32) {
    attrib(i, [*v, *v.add(1), 0.0, 1.0])
}

// ---- drawing ----

/// Where vertices come from: array elements (indices) or glBegin/glEnd vertices.
pub(crate) enum Src<'a> {
    Indices(&'a [u32]),
    Immediate(&'a [[f32; 16]]),
}

pub(crate) fn draw(g: &mut Gl, c: &mut Ctx, mode: u32, idx: &[u32]) {
    draw_from(g, c, mode, Src::Indices(idx))
}

fn builtin(name: &str) -> Option<usize> {
    Some(match name {
        "vn_Vertex" => 0,
        "vn_Color" => 1,
        "vn_MultiTexCoord0" => 2,
        "vn_MultiTexCoord1" => 3,
        _ => return None,
    })
}

pub(crate) fn draw_from(g: &mut Gl, c: &mut Ctx, mode: u32, src: Src) {
    let n = match src {
        Src::Indices(i) => i.len(),
        Src::Immediate(v) => v.len(),
    };
    let (topo, order) = assemble(mode, n);
    if order.is_empty() {
        return;
    }
    let Some((color, level, depth, w, h)) = target(g, c) else { return };
    let Some(p) = g.programs.get(&c.program) else { return };
    let Some(l) = &p.linked else { return };
    let attribs = l.attribs.clone();
    let (vs, ps) = (l.vs.shader, l.ps.shader);
    // vertices: every input as 4 floats
    let na = attribs.len().max(1);
    let mut verts: Vec<f32> = Vec::with_capacity(order.len() * na * 4);
    for &o in &order {
        for (name, loc) in &attribs {
            let v: [f32; 4] = match (&src, builtin(name)) {
                (Src::Immediate(v), Some(b)) => v[o][b * 4..b * 4 + 4].try_into().unwrap(),
                (Src::Immediate(_), None) => c.attrib_values[*loc as usize],
                (Src::Indices(ix), b) => {
                    let i = ix[o];
                    let arr = match b {
                        Some(0) => Some(c.vertex),
                        Some(1) => Some(c.colors),
                        Some(2) => Some(c.units[0].array),
                        Some(3) => Some(c.units[1].array),
                        _ => c.attribs.get(*loc as usize).copied(),
                    };
                    match (arr, b) {
                        (Some(a), _) if a.on => fetch(g, &a, i, [0.0, 0.0, 0.0, 1.0]),
                        (_, Some(1)) => c.color,
                        (_, Some(2)) => c.units[0].coords,
                        (_, Some(3)) => c.units[1].coords,
                        _ => c.attrib_values[(*loc as usize).min(15)],
                    }
                }
            };
            verts.extend_from_slice(&v);
        }
        if attribs.is_empty() {
            verts.extend_from_slice(&[0.0; 4]);
        }
    }
    let mut vertex = vec![1, (na * 16) as u32, 0, attribs.len() as u32];
    for (k, (_, loc)) in attribs.iter().enumerate() {
        vertex.extend_from_slice(&[3, k as u32 * 16, *loc]);
    }
    let pipe = pipeline(g, pipeline_key(c, vs, ps, topo, depth != 0, &vertex));
    // uniform blocks and textures
    let p = &g.programs[&c.program];
    let l = p.linked.as_ref().unwrap();
    let mvp = mul(c.projection.last().unwrap(), c.modelview.last().unwrap());
    let builtin_value = |n: &str| -> Option<Vec<u32>> {
        let m: &[f32] = match n {
            "vn_MVP" => &mvp,
            "vn_MV" => c.modelview.last().unwrap(),
            "vn_P" => c.projection.last().unwrap(),
            "vn_TM0" => c.units[0].matrices.last().unwrap(),
            "vn_TM1" => c.units[1].matrices.last().unwrap(),
            "vn_NM" => &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            _ => return None,
        };
        Some(m.iter().map(|f| f.to_bits()).collect())
    };
    let mut groups: Vec<(Vec<u8>, Vec<[u32; 8]>)> = Vec::new();
    for st in [&l.vs, &l.ps] {
        let mut block = Vec::new();
        if let Some((ms, size)) = &st.block {
            block = vec![0u8; (*size as usize).max(16)];
            for (n, off, t, chain) in ms {
                let stride = match t {
                    naga::TypeInner::Array { stride, .. } => *stride,
                    _ => 0,
                };
                if let Some(v) = builtin_value(n).or_else(|| p.values.get(n).cloned()) {
                    put(&mut block, *off, t, chain.first(), stride, &v);
                }
            }
        }
        let mut entries = Vec::new();
        for (k, s) in st.samplers.iter().enumerate() {
            let unit = p.values.get(s).and_then(|v| v.first()).copied().unwrap_or(0) as usize;
            let t = c.units.get(unit).and_then(|u| g.textures.get(&u.bound)).filter(|t| t.gpu != 0 && t.base != 5);
            let (key, maxlod) = sampler(t);
            entries.push([16 + k as u32, 1, t.map_or(0, |t| t.gpu), 1, 0, t.map_or(1, |t| t.levels), 0, 1]);
            entries.push([160 + k as u32, 2, key, 0, maxlod, 0, 0, 0]);
        }
        groups.push((block, entries));
    }
    let state = draw_state(c, w, h);
    gpu::record(|q| {
        q.cmd(&[gpu::SET_TARGETS2, (color != 0) as u32]);
        if color != 0 {
            q.cmd(&[color, level, 0]);
        }
        q.cmd(&[depth, 0, 0]);
        let vo = q.data(bytes(&verts), 16);
        let mut wds = vec![gpu::DRAW2, pipe];
        wds.extend_from_slice(&state);
        wds.extend_from_slice(&[1, DATA, vo, 0, 0, 0]);
        for (block, entries) in &groups {
            let nb = !block.is_empty() as u32;
            wds.push(nb + entries.len() as u32);
            if nb != 0 {
                let off = q.data(block, 256);
                wds.extend_from_slice(&[0, 0, DATA, off, block.len() as u32, block.len() as u32, 0, 0]);
            }
            for e in entries {
                wds.extend_from_slice(e);
            }
        }
        wds.extend_from_slice(&[0, order.len() as u32, 1, 0, 0, 0]);
        q.cmd(&wds);
    });
}
