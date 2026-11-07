//! `IPPAI_API_COUNTS`: how often a run called each entry point: Direct3D 8/9/10/11 and DXGI
//! methods by their call id, every sake export and COM method by name, and the GL functions a
//! game asked for that kura does not have. Printed to stderr as one line,
//! `api-counts <key>=<n> ...`, every time the total doubles (from 4096), every 2^20 calls and
//! when the process exits, so the last such line in a log is the run's tally. tests/coverage/usage.py sets the corpus' tallies beside
//! what the unit tests call (the same counter; at exit also written to the file
//! `IPPAI_API_COUNTS_OUT` names): the entry points games use that no test reaches.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// (by call id: the hot path is one atomic add)
static D3D: [AtomicU64; 1 << 16] = [const { AtomicU64::new(0) }; 1 << 16];
static NAMED: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static TOTAL: AtomicU64 = AtomicU64::new(0);
/// more tallies (sake's traps, counted by index there), read when a line is printed
static MORE: OnceLock<fn() -> Vec<(String, u64)>> = OnceLock::new();

pub fn on() -> bool {
    crate::env_on!("IPPAI_API_COUNTS")
}

/// A Direct3D / DXGI call id (key `d3d9:<iface>::<method>`, else `d3d:<iface>.<method>`).
pub fn d3d(id: u32) {
    if on() {
        // (the entry calls 0xffff_00xx: interface 255)
        let i = if id >> 16 == 0xffff { 0xff00 | id & 0xff } else { id & 0xffff };
        D3D[i as usize].fetch_add(1, Relaxed);
        tick();
    }
}

/// A rare entry point by name (key `<kind>:<name>`), e.g. `glmissing:<name>`, the GL
/// functions a game asked for that kura does not have.
pub fn named(kind: &str, name: &str) {
    if on() {
        *NAMED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(format!("{kind}:{name}")).or_insert(0) += 1;
        tick();
    }
}

/// The tallies `f` gives join every line (sake: `api:<dll>!<name>` for each export and COM
/// method, `stub:` for those it does not implement); its counting calls `tick`.
pub fn more(f: fn() -> Vec<(String, u64)>) {
    let _ = MORE.set(f);
}

/// One more call counted.
pub fn tick() {
    static AT_EXIT: std::sync::Once = std::sync::Once::new();
    AT_EXIT.call_once(|| {
        extern "C" fn at_exit() {
            let l = line();
            eprintln!("{l}");
            if let Some(p) = std::env::var_os("IPPAI_API_COUNTS_OUT") {
                let _ = std::fs::write(p, l + "\n");
            }
        }
        extern "C" {
            fn atexit(f: extern "C" fn()) -> i32;
        }
        unsafe { atexit(at_exit) };
    });
    let n = TOTAL.fetch_add(1, Relaxed) + 1;
    if n >= 4096 && n.is_power_of_two() || n % (1 << 20) == 0 {
        eprintln!("{}", line());
    }
}

/// `api-counts k=n ...`, keys sorted.
pub fn line() -> String {
    let mut v: Vec<(String, u64)> = D3D.iter().enumerate().filter_map(|(i, n)| {
        let n = n.load(Relaxed);
        // (Direct3D 9's by name, the rest by interface and method number)
        let named = if i >> 8 < 64 { crate::d3d9::name((i >> 8) as u32, (i & 0xff) as u32) } else { String::new() };
        let k = if named.contains('?') || named.contains('#') || named.is_empty() { format!("d3d:{}.{}", i >> 8, i & 0xff) } else { format!("d3d9:{named}") };
        (n > 0).then_some((k, n))
    }).collect();
    v.extend(NAMED.lock().unwrap_or_else(|e| e.into_inner()).iter().flatten().map(|(k, n)| (k.clone(), *n)));
    if let Some(f) = MORE.get() {
        v.extend(f());
    }
    v.sort();
    let mut s = String::from("api-counts");
    for (k, n) in v {
        s += &format!(" {k}={n}");
    }
    s
}
