//! GLSL (OpenGL 2.x, GLSL 1.10-1.30 with the compatibility built-ins) to WGSL: the sources are
//! rewritten into Vulkan-style GLSL 450 (locations, sets and bindings, uniforms gathered into
//! one block per stage, samplers split into texture + sampler, built-ins as our own `vn_*`
//! inputs, outputs and uniforms), naga makes WGSL of that.
//!
//! [`link`] takes a program's vertex and fragment source and gives each stage's WGSL, its
//! uniform block layout ([`Member::put`] writes glUniform data into it) and samplers.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// One linked stage
pub struct Stage {
    pub wgsl: String,
    /// the stage's uniform block (group = stage, binding 0)
    pub block: Option<Block>,
    /// sampler uniforms in binding order (texture 1 + 2k, sampler 2 + 2k)
    pub samplers: Vec<String>,
}

pub struct Block {
    pub members: Vec<Member>,
    pub size: u32,
}

/// A uniform in a block: `count` elements (None: not an array) of `cols` x `rows` 32-bit values
/// (a vector is 1 x n, a scalar 1 x 1).
pub struct Member {
    pub name: String,
    pub offset: u32,
    pub cols: u32,
    pub rows: u32,
    pub count: Option<u32>,
    /// array stride
    pub stride: u32,
}

pub struct Program {
    pub vs: Stage,
    pub ps: Stage,
    /// vertex inputs: (name, location)
    pub attribs: Vec<(String, u32)>,
    /// uniform name -> 32-bit words per element (samplers: 1)
    pub sizes: HashMap<String, u32>,
}

/// glLinkProgram: vertex and fragment source, glBindAttribLocation's locations.
pub fn link(vs: &str, ps: &str, bound: &HashMap<String, u32>) -> Result<Program, String> {
    let mut attribs = Vec::new();
    let mut varyings = Vec::new();
    let (vsrc, vsamp) = rewrite(vs, false, &mut attribs, bound, &mut varyings);
    let (psrc, psamp) = rewrite(ps, true, &mut attribs, bound, &mut varyings);
    let (vw, vm) = compile(&vsrc, false).map_err(|e| format!("vertex shader: {e}\n{vsrc}"))?;
    let (pw, pm) = compile(&psrc, true).map_err(|e| format!("fragment shader: {e}\n{psrc}"))?;
    let (vb, pb) = (block_of(&vm), block_of(&pm));
    let mut sizes = HashMap::new();
    for b in [&vb, &pb].into_iter().flatten() {
        for m in &b.members {
            sizes.insert(m.name.clone(), m.words());
        }
    }
    for s in vsamp.iter().chain(&psamp) {
        sizes.insert(s.clone(), 1);
    }
    Ok(Program {
        vs: Stage { wgsl: vw, block: vb, samplers: vsamp },
        ps: Stage { wgsl: pw, block: pb, samplers: psamp },
        attribs,
        sizes,
    })
}

/// glCompileShader: one stage on its own (linking does it again with the other stage);
/// Err((message, the rewritten GLSL 450)).
pub fn check(src: &str, pixel: bool) -> Result<(), (String, String)> {
    let (src, _) = rewrite(src, pixel, &mut Vec::new(), &HashMap::new(), &mut Vec::new());
    compile(&src, pixel).map(|_| ()).map_err(|e| (e, src))
}

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
    let mut two_rows: Vec<(String, &str)> = Vec::new();
    let mut narrow_arrays: Vec<(String, &str)> = Vec::new();
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
                                // (2-row matrices have no std140 layout in WGSL: kept as 4-row ones
                                // of which the shader reads the top two rows, see `TWO_ROWS`)
                                // (arrays of scalars and short vectors: elements 16 bytes apart in
                                // std140, which WGSL cannot say of them; kept as vec4 arrays, see
                                // `NARROW`)
                                let narrow = NARROW.iter().find(|t| t.0 == d.ty).filter(|_| n.contains('['));
                                match (TWO_ROWS.iter().find(|t| t.0 == d.ty && !n.contains('[')), narrow) {
                                    (Some(&(_, wide, gl)), _) => {
                                        let _ = writeln!(block, "  {wide} {n}{TWO_ROWS_SUFFIX};");
                                        two_rows.push((n.clone(), gl));
                                    }
                                    (None, Some(&(_, wide, swz))) => {
                                        let b = base_name(n);
                                        let _ = writeln!(block, "  {wide} {b}{NARROW_SUFFIX}{}{};", swz.len(), &n[b.len()..]);
                                        narrow_arrays.push((b.to_string(), swz));
                                    }
                                    _ => {
                                        let _ = writeln!(block, "  {} {};", d.ty, n);
                                    }
                                }
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
    let mut body = body;
    for (n, swz) in &narrow_arrays {
        body = index_narrow(&body, n, &format!("{n}{NARROW_SUFFIX}{}", swz.len()), swz);
    }
    let body = map_idents(&body, &mut |id, _| {
        if samplers.iter().any(|s| s == id) {
            Some(format!("sampler2D({id}_vnt, {id}_vns)"))
        } else if let Some((_, gl)) = two_rows.iter().find(|t| t.0 == id) {
            Some(format!("{gl}({id}{TWO_ROWS_SUFFIX})"))
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
    // (naga's GLSL front end panics on some malformed sources: an error where unwinding exists)
    let m = std::panic::catch_unwind(|| naga::front::glsl::Frontend::default().parse(&naga::front::glsl::Options::from(stage), src))
        .map_err(|_| "naga's GLSL front end panicked".to_string())?
        .map_err(|e| format!("{e:?}"))?;
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&m)
        .map_err(|e| format!("naga validation: {e:?}"))?;
    let text = naga::back::wgsl::write_string(&m, &info, naga::back::wgsl::WriterFlags::empty()).map_err(|e| format!("naga wgsl-out: {e}"))?;
    Ok((format!("diagnostic(off, derivative_uniformity);\n{text}"), m))
}

/// Uniform matrices with two rows: (GLSL type, the 4-row type the block holds, the constructor
/// that takes its top rows back).
const TWO_ROWS: [(&str, &str, &str); 4] = [("mat2", "mat2x4", "mat2"), ("mat2x2", "mat2x4", "mat2"), ("mat3x2", "mat3x4", "mat3x2"), ("mat4x2", "mat4x4", "mat4x2")];
const TWO_ROWS_SUFFIX: &str = "_vn2r";
/// Array element types narrower than 16 bytes: (GLSL type, the vec4 type the block holds, the
/// components the shader reads).
const NARROW: [(&str, &str, &str); 6] =
    [("float", "vec4", "x"), ("vec2", "vec4", "xy"), ("vec3", "vec4", "xyz"), ("int", "ivec4", "x"), ("ivec2", "ivec4", "xy"), ("ivec3", "ivec4", "xyz")];
const NARROW_SUFFIX: &str = "_vna";

/// `name[i]` -> `wide[i].swz` in `s` (indices rewritten too).
fn index_narrow(s: &str, name: &str, wide: &str, swz: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if (b[i].is_ascii_alphabetic() || b[i] == '_') && (i == 0 || !is_ident(b[i - 1])) {
            let st = i;
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            let id: String = b[st..i].iter().collect();
            let mut j = i;
            while j < b.len() && b[j].is_whitespace() {
                j += 1;
            }
            if id == name && j < b.len() && b[j] == '[' {
                let mut depth = 0;
                let mut k = j;
                while k < b.len() {
                    match b[k] {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                let inner: String = b[j + 1..k.min(b.len())].iter().collect();
                out.push_str(&format!("{wide}[{}].{swz}", index_narrow(&inner, name, wide, swz)));
                i = k + 1;
            } else {
                out.push_str(&id);
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn block_of(m: &naga::Module) -> Option<Block> {
    let shape = |t: &naga::TypeInner| match t {
        naga::TypeInner::Vector { size, .. } => (1, *size as u32),
        naga::TypeInner::Matrix { columns, rows, .. } => (*columns as u32, *rows as u32),
        _ => (1, 1),
    };
    for (_, g) in m.global_variables.iter() {
        if g.space != naga::AddressSpace::Uniform {
            continue;
        }
        if let naga::TypeInner::Struct { members, span } = &m.types[g.ty].inner {
            let members = members
                .iter()
                .map(|x| {
                    let inner = &m.types[x.ty].inner;
                    let (cols, rows, count, stride) = match inner {
                        naga::TypeInner::Array { base, size, stride } => {
                            let n = match size {
                                naga::ArraySize::Constant(n) => n.get(),
                                _ => 1,
                            };
                            let (c, r) = shape(&m.types[*base].inner);
                            (c, r, Some(n), *stride)
                        }
                        t => {
                            let (c, r) = shape(t);
                            (c, r, None, 0)
                        }
                    };
                    let name = x.name.clone().unwrap_or_default();
                    let narrow = name.rfind(NARROW_SUFFIX).filter(|&i| name[i + NARROW_SUFFIX.len()..].parse::<u32>().is_ok());
                    match (name.strip_suffix(TWO_ROWS_SUFFIX), narrow) {
                        (Some(n), _) => Member { name: n.to_string(), offset: x.offset, cols, rows: 2, count, stride },
                        (None, Some(i)) => {
                            let rows = name[i + NARROW_SUFFIX.len()..].parse().unwrap_or(4);
                            Member { name: name[..i].to_string(), offset: x.offset, cols: 1, rows, count, stride }
                        }
                        (None, None) => Member { name, offset: x.offset, cols, rows, count, stride },
                    }
                })
                .collect();
            return Some(Block { members, size: *span });
        }
    }
    None
}

impl Member {
    /// 32-bit words one element takes in GL (glUniform data)
    pub fn words(&self) -> u32 {
        self.cols * self.rows
    }

    /// Writes GL-packed `v` (elements one after another) into the block `out` (std140-style).
    pub fn put(&self, out: &mut [u8], v: &[u32]) {
        let mut w = |o: u32, x: u32| {
            if let Some(d) = out.get_mut(o as usize..o as usize + 4) {
                d.copy_from_slice(&x.to_le_bytes());
            }
        };
        let ew = self.words() as usize;
        // matrix columns are 16 bytes apart (std140, 2-row ones too); vectors and scalars are packed
        let cstride = if self.cols == 1 { 0 } else { 16 };
        for k in 0..self.count.unwrap_or(1) {
            let base = self.offset + k * self.stride;
            let s = &v[(k as usize * ew).min(v.len())..((k as usize + 1) * ew).min(v.len())];
            for (i, &x) in s.iter().enumerate() {
                let (ci, ri) = (i as u32 / self.rows, i as u32 % self.rows);
                w(base + ci * cstride + ri * 4, x);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (vertex WGSL, fragment WGSL, samplers, attributes)
    fn translate(vs: &str, ps: &str) -> (String, String, Vec<String>, Vec<(String, u32)>) {
        let p = link(vs, ps, &HashMap::new()).unwrap_or_else(|e| panic!("{e}"));
        let mut samp = p.vs.samplers;
        samp.extend(p.ps.samplers);
        (p.vs.wgsl, p.ps.wgsl, samp, p.attribs)
    }

    #[test]
    fn fixed_function_builtins() {
        // GLSL 1.10 the old way: built-in attributes, matrices and varyings
        let (vw, pw, samp, _) = translate(
            "void main() { gl_TexCoord[0] = gl_MultiTexCoord0; gl_FrontColor = gl_Color;\n\
             gl_Position = gl_ModelViewProjectionMatrix * gl_Vertex; }",
            "uniform sampler2D tex; uniform vec4 tint;\n\
             void main() { gl_FragColor = texture2D(tex, gl_TexCoord[0].st) * gl_Color * tint; }",
        );
        assert!(vw.contains("@vertex") && pw.contains("@fragment"));
        assert_eq!(samp, ["tex"]);
    }

    #[test]
    fn attributes_varyings_uniforms() {
        // what gltest.c and engines with their own shaders write (Ren'Py's gl renderer looks like this)
        let (vw, pw, samp, attribs) = translate(
            "#ifdef GL_ES\nprecision highp float;\n#endif\n\
             uniform mat4 Projection; attribute vec4 Vertex; attribute vec2 VertexTexCoord0;\n\
             varying vec2 vTexCoord0; // comment\n\
             void main() { vTexCoord0 = VertexTexCoord0; gl_Position = Projection * Vertex; }",
            "#ifdef GL_ES\nprecision mediump float;\n#endif\n\
             uniform sampler2D tex0; uniform float Alpha; varying vec2 vTexCoord0;\n\
             /* block\n comment */ void main() { vec4 c = texture2D(tex0, vTexCoord0); gl_FragColor = c * Alpha; }",
        );
        assert!(vw.contains("@vertex") && pw.contains("@fragment"));
        assert_eq!(samp, ["tex0"]);
        let names: Vec<&str> = attribs.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"Vertex") && names.contains(&"VertexTexCoord0"), "{attribs:?}");
    }

    #[test]
    fn glsl_130_in_out() {
        let (vw, pw, _, attribs) = translate(
            "#version 130\nin vec2 pos; in vec3 col; out vec3 v_col; uniform mat4 rot;\n\
             void main() { v_col = col; gl_Position = rot * vec4(pos, 0.0, 1.0); }",
            "#version 130\nin vec3 v_col; uniform vec4 tint; out vec4 color;\n\
             void main() { color = vec4(v_col, 1.0) * tint; }",
        );
        assert!(vw.contains("@vertex") && pw.contains("@fragment"));
        assert_eq!(attribs.len(), 2, "{attribs:?}");
    }

    #[test]
    fn block_layout() {
        let p = link(
            "uniform mat4 m; uniform vec2 offs[3]; attribute vec4 v; void main() { gl_Position = m * v + vec4(offs[1], 0.0, 0.0); }",
            "uniform float a; void main() { gl_FragColor = vec4(a); }",
            &HashMap::new(),
        )
        .unwrap();
        let b = p.vs.block.as_ref().unwrap();
        let offs = b.members.iter().find(|m| m.name == "offs").unwrap();
        assert_eq!((offs.cols, offs.rows, offs.count, offs.stride), (1, 2, Some(3), 16));
        assert_eq!(p.sizes["m"], 16);
        let mut out = vec![0u8; b.size as usize];
        offs.put(&mut out, &[1, 2, 3, 4, 5, 6]);
        let at = |o: u32| u32::from_le_bytes(out[o as usize..o as usize + 4].try_into().unwrap());
        assert_eq!((at(offs.offset + 16), at(offs.offset + 20)), (3, 4));
    }
    #[test]
    fn check_reports_errors_with_the_rewritten_source() {
        assert!(check("void main() { gl_FragColor = vec4(1.0); }", true).is_ok());
        let (msg, src) = check("void main() { gl_FragColor = undefined_thing; }", true).unwrap_err();
        assert!(!msg.is_empty());
        assert!(src.starts_with("#version 450\n") && src.contains("vn_FragColor = undefined_thing"), "{src}");
    }

    #[test]
    fn identifiers() {
        assert!(is_ident('a') && is_ident('Z') && is_ident('7') && is_ident('_'));
        assert!(!is_ident('.') && !is_ident(' ') && !is_ident('['));
    }

    #[test]
    fn comments() {
        assert_eq!(strip_comments("a // b\nc"), "a \nc");
        assert_eq!(strip_comments("a /* b\nc */ d"), "a \n d");
        assert_eq!(strip_comments("a /* b */"), "a ");
        assert_eq!(strip_comments("/**/x"), "x");
        assert_eq!(strip_comments("/* * / */y"), "y");
        assert_eq!(strip_comments("x/y/"), "x/y/");
        assert_eq!(strip_comments("p /* unclosed"), "p ");
        assert_eq!(strip_comments("a/"), "a/");
        assert_eq!(strip_comments("x // tail"), "x ");
        assert_eq!(strip_comments("p /* x*"), "p ");
    }

    #[test]
    fn identifier_mapping() {
        let mut f = |id: &str, rest: &str| (id == "ab").then(|| format!("<{rest}>"));
        assert_eq!(map_idents("ab+ab1 xab _ab ab", &mut f), "<+ab1>+ab1 xab _ab <>");
        assert_eq!(map_idents("1ab ab", &mut f), "1ab <>");
        // gl_TexCoord[n] and friends take their index with them
        let mut g = |id: &str, _: &str| (id == "gl_TexCoord").then(|| "T".to_string());
        assert_eq!(map_idents("gl_TexCoord[1].x", &mut g), "T.x");
        assert_eq!(map_idents("gl_TexCoord", &mut g), "T");
        let mut h = |id: &str, _: &str| (id == "gl_FragData" || id == "gl_TextureMatrix" || id == "gl_MultiTexCoord").then(|| "F".to_string());
        assert_eq!(map_idents("gl_FragData[0] gl_TextureMatrix[2] gl_MultiTexCoord[3]", &mut h), "F F F");
    }

    #[test]
    fn declarations() {
        let d = parse_decl("uniform highp vec4 a, b[2]").unwrap();
        assert_eq!((d.qual.as_str(), d.ty.as_str(), d.names.clone()), ("uniform", "vec4", vec!["a".to_string(), "b[2]".to_string()]));
        let d = parse_decl("flat in int out_like").unwrap();
        assert_eq!((d.qual.as_str(), d.ty.as_str()), ("in", "int"));
        assert!(parse_decl("vec4 f(vec4 x)").is_none());
        assert!(parse_decl("const float k = 1.0").is_none());
        assert!(parse_decl("uniform float k = 1.0").is_none());
        assert!(parse_decl("uniform vec4 f(int)").is_none());
        assert!(parse_decl("uniform Blk { vec4 x").is_none());
        assert!(parse_decl("layout(location = 0) out vec4 c").is_none());
        assert!(parse_decl("uniform").is_none());
        assert_eq!(base_name("x[3]"), "x");
    }

    fn vs_of(src: &str) -> String {
        rewrite(src, false, &mut Vec::new(), &HashMap::new(), &mut Vec::new()).0
    }
    fn ps_of(src: &str) -> String {
        rewrite(src, true, &mut Vec::new(), &HashMap::new(), &mut Vec::new()).0
    }

    #[test]
    fn builtins_rewritten() {
        let v = vs_of(
            "void main() { gl_Position = gl_ModelViewMatrix * gl_ProjectionMatrix * gl_Vertex;\n\
             gl_FrontColor = gl_Color; gl_TexCoord[1] = gl_TextureMatrix[2] * gl_MultiTexCoord1;\n\
             vec3 n = gl_NormalMatrix * gl_Normal; }",
        );
        for want in [
            "in vec4 vn_Vertex;",
            "in vec4 vn_Color;",
            "in vec3 vn_Normal;",
            "in vec4 vn_MultiTexCoord1;",
            "out vec4 vn_FrontColor;",
            "out vec4 vn_TexCoord1;",
            "  mat4 vn_MV;\n",
            "  mat4 vn_P;\n",
            "  mat3 vn_NM;\n",
            "  mat4 vn_TM2;\n",
            "vn_TM2 * vn_MultiTexCoord1",
            "vn_NM * vn_Normal",
            "uniform VnU0 {",
            "void vn_main()",
            "gl_Position.y = -gl_Position.y;",
        ] {
            assert!(v.contains(want), "{want:?} not in\n{v}");
        }
        assert!(!v.contains("gl_Vertex") && !v.contains("gl_Color") && !v.contains("[1]"), "{v}");
        let p = ps_of(
            "uniform lowp sampler2D t; void main() { mediump vec4 c = texture2DProj(t, gl_TexCoord[0]) + texture2DLod(t, gl_TexCoord[3].xy, 0.0);\n\
             gl_FragColor = c * gl_Color; }",
        );
        for want in [
            "in vec4 vn_FrontColor;",
            "in vec4 vn_TexCoord0;",
            "in vec4 vn_TexCoord3;",
            "out vec4 vn_FragColor;",
            "textureProj(sampler2D(t_vnt, t_vns), vn_TexCoord0)",
            "textureLod(sampler2D(t_vnt, t_vns), vn_TexCoord3.xy, 0.0)",
            "layout(set = 1, binding = 16) uniform texture2D t_vnt;",
            "layout(set = 1, binding = 160) uniform sampler t_vns;",
            "void main()",
        ] {
            assert!(p.contains(want), "{want:?} not in\n{p}");
        }
        assert!(!p.contains("mediump") && !p.contains("lowp") && !p.contains("vn_main") && !p.contains("gl_Position"), "{p}");
        assert!(!p.contains("in vec4 vn_Color"), "{p}");
        // vertex inputs mean nothing in a pixel shader: left to naga to refuse
        let p = ps_of("void main() { gl_FragColor = vec4(gl_Normal, 1.0) + gl_Vertex; }");
        assert!(p.contains("vec4(gl_Normal, 1.0) + gl_Vertex") && !p.contains("vn_Normal") && !p.contains("vn_Vertex"), "{p}");
    }

    #[test]
    fn preprocessor_lines_and_scopes() {
        let v = vs_of("#version 120\n#extension GL_EXT_foo : enable\n#define K 2.0\n  #ifdef X\nuniform float a;\n#endif\nfloat f() { if (true) { return K; } return a; }\nvoid main() { gl_Position = vec4(f()); }");
        assert!(!v.contains("#version 120") && !v.contains("extension"), "{v}");
        assert!(v.contains("#define K 2.0\n") && v.contains("#ifdef X\n") && v.contains("#endif\n"), "{v}");
        // a function body stays one piece, nested braces included, and keeps its statements
        assert!(v.contains("float f() { if (true) { return K; } return a; }"), "{v}");
        assert!(v.contains("  float a;\n"), "{v}");
        // a '#' inside an expression line is not a directive
        let v = vs_of("void main() { gl_Position = vec4(1.0); }\nconst float k = 2.0;");
        assert!(v.contains("const float k = 2.0;\n"), "{v}");
        let v = vs_of("float k = 1.0; #extension x\nvoid main() { gl_Position = vec4(k); }");
        assert!(v.contains("#extension x"), "{v}");
        // declarations after functions and structs are still found
        let v = vs_of("struct S { float x; }; float f() { { return 1.0; } }\nuniform float b; void main() { gl_Position = vec4(f() + b); }");
        assert!(v.contains("  float b;\n") && v.contains("struct S { float x; }") && v.contains("float f() { { return 1.0; } }"), "{v}");
    }

    #[test]
    fn locations_shared_between_stages() {
        let mut attribs = Vec::new();
        let mut vary = Vec::new();
        let mut bound = HashMap::new();
        bound.insert("pos".to_string(), 3);
        let (v, _) = rewrite(
            "attribute vec4 pos; attribute vec4 col; varying vec4 a; varying ivec2 b; void main() { a = col; b = ivec2(1); gl_Position = pos; }",
            false,
            &mut attribs,
            &bound,
            &mut vary,
        );
        assert_eq!(attribs, [("pos".to_string(), 3), ("col".to_string(), 0)]);
        assert!(v.contains("layout(location = 3) in vec4 pos;") && v.contains("layout(location = 0) in vec4 col;"), "{v}");
        assert!(v.contains("layout(location = 0) out vec4 a;") && v.contains("layout(location = 1) flat out ivec2 b;"), "{v}");
        let (p, _) = rewrite("varying ivec2 b; varying vec4 a; void main() { gl_FragColor = a; }", true, &mut attribs, &bound, &mut vary);
        assert!(p.contains("layout(location = 1) flat in ivec2 b;") && p.contains("layout(location = 0) in vec4 a;"), "{p}");
        // GLSL 1.30 fragment outputs
        let p = ps_of("#version 130\nin vec4 a; out vec4 color; void main() { color = a; }");
        assert!(p.contains("layout(location = 0) out vec4 color;") && p.contains("layout(location = 0) in vec4 a;"), "{p}");
        let p = ps_of("flat in uint u; void main() { gl_FragColor = vec4(float(u)); }");
        assert!(p.contains("flat in uint u;"), "{p}");
    }

    #[test]
    fn narrow_and_two_row_uniforms() {
        let v = vs_of("uniform mat2 r; uniform float w[4]; uniform vec3 c[2]; uniform mat2 ra[2]; uniform float s;\n\
                       void main() { gl_Position = vec4(r * vec2(w[1], w[w[0] > 0.0 ? 2 : 3]), c[1].z, s); }");
        for want in ["  mat2x4 r_vn2r;\n", "  vec4 w_vna1[4];\n", "  vec4 c_vna3[2];\n", "  mat2 ra[2];\n", "  float s;\n", "mat2(r_vn2r)", "w_vna1[1].x", "w_vna1[w_vna1[0].x > 0.0 ? 2 : 3].x", "c_vna3[1].xyz.z"] {
            assert!(v.contains(want), "{want:?} not in\n{v}");
        }
        assert_eq!(index_narrow("w [1] + w2[0] + xw[0]", "w", "W", "x"), "W[1].x + w2[0] + xw[0]");
        assert_eq!(index_narrow("w[a[1]] w", "w", "W", "xy"), "W[a[1]].xy w");
        assert_eq!(index_narrow("w[w[1]]", "w", "W", "x"), "W[W[1].x].x");
        assert_eq!(index_narrow("w[", "w", "W", "x"), "W[].x");
        assert_eq!(index_narrow("_w[0]", "_w", "W", "x"), "W[0].x");
    }

    #[test]
    fn block_members_of_every_shape() {
        let p = link(
            "uniform mat2 r; uniform vec3 v3; uniform float fa[3]; uniform mat3x2 m32; attribute vec4 v;\n\
             void main() { gl_Position = v + vec4(r * vec2(fa[0], fa[2]), v3.x, (m32 * vec3(1.0)).x); }",
            "void main() { gl_FragColor = vec4(1.0); }",
            &HashMap::new(),
        )
        .unwrap();
        assert!(p.ps.block.is_none());
        let b = p.vs.block.as_ref().unwrap();
        let m = |n: &str| b.members.iter().find(|m| m.name == n).unwrap_or_else(|| panic!("{n}"));
        assert_eq!((m("r").cols, m("r").rows, m("r").count), (2, 2, None));
        assert_eq!((m("v3").cols, m("v3").rows), (1, 3));
        assert_eq!((m("fa").cols, m("fa").rows, m("fa").count, m("fa").stride), (1, 1, Some(3), 16));
        assert_eq!((m("m32").cols, m("m32").rows), (3, 2));
        assert_eq!((p.sizes["r"], p.sizes["v3"], p.sizes["fa"], p.sizes["m32"]), (4, 3, 1, 6));
        assert_eq!(p.attribs, [("v".to_string(), 0)]);
    }

    #[test]
    fn put_packs_std140() {
        let rd = |out: &[u8], o: u32| u32::from_le_bytes(out[o as usize..o as usize + 4].try_into().unwrap());
        // 2-row matrix: columns 16 bytes apart
        let m = Member { name: "m".into(), offset: 8, cols: 2, rows: 2, count: None, stride: 0 };
        let mut out = vec![0u8; 64];
        m.put(&mut out, &[1, 2, 3, 4]);
        assert_eq!([rd(&out, 8), rd(&out, 12), rd(&out, 24), rd(&out, 28)], [1, 2, 3, 4]);
        assert_eq!(rd(&out, 16), 0);
        // vector: packed
        let v = Member { name: "v".into(), offset: 4, cols: 1, rows: 3, count: None, stride: 0 };
        let mut out = vec![0u8; 32];
        v.put(&mut out, &[7, 8, 9]);
        assert_eq!([rd(&out, 4), rd(&out, 8), rd(&out, 12)], [7, 8, 9]);
        // array of mat3x2, short data, write past the end ignored
        let a = Member { name: "a".into(), offset: 0, cols: 3, rows: 2, count: Some(2), stride: 48 };
        let mut out = vec![0u8; 80];
        a.put(&mut out, &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!([rd(&out, 0), rd(&out, 4), rd(&out, 16), rd(&out, 20), rd(&out, 32), rd(&out, 36)], [1, 2, 3, 4, 5, 6]);
        assert_eq!([rd(&out, 48), rd(&out, 52), rd(&out, 64), rd(&out, 68)], [7, 8, 9, 0]);
        assert_eq!(a.words(), 6);
    }
}
