//! GLSL programs (OpenGL 2.x, GLSL 1.10-1.30 with the compatibility built-ins): the glwgsl crate
//! makes WGSL of the sources (built-ins become `vn_*` inputs and uniforms, filled here). Uniform
//! values are kept by name and laid out into each stage's block at every draw.

use super::*;

pub(crate) struct Shader {
    pixel: bool,
    source: String,
    ok: bool,
    log: String,
    deleted: bool,
}

#[derive(Default)]
pub(crate) struct Program {
    /// glDeleteProgram while current somewhere: goes when no context uses it
    deleted: bool,
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
    block: Option<glwgsl::Block>,
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
    /// uniform name -> elements it has (1 for one that is not an array)
    counts: HashMap<String, u32>,
}

/// Generic vertex attributes (GL_MAX_VERTEX_ATTRIBS)
const MAX_ATTRIBS: u32 = 16;

const GL_VERTEX_SHADER: u32 = 0x8B31;

fn link(g: &mut Gl, p: &mut Program) -> Result<Linked, String> {
    // (one compiled shader of each stage: several of one stage would need their sources linked
    // together, which glwgsl does not do; ours each have a main)
    let find = |pixel: bool| -> Result<u32, String> {
        let v: Vec<u32> = p.shaders.iter().copied().filter(|s| g.shaders.get(s).is_some_and(|x| x.pixel == pixel)).collect();
        let what = if pixel { "fragment" } else { "vertex" };
        match v[..] {
            [s] if g.shaders[&s].ok => Ok(s),
            [_] => Err(format!("the {what} shader is not compiled")),
            [] => Err(format!("no {what} shader")),
            _ => Err(format!("several {what} shaders")),
        }
    };
    let (vs, ps) = (find(false)?, find(true)?);
    let l = glwgsl::link(&g.shaders[&vs].source, &g.shaders[&ps].source, &p.bound_attribs)?;
    let mut counts: HashMap<String, u32> = l.sizes.keys().map(|k| (k.clone(), 1)).collect();
    for b in [&l.vs.block, &l.ps.block].into_iter().flatten() {
        for m in &b.members {
            counts.insert(m.name.clone(), m.count.unwrap_or(1).max(1));
        }
    }
    Ok(Linked {
        counts,
        vs: Stage { shader: shader(&l.vs.wgsl), block: l.vs.block, samplers: l.vs.samplers },
        ps: Stage { shader: shader(&l.ps.wgsl), block: l.ps.block, samplers: l.ps.samplers },
        attribs: l.attribs,
        sizes: l.sizes,
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

/// Sets the current context's error (call with the lock not held).
fn fail(e: u32) {
    with(|_, c| c.err(e))
}

/// What a name is (formal/tla/GlPrograms.tla): GL_INVALID_VALUE for no object,
/// GL_INVALID_OPERATION for an object of the other kind.
fn kind_err(g: &Gl, name: u32, want_shader: bool) -> Option<u32> {
    let (is_s, is_p) = (g.shaders.contains_key(&name), g.programs.contains_key(&name));
    if want_shader && is_s || !want_shader && is_p {
        None
    } else if is_s || is_p {
        Some(GL_INVALID_OPERATION)
    } else {
        Some(GL_INVALID_VALUE)
    }
}

impl Gl {
    /// Shaders flagged for deletion that no program has any more go; programs flagged that no
    /// context uses go, and so do their flagged shaders then.
    pub(crate) fn collect_programs(&mut self) {
        // (the context `with` runs: out of the table, its program in the hint)
        let used: HashSet<u32> = self.ctxs.values().map(|c| c.program).chain(self.ctxs_program_hint).collect();
        let gone: Vec<u32> = self.programs.iter().filter(|(id, p)| p.deleted && !used.contains(id)).map(|(id, _)| *id).collect();
        for id in gone {
            if let Some(l) = self.programs.remove(&id).and_then(|p| p.linked) {
                forget_linked(self, &l);
            }
        }
        let attached: HashSet<u32> = self.programs.values().flat_map(|p| p.shaders.iter().copied()).collect();
        self.shaders.retain(|id, s| !s.deleted || attached.contains(id));
    }
}

/// A link that is no more (the program linked again or deleted): its two page shaders and the
/// pipelines made of them go from the page (each link made two new ones, kept for the session).
fn forget_linked(g: &mut Gl, l: &Linked) {
    let (vs, ps) = (l.vs.shader, l.ps.shader);
    let mut gone = Vec::new();
    g.pipelines.retain(|k, &mut id| {
        let keep = k[0] != vs && k[1] != ps;
        if !keep {
            gone.push(id);
        }
        keep
    });
    gpu::record(|q| {
        for id in gone {
            q.cmd(&[gpu::DESTROY_PIPELINE, id]);
        }
        q.cmd(&[gpu::DESTROY_SHADER, vs]);
        q.cmd(&[gpu::DESTROY_SHADER, ps]);
    });
}

extern "C" fn glCreateShader(ty: u32) -> u32 {
    let mut g = gl();
    let g = g.as_mut().unwrap();
    let id = g.next_free();
    g.shaders.insert(id, Shader { pixel: ty != GL_VERTEX_SHADER, source: String::new(), ok: false, log: String::new(), deleted: false });
    id
}
extern "C" fn glCreateShaderObject(ty: u32) -> u32 {
    glCreateShader(ty)
}
unsafe extern "C" fn glShaderSource(sh: u32, count: i32, strings: *const *const c_char, lengths: *const i32) {
    // (a statement of its own: the lock must be let go before `fail` takes it)
    let e = kind_err(gl().as_ref().unwrap(), sh, true).or((count < 0).then_some(GL_INVALID_VALUE));
    if let Some(e) = e {
        return fail(e);
    }
    let mut s = String::new();
    // (a 64-bit guest's array holds 8-byte pointers: their low halves are the addresses)
    let stride = crate::d3d9::device::ptr() as usize / 4;
    for k in 0..count.max(0) as usize {
        let p = *(strings as *const u32).add(k * stride) as usize as *const c_char;
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
    if let Some(e) = kind_err(g.as_ref().unwrap(), sh, true) {
        drop(g);
        return fail(e);
    }
    let Some(x) = g.as_mut().unwrap().shaders.get_mut(&sh) else { return };
    match glwgsl::check(&x.source, x.pixel) {
        Ok(()) => {
            x.ok = true;
            x.log.clear();
        }
        Err((e, src)) => {
            x.ok = false;
            x.log = e;
            eprintln!("gl: shader {sh} does not compile: {}\n{src}", x.log);
        }
    }
}
unsafe extern "C" fn glGetShaderiv(sh: u32, p: u32, out: *mut i32) {
    let g = gl();
    if let Some(e) = kind_err(g.as_ref().unwrap(), sh, true) {
        drop(g);
        return fail(e);
    }
    let x = &g.as_ref().unwrap().shaders[&sh];
    *out = match p {
        0x8B4F => if x.pixel { 0x8B30 } else { 0x8B31 },
        // (GL_OBJECT_TYPE_ARB: GL_SHADER_OBJECT_ARB)
        0x8B4E => 0x8B48,
        0x8B80 => x.deleted as i32,
        0x8B81 => x.ok as i32,
        0x8B84 => x.log.len() as i32 + 1,
        0x8B88 => x.source.len() as i32 + 1,
        _ => {
            drop(g);
            return fail(GL_INVALID_ENUM);
        }
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
/// (a shader attached to a program is only flagged: it goes when no program has it)
extern "C" fn glDeleteShader(sh: u32) {
    if sh == 0 {
        return;
    }
    let mut g = gl();
    let g2 = g.as_mut().unwrap();
    if let Some(e) = kind_err(g2, sh, true) {
        drop(g);
        return fail(e);
    }
    g2.shaders.get_mut(&sh).unwrap().deleted = true;
    g2.collect_programs();
}
extern "C" fn glIsShader(sh: u32) -> u8 {
    gl().as_ref().unwrap().shaders.contains_key(&sh) as u8
}
extern "C" fn glCreateProgram() -> u32 {
    let mut g = gl();
    let g = g.as_mut().unwrap();
    let id = g.next_free();
    g.programs.insert(id, Program::default());
    id
}
extern "C" fn glCreateProgramObject() -> u32 {
    glCreateProgram()
}
extern "C" fn glAttachShader(p: u32, sh: u32) {
    let mut g = gl();
    let g2 = g.as_mut().unwrap();
    let e = kind_err(g2, p, false).or(kind_err(g2, sh, true));
    let e = e.or_else(|| g2.programs[&p].shaders.contains(&sh).then_some(GL_INVALID_OPERATION));
    if let Some(e) = e {
        drop(g);
        return fail(e);
    }
    g2.programs.get_mut(&p).unwrap().shaders.push(sh);
}
extern "C" fn glAttachObject(p: u32, sh: u32) {
    glAttachShader(p, sh)
}
extern "C" fn glDetachShader(p: u32, sh: u32) {
    let mut g = gl();
    let g2 = g.as_mut().unwrap();
    let e = kind_err(g2, p, false).or(kind_err(g2, sh, true));
    let e = e.or_else(|| (!g2.programs[&p].shaders.contains(&sh)).then_some(GL_INVALID_OPERATION));
    if let Some(e) = e {
        drop(g);
        return fail(e);
    }
    g2.programs.get_mut(&p).unwrap().shaders.retain(|&s| s != sh);
    g2.collect_programs();
}
unsafe extern "C" fn glGetAttachedShaders(p: u32, max: i32, count: *mut i32, out: *mut u32) {
    let e = kind_err(gl().as_ref().unwrap(), p, false).or((max < 0).then_some(GL_INVALID_VALUE));
    if let Some(e) = e {
        return fail(e);
    }
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
    // (an index past the generic attributes would be an input no draw has a value for)
    if idx >= MAX_ATTRIBS {
        return fail(GL_INVALID_VALUE);
    }
    let n = CStr::from_ptr(name).to_string_lossy().into_owned();
    if let Some(x) = gl().as_mut().unwrap().programs.get_mut(&p) {
        x.bound_attribs.insert(n, idx);
    }
}
extern "C" fn glLinkProgram(p: u32) {
    let mut g = gl();
    if let Some(e) = kind_err(g.as_ref().unwrap(), p, false) {
        drop(g);
        return fail(e);
    }
    let g = g.as_mut().unwrap();
    let Some(mut x) = g.programs.remove(&p) else { return };
    let old = match link(g, &mut x) {
        Ok(l) => {
            x.log.clear();
            x.linked.replace(l)
        }
        Err(e) => {
            eprintln!("gl: program {p} does not link: {e}");
            x.log = e;
            x.linked.take()
        }
    };
    if let Some(old) = old {
        forget_linked(g, &old);
    }
    x.locations.clear();
    g.programs.insert(p, x);
}
extern "C" fn glValidateProgram(_p: u32) {}
unsafe extern "C" fn glGetProgramiv(p: u32, pname: u32, out: *mut i32) {
    let g = gl();
    if let Some(e) = kind_err(g.as_ref().unwrap(), p, false) {
        drop(g);
        return fail(e);
    }
    let x = &g.as_ref().unwrap().programs[&p];
    *out = match pname {
        0x8B80 => x.deleted as i32,
        // (GL_OBJECT_TYPE_ARB: GL_PROGRAM_OBJECT_ARB)
        0x8B4E => 0x8B40,
        0x8B82 | 0x8B83 => x.linked.is_some() as i32,
        0x8B84 => x.log.len() as i32 + 1,
        0x8B85 => x.shaders.len() as i32,
        0x8B86 => x.linked.as_ref().map_or(0, |l| l.sizes.len() as i32),
        0x8B89 => x.linked.as_ref().map_or(0, |l| l.attribs.len() as i32),
        0x8B87 | 0x8B8A => 64,
        _ => {
            drop(g);
            return fail(GL_INVALID_ENUM);
        }
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
/// (a program not linked, or no program: an error, the current one stays; leaving a program
/// flagged for deletion deletes it)
extern "C" fn glUseProgram(p: u32) {
    with(|g, c| {
        if p != 0 {
            if let Some(e) = kind_err(g, p, false).or_else(|| g.programs[&p].linked.is_none().then_some(GL_INVALID_OPERATION)) {
                return c.err(e);
            }
        }
        c.program = p;
        g.ctxs_program_hint = Some(p);
        g.collect_programs();
        g.ctxs_program_hint = None;
    })
}
extern "C" fn glUseProgramObject(p: u32) {
    glUseProgram(p)
}
/// (a program current in a context is only flagged: it goes when no context uses it)
extern "C" fn glDeleteProgram(p: u32) {
    if p == 0 {
        return;
    }
    with(|g, c| {
        if let Some(e) = kind_err(g, p, false) {
            return c.err(e);
        }
        g.programs.get_mut(&p).unwrap().deleted = true;
        g.ctxs_program_hint = Some(c.program);
        g.collect_programs();
        g.ctxs_program_hint = None;
    })
}
/// (GL_ARB_shader_objects: shaders and programs alike)
extern "C" fn glDeleteObject(o: u32) {
    let is_shader = gl().as_ref().unwrap().shaders.contains_key(&o);
    if is_shader {
        glDeleteShader(o)
    } else {
        glDeleteProgram(o)
    }
}
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
    // (an element past the array's end has no location, GL 2.1 section 2.15.3)
    if l.counts.get(&base).is_none_or(|&k| elem >= k) {
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
        let Some((name, elem)) = x.locations.get(loc as usize) else { return };
        let Some(l) = x.linked.as_ref() else { return };
        let per = l.sizes.get(name).copied().unwrap_or(n as u32) as usize;
        // (elements past the array's end are ignored)
        let count = count.min(l.counts.get(name).copied().unwrap_or(1).saturating_sub(*elem) as usize);
        let at = *elem as usize * per;
        if !x.values.contains_key(name) {
            x.values.insert(name.clone(), Vec::new());
        }
        let val = x.values.get_mut(name).unwrap();
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
    /// (the vertices and, for each, units 2-7's coordinates and the normal)
    Immediate(&'a [[f32; 16]], &'a [[f32; 28]]),
}

pub(crate) fn draw(g: &mut Gl, c: &mut Ctx, mode: u32, idx: &[u32]) {
    draw_from(g, c, mode, Src::Indices(idx))
}

/// A built-in attribute: 0 gl_Vertex, 1 gl_Color, 2 + n gl_MultiTexCoordn (n 0-7), 10 gl_Normal.
fn builtin(name: &str) -> Option<usize> {
    Some(match name {
        "vn_Vertex" => 0,
        "vn_Color" => 1,
        "vn_Normal" => 10,
        _ => match name.strip_prefix("vn_MultiTexCoord").and_then(|n| n.parse::<usize>().ok()) {
            Some(n) if n < 8 => 2 + n,
            _ => return None,
        },
    })
}

/// gl_NormalMatrix: the inverse transpose of the model-view matrix's upper 3x3 (column-major)
fn normal_matrix(mv: &[f32; 16]) -> [f32; 9] {
    let m = |c: usize, r: usize| mv[c * 4 + r] as f64;
    // (cofactors: inverse = adjugate / det; its transpose is the cofactor matrix / det)
    let cof = |c: usize, r: usize| {
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        m(c1, r1) * m(c2, r2) - m(c2, r1) * m(c1, r2)
    };
    let det = m(0, 0) * cof(0, 0) + m(1, 0) * cof(1, 0) + m(2, 0) * cof(2, 0);
    let det = if det == 0.0 { 1.0 } else { det };
    std::array::from_fn(|i| (cof(i / 3, i % 3) / det) as f32)
}

/// A built-in uniform's value (gl_ModelViewProjectionMatrix, gl_ModelViewMatrix,
/// gl_ProjectionMatrix, gl_NormalMatrix, gl_TextureMatrix[n]), column-major.
pub(crate) fn builtin_uniform(c: &Ctx, n: &str) -> Option<Vec<f32>> {
    let mv = c.modelview.last().unwrap();
    Some(match n {
        "vn_MVP" => mul(c.projection.last().unwrap(), mv).to_vec(),
        "vn_MV" => mv.to_vec(),
        "vn_P" => c.projection.last().unwrap().to_vec(),
        "vn_NM" => normal_matrix(mv).to_vec(),
        _ => match n.strip_prefix("vn_TM").and_then(|t| t.parse::<usize>().ok()) {
            Some(t) if t < 8 => c.units[t].matrices.last().unwrap().to_vec(),
            _ => return None,
        },
    })
}

pub(crate) fn draw_from(g: &mut Gl, c: &mut Ctx, mode: u32, src: Src) {
    let n = match src {
        Src::Indices(i) => i.len(),
        Src::Immediate(v, _) => v.len(),
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
                (Src::Immediate(v, _), Some(b)) if b < 4 => v[o][b * 4..b * 4 + 4].try_into().unwrap(),
                (Src::Immediate(_, more), Some(10)) => [more[o][24], more[o][25], more[o][26], 1.0],
                (Src::Immediate(_, more), Some(b)) => more[o][(b - 4) * 4..(b - 3) * 4].try_into().unwrap(),
                (Src::Immediate(..), None) => c.attrib_values.get(*loc as usize).copied().unwrap_or([0.0, 0.0, 0.0, 1.0]),
                (Src::Indices(ix), b) => {
                    let i = ix[o];
                    let arr = match b {
                        Some(0) => Some(c.vertex),
                        Some(1) => Some(c.colors),
                        Some(10) => Some(c.normals),
                        Some(t) => Some(c.units[t - 2].array),
                        _ => c.attribs.get(*loc as usize).copied(),
                    };
                    match (arr, b) {
                        (Some(a), _) if a.on => fetch(g, &a, i, [0.0, 0.0, 0.0, 1.0]),
                        (_, Some(1)) => c.color,
                        (_, Some(10)) => [c.normal[0], c.normal[1], c.normal[2], 1.0],
                        (_, Some(t)) if t >= 2 => c.units[t - 2].coords,
                        _ => c.attrib_values.get(*loc as usize).copied().unwrap_or([0.0, 0.0, 0.0, 1.0]),
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
    let builtin_value = |n: &str| builtin_uniform(c, n).map(|m| m.iter().map(|f| f.to_bits()).collect::<Vec<u32>>());
    let mut groups: Vec<(Vec<u8>, Vec<[u32; 8]>)> = Vec::new();
    for st in [&l.vs, &l.ps] {
        let mut block = Vec::new();
        if let Some(b) = &st.block {
            block = vec![0u8; (b.size as usize).max(16)];
            for m in &b.members {
                if let Some(v) = builtin_value(&m.name).or_else(|| p.values.get(&m.name).cloned()) {
                    m.put(&mut block, &v);
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
        wds.extend_from_slice(&[0, order.len() as u32, 1, 0, 0, 0, 0]);
        q.cmd(&wds);
    });
}

#[cfg(test)]
mod programs_tests;
#[cfg(test)]
mod draw_tests;
#[cfg(test)]
mod uniform_tests;
