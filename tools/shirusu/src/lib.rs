//! sake's SAKE_SHIRUSU=1 lines (crates/sake/src/shirusu.rs) into a shirusu Trace: each API's
//! Schemas are the catalog's (shirusu-api-catalog), and its values are made by the same capture
//! functions shirusu's Windows hooks use (shirusu-hook's capture module, over a memory holding
//! just what sake read), so a sake Trace and a Windows capture compare field for field.
//!
//! The lines may sit anywhere in a log line (a page's console.log puts a time and a level before
//! them); other lines are skipped. A call with no leave line (the thread or the program ended in
//! it, or sake's handler jumped) is written down as a Loss Record of an unknown termination,
//! without its enter: shirusu-conformance fails a Trace with an enter left open, and
//! shirusu-emit has no aborted event to close one with (abandon_call writes nothing).

use shirusu_api_catalog::api::{api_by_import, ApiDef};
use shirusu_emit::{ApiSet, ApiSpec, CallToken, Emitter, EventMeta, SessionConfig};
use shirusu_hook::capture::{self, MapMemory};
use shirusu_trace::compact::decode_values;
use shirusu_trace::proto::{loss_kind, LossRecord};
use shirusu_trace::schema::{CapturedValue, Ty};
use shirusu_trace::writer::FinalizationInfo;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// "x86" or "x86_64"
    pub arch: String,
    pub created_by: String,
    pub target: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub calls: u64,
    pub unfinished: u64,
    /// lines that looked like sake's but did not parse, and leave lines of no open call
    pub bad: u64,
    /// calls per API
    pub apis: BTreeMap<String, u64>,
}

/// One parsed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Enter { tid: u64, call: u64, ms: u64, api: String, args: Vec<u64>, s: Option<Vec<u8>>, d: Option<Vec<u8>> },
    Leave { tid: u64, call: u64, ms: u64, ret: u64, err: u32, o: Option<u32>, d: Option<Vec<u8>> },
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn hexnum(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 16).ok()
}

/// The sake line in `text`, if it has one; Some(None) for one that does not parse.
pub fn parse(text: &str) -> Option<Option<Line>> {
    let at = text.find("shirusu E ").or_else(|| text.find("shirusu L "))?;
    let mut w = text[at + 8..].split_ascii_whitespace();
    let kind = w.next()?;
    Some((|| {
        let tid = hexnum(w.next()?)?;
        let call = hexnum(w.next()?)?;
        let ms = w.next()?.parse().ok()?;
        if kind == "E" {
            let api = w.next()?.to_string();
            let list = w.next()?;
            let args = if list == "-" { Vec::new() } else { list.split(',').map(hexnum).collect::<Option<Vec<_>>>()? };
            let (mut s, mut d) = (None, None);
            for x in w {
                match x.split_once('=')? {
                    ("s", v) => s = Some(unhex(v)?),
                    ("d", v) => d = Some(unhex(v)?),
                    _ => return None,
                }
            }
            Some(Line::Enter { tid, call, ms, api, args, s, d })
        } else {
            let ret = hexnum(w.next()?)?;
            let err = u32::try_from(hexnum(w.next()?)?).ok()?;
            let (mut o, mut d) = (None, None);
            for x in w {
                match x.split_once('=')? {
                    ("o", v) => o = Some(u32::try_from(hexnum(v)?).ok()?),
                    ("d", v) => d = Some(unhex(v)?),
                    _ => return None,
                }
            }
            Some(Line::Leave { tid, call, ms, ret, err, o, d })
        }
    })())
}

/// The catalog's definition of `api` ("kernel32!CreateFileW").
pub fn def_of(api: &str) -> Option<&'static ApiDef> {
    let (dll, f) = api.split_once('!')?;
    api_by_import(&format!("{dll}.dll"), f)
}

fn spec_of(def: &ApiDef) -> ApiSpec {
    ApiSpec::new(&def.schema_base_name(), def.enter_fields, def.leave_fields).with_last_error_meaning(def.last_error_meaning)
}

/// Addresses the converter's memory puts what sake read at (the values' own pointers stay
/// in the Trace; the capture functions only look this memory up).
fn mem_with(at: &[(u64, &[u8])]) -> MapMemory {
    let mut m = MapMemory::new();
    for (a, b) in at {
        if *a != 0 {
            m.write(*a, b);
        }
    }
    m
}

/// A pointer argument as the Windows hook sees it on this width (x86: the slot's 32 bits).
fn ptr(v: u64, x86: bool) -> u64 {
    if x86 { v as u32 as u64 } else { v }
}

/// The enter values of a call: the handwritten file APIs through shirusu-hook's own capture
/// functions, the rest as generated L1 scalars.
pub fn enter_values(def: &ApiDef, args: &[u64], s: Option<&[u8]>, d: Option<&[u8]>, x86: bool) -> Vec<CapturedValue> {
    let a = |k: usize| ptr(args.get(k).copied().unwrap_or(0), x86);
    match (def.module, def.function) {
        ("kernel32.dll", "CreateFileW") => {
            let m = mem_with(&[(a(0), s.unwrap_or(&[]))]);
            capture::create_file_w_enter(&m, a(0), a(1) as u32, a(2) as u32, a(3), a(4) as u32, a(5) as u32, a(6))
        }
        ("kernel32.dll", "ReadFile") => capture::read_file_enter(a(0), a(1), a(2) as u32, a(4)),
        ("kernel32.dll", "WriteFile") => {
            let m = mem_with(&[(a(1), d.unwrap_or(&[]))]);
            capture::write_file_enter(&m, a(0), a(1), a(2) as u32, a(4))
        }
        ("kernel32.dll", "CloseHandle") => capture::close_handle_enter(a(0)),
        _ => {
            let raw: Vec<u64> = (0..def.enter_fields.len()).map(a).collect();
            let p = capture::encode_generic_enter::<16>(def, &raw);
            let types: Vec<Ty> = def.enter_fields.iter().map(|f| f.1).collect();
            decode_values(&types, p.as_bytes()).expect("generic enter decodes")
        }
    }
}

/// The leave values: `ret` the call's result (x86 EAX zero-extended, x64 RAX), `err` the last
/// error after it, `o` the count sake read at the call's count pointer, `d` ReadFile's buffer.
pub fn leave_values(def: &ApiDef, args: &[u64], ret: u64, err: u32, o: Option<u32>, d: Option<&[u8]>, x86: bool) -> Vec<CapturedValue> {
    let a = |k: usize| ptr(args.get(k).copied().unwrap_or(0), x86);
    let count = |at: u64| mem_with(&[(at, &o.map(u32::to_le_bytes).unwrap_or_default()[..o.map_or(0, |_| 4)])]);
    match (def.module, def.function) {
        ("kernel32.dll", "CreateFileW") => capture::create_file_w_leave(ptr(ret, x86), err, a(4) as u32),
        ("kernel32.dll", "ReadFile") => {
            let mut m = count(a(3));
            if let (Some(b), true) = (d, a(1) != 0) {
                m.write(a(1), b);
            }
            capture::read_file_leave(&m, ret as u32 as i32, a(1), a(2) as u32, a(3), a(4) != 0, err)
        }
        ("kernel32.dll", "WriteFile") => capture::write_file_leave(&count(a(3)), ret as u32 as i32, a(3), a(4) != 0, err),
        ("kernel32.dll", "CloseHandle") => capture::close_handle_leave(ret as u32 as i32, err),
        _ => {
            // (the hook passes a 32-bit result as its own type's bits: a BOOL sign-extended)
            let r = match def.leave_fields.iter().find(|f| f.0 == "return").map(|f| f.1) {
                Some(Ty::I32) => ret as u32 as i32 as i64 as u64,
                Some(Ty::U32) => ret as u32 as u64,
                _ => ptr(ret, x86),
            };
            let raw: Vec<u64> = (0..def.enter_fields.len()).map(a).collect();
            let p = capture::encode_generic_leave_with_inputs(def, r, err, &raw);
            let types: Vec<Ty> = def.leave_fields.iter().map(|f| f.1).collect();
            decode_values(&types, p.as_bytes()).expect("generic leave decodes")
        }
    }
}

struct OpenCall {
    token: CallToken,
    def: &'static ApiDef,
    args: Vec<u64>,
}

/// The whole log into a Trace (its bytes) and what was in it.
pub fn convert(log: &str, opt: &Options) -> Result<(Vec<u8>, Stats), String> {
    let x86 = opt.arch == "x86";
    let mut stats = Stats::default();
    let lines: Vec<Line> = log
        .lines()
        .filter_map(parse)
        .filter_map(|l| {
            if l.is_none() {
                stats.bad += 1;
            }
            l
        })
        .collect();
    // (the API set: the catalog's APIs the log has, in the catalog's order)
    let mut defs: Vec<&'static ApiDef> = Vec::new();
    for l in &lines {
        if let Line::Enter { api, .. } = l {
            let def = def_of(api).ok_or_else(|| format!("{api}: not in shirusu's catalog"))?;
            if !defs.iter().any(|d| d.api_id == def.api_id) {
                defs.push(def);
            }
        }
    }
    defs.sort_by_key(|d| d.api_id);
    let mut set = ApiSet::new();
    for d in &defs {
        set.add(spec_of(d)).map_err(|e| format!("{e:?}"))?;
    }
    let mut cfg = SessionConfig::new(1, &opt.created_by);
    cfg.cpu_arch = opt.arch.clone();
    cfg.pointer_width = if x86 { 32 } else { 64 };
    // (sake's ms clock as the Trace's timestamps)
    cfg.qpc_frequency = 1000;
    cfg.os_version = "sake".into();
    cfg.capture_config = "sake SAKE_SHIRUSU=1; mode=filtered (tools/shirusu/apis.txt); calls from guest code only".into();
    cfg.target_description = opt.target.clone();
    let mut em = Emitter::new(Vec::new(), set, cfg).map_err(|e| format!("{e:?}"))?;
    let mut open: HashMap<(u64, u64), OpenCall> = HashMap::new();
    let left: std::collections::HashSet<(u64, u64)> = lines.iter().filter_map(|l| match l { Line::Leave { tid, call, .. } => Some((*tid, *call)), _ => None }).collect();
    let mut losses = Vec::new();
    for l in lines {
        match l {
            Line::Enter { tid, call, ms, api, args, s, d } => {
                let def = def_of(&api).expect("in the set");
                *stats.apis.entry(api.clone()).or_default() += 1;
                stats.calls += 1;
                if !left.contains(&(tid, call)) {
                    losses.push(LossRecord {
                        kind: loss_kind::UNKNOWN_TERMINATION,
                        count: 1,
                        thread_id: tid,
                        process_id: 1,
                        reason: format!("{api} at {ms} ms: no leave line (the call never returned to the guest)"),
                        ..Default::default()
                    });
                    stats.unfinished += 1;
                    continue;
                }
                let values = enter_values(def, &args, s.as_deref(), d.as_deref(), x86);
                let token = em.begin_call(tid, &def.schema_base_name(), &values, ms, EventMeta::default()).map_err(|e| format!("{api}: {e:?}"))?;
                open.insert((tid, call), OpenCall { token, def, args });
            }
            Line::Leave { tid, call, ms, ret, err, o, d } => {
                let Some(c) = open.remove(&(tid, call)) else {
                    stats.bad += 1;
                    continue;
                };
                let values = leave_values(c.def, &c.args, ret, err, o, d.as_deref(), x86);
                em.end_call(c.token, &values, ms, EventMeta::default()).map_err(|e| format!("{}: {e:?}", c.def.function))?;
            }
        }
    }
    if !losses.is_empty() {
        em.write_loss_records(&losses).map_err(|e| format!("{e:?}"))?;
    }
    let degraded = if losses.is_empty() && stats.bad == 0 { 0 } else { 1 };
    let out = em
        .finish(FinalizationInfo {
            // (calls between sake's own DLLs never reach a trap: what a Windows DLL calls of
            // another is not seen, as shirusu's standard boundary does not record it either)
            capture_coverage: 1,
            event_integrity: degraded,
            semantic_completeness: 1,
            notes: vec![format!("sake-shirusu: {} calls, {} unfinished, {} bad lines", stats.calls, stats.unfinished, stats.bad)],
        })
        .map_err(|e| format!("{e:?}"))?;
    Ok((out, stats))
}
