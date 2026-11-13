//! msvcp140 / msvcp120 for 32-bit programs (#150): the x64 handlers registered under their x86
//! names, `this` in ECX as argument 0, 64-bit arguments in two slots.
use proptest::prelude::*;

use crate::api::{self, Ctx};
use crate::mem;
use crate::msvcp::x86_names;
use crate::test_guest;

/// (x64 name, x86 name) of one function, from Wine's msvcp140.spec / msvcp120.spec (the lines of
/// one implementation under -arch=win64 and -arch=i386/win32)
const PAIRS: &str = include_str!("../data/msvcp_x86_names.txt");
/// "dll name" of what Dominating eyes (RJ394990: player.exe, libcocos2d.dll, mozjs-33.dll)
/// imports from msvcp140 / msvcp120
const IMPORTS: &str = include_str!("../data/msvcp_x86_imports.txt");

#[test]
fn x86_names_by_hand() {
    let one = |n: &str| x86_names(n);
    // member: <access>E<cv>A -> <access><cv>E; PEA/AEB lose their E
    assert!(one("?widen@?$basic_ios@DU?$char_traits@D@std@@@std@@QEBADD@Z").contains(&"?widen@?$basic_ios@DU?$char_traits@D@std@@@std@@QBEDD@Z".to_string()));
    assert!(one("?_Ipfx@?$basic_istream@DU?$char_traits@D@std@@@std@@QEAA_N_N@Z").contains(&"?_Ipfx@?$basic_istream@DU?$char_traits@D@std@@@std@@QAE_N_N@Z".to_string()));
    // a global function: only the pointers change
    assert_eq!(one("?_Xinvalid_argument@std@@YAXPEBD@Z"), vec!["?_Xinvalid_argument@std@@YAXPBD@Z".to_string()]);
    // a variable: the trailing @EA
    assert_eq!(one("?_Ptr_cerr@std@@3PEAV?$basic_ostream@DU?$char_traits@D@std@@@1@EA"), vec!["?_Ptr_cerr@std@@3PAV?$basic_ostream@DU?$char_traits@D@std@@@1@A".to_string()]);
    assert_eq!(one("?_Raise_handler@std@@3P6AXAEBVexception@stdext@@@ZEA"), vec!["?_Raise_handler@std@@3P6AXABVexception@stdext@@@ZA".to_string()]);
    // a __cdecl member (num_put's helpers): `this` on the stack, after the thiscall spelling
    let v = one("?_Ifmt@?$num_put@DV?$ostreambuf_iterator@DU?$char_traits@D@std@@@std@@@std@@AEBAPEADPEADPEBDH@Z");
    assert_eq!(v, vec![
        "?_Ifmt@?$num_put@DV?$ostreambuf_iterator@DU?$char_traits@D@std@@@std@@@std@@ABEPADPADPBDH@Z".to_string(),
        "?_Ifmt@?$num_put@DV?$ostreambuf_iterator@DU?$char_traits@D@std@@@std@@@std@@ABAPADPADPBDH@Z".to_string(),
    ]);
    // size_t: `I` on x86, but an unsigned __int64 stays `_K` (kept first)
    let v = one("??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV01@_K@Z");
    assert_eq!(v[0], "??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@_K@Z");
    assert!(v.contains(&"??6?$basic_ostream@DU?$char_traits@D@std@@@std@@QAEAAV01@I@Z".to_string()));
    // C names are themselves
    assert_eq!(one("_Mtx_lock"), vec!["_Mtx_lock".to_string()]);
}

/// What x86_names can't derive (see its comment): a `_K` back reference renumbered on x86, a
/// callback that is __stdcall there. None of them is implemented.
fn underivable(x64: &str) -> bool {
    x64.contains("_Concurrent_vector_base_v4@") || x64.starts_with("?_Execute_once@") || x64.starts_with("?_Fput@?$num_put@")
}

/// Differential against Wine's spec files: every x64 export's x86 spelling is a candidate.
#[test]
fn x86_names_match_wine_specs() {
    let mut bad = vec![];
    let mut n = 0;
    for l in PAIRS.lines() {
        let Some((x64, x86)) = l.split_once(' ') else { continue };
        if underivable(x64) {
            continue;
        }
        n += 1;
        if !x86_names(x64).iter().any(|c| c == x86) {
            bad.push(format!("{x64} -> {x86} (got {:?})", x86_names(x64)));
        }
    }
    assert!(n > 700, "{n} pairs");
    assert!(bad.is_empty(), "{} of {n}:\n{}", bad.len(), bad.join("\n"));
}

/// Every msvcp140 / msvcp120 import of the 32-bit target game has a handler and a spec line
/// (stack bytes, thiscall), so nothing falls to the unimplemented-call path.
#[test]
fn game_imports_implemented_on_x86() {
    let _g = test_guest::enter();
    crate::msvcp::init();
    let mut missing = vec![];
    let mut n = 0;
    for l in IMPORTS.lines() {
        let Some((dll, name)) = l.split_once(' ') else { continue };
        n += 1;
        let data = !name.ends_with('Z') && crate::msvcp::data(name).is_some();
        if !data && (api::find_impl(dll, name).is_none() || !api::exports(dll, name)) {
            missing.push(format!("{dll} {name}"));
        }
    }
    assert!(n > 150, "{n} imports");
    assert!(missing.is_empty(), "{} of {n}:\n{}", missing.len(), missing.join("\n"));
}

/// Every handler registered by its x64 name found its x86 export (a miss is an x86 program's
/// call that would go unimplemented), but the x64-only ones.
#[test]
fn every_handler_has_an_x86_name() {
    let _g = test_guest::enter();
    crate::msvcp::init();
    let mut m: Vec<String> = crate::msvcp::MISSED.lock().unwrap().clone();
    m.sort();
    m.dedup();
    assert!(m.is_empty(), "{}:\n{}", m.len(), m.join("\n"));
}

/// The x64 names are never registered on x86 (they would be dead entries hiding a miss).
#[test]
fn no_x64_names_on_x86() {
    let _g = test_guest::enter();
    crate::msvcp::init();
    for l in PAIRS.lines() {
        let Some((x64, x86)) = l.split_once(' ') else { continue };
        if x64 != x86 {
            assert!(!api::exports("msvcp140", x64), "{x64}");
        }
    }
}

fn frame(this: Option<u32>, slots: &[u32]) -> u32 {
    // the return address, then the slots
    let sp = mem::heap_alloc(4 + 4 * slots.len() as u32 + 4, true);
    for (i, &v) in slots.iter().enumerate() {
        mem::w32(sp + 4 + 4 * i as u32, v);
    }
    let _ = this;
    sp
}

proptest! {
    /// thiscall: argument 0 is ECX, argument k > 0 the stack slot k - 1; q/d/byval follow
    #[test]
    fn thiscall_args(this in any::<u32>(), slots in proptest::collection::vec(any::<u32>(), 4..8)) {
        let _g = test_guest::enter();
        let sp = frame(Some(this), &slots);
        let mut cpu = koji::Cpu::default();
        let c = Ctx { cpu: &mut cpu, esp: sp, edx: None, jumped: false, st0: None, trap: 0, this: Some(this) };
        prop_assert_eq!(c.a(0), this);
        for k in 1..=slots.len() as u32 {
            prop_assert_eq!(c.a(k), slots[k as usize - 1]);
        }
        prop_assert_eq!(c.q(1), slots[0] as u64 | (slots[1] as u64) << 32);
        prop_assert_eq!(c.z(2), slots[1] as u64);
        prop_assert_eq!(c.d(2).to_bits(), slots[1] as u64 | (slots[2] as u64) << 32);
        prop_assert_eq!(c.fl(3).to_bits(), slots[2]);
        prop_assert_eq!(mem::r32(c.byval(1)), slots[0]);
        prop_assert_eq!(c.byval(2), c.byval(1) + 4);
        mem::heap_free(sp);
    }

    /// cdecl (no `this`): argument k is slot k
    #[test]
    fn cdecl_args(slots in proptest::collection::vec(any::<u32>(), 3..6)) {
        let _g = test_guest::enter();
        let sp = frame(None, &slots);
        let mut cpu = koji::Cpu::default();
        let c = Ctx { cpu: &mut cpu, esp: sp, edx: None, jumped: false, st0: None, trap: 0, this: None };
        for k in 0..slots.len() as u32 {
            prop_assert_eq!(c.a(k), slots[k as usize]);
        }
        prop_assert_eq!(c.q(0), slots[0] as u64 | (slots[1] as u64) << 32);
        prop_assert_eq!(mem::r32(c.byval(0)), slots[0]);
        mem::heap_free(sp);
    }

    /// ios_base::width(streamsize) on x86: `this` in ECX, the __int64 in two slots, the old
    /// width back in EDX:EAX, _Wide at 32 (MS STL's i686 layout)
    #[test]
    fn ios_width_x86(old in any::<u64>(), new in any::<u64>()) {
        let _g = test_guest::enter();
        crate::msvcp::init();
        let set = api::find_impl("msvcp140", "?width@ios_base@std@@QAE_J_J@Z").unwrap();
        let get = api::find_impl("msvcp140", "?width@ios_base@std@@QBE_JXZ").unwrap();
        let ios = mem::heap_alloc(56, true);
        mem::w64(ios + 32, old);
        let sp = frame(Some(ios), &[new as u32, (new >> 32) as u32]);
        let mut cpu = koji::Cpu::default();
        let mut c = Ctx { cpu: &mut cpu, esp: sp, edx: None, jumped: false, st0: None, trap: 0, this: Some(ios) };
        let lo = set(&mut c);
        prop_assert_eq!(lo as u64 | (c.edx.unwrap() as u64) << 32, old);
        prop_assert_eq!(mem::r64(ios + 32), new);
        // nothing else of the ios_base written
        prop_assert_eq!(mem::slice(ios, 32).iter().all(|&b| b == 0), true);
        prop_assert_eq!(mem::slice(ios + 40, 16).iter().all(|&b| b == 0), true);
        let mut c = Ctx { cpu: &mut cpu, esp: sp, edx: None, jumped: false, st0: None, trap: 0, this: Some(ios) };
        let lo = get(&mut c);
        prop_assert_eq!(lo as u64 | (c.edx.unwrap() as u64) << 32, new);
        mem::heap_free(sp);
        mem::heap_free(ios);
    }
}

/// The spec's thiscall functions of msvcp140 / msvcp120 get `this` from ECX (Entry::this_arg),
/// their stack bytes without it; cdecl ones don't.
#[test]
fn thiscall_entries() {
    let _g = test_guest::enter();
    for (dll, name, want) in [
        ("msvcp140", "?width@ios_base@std@@QAE_J_J@Z", (false, 8)),
        ("msvcp140", "?widen@?$basic_ios@DU?$char_traits@D@std@@@std@@QBEDD@Z", (false, 4)),
        ("msvcp140", "?flags@ios_base@std@@QBEHXZ", (false, 0)),
        ("msvcp120", "?_Getcat@?$ctype@D@std@@SAIPAPBVfacet@locale@2@PBV42@@Z", (true, 8)),
    ] {
        assert_eq!(api::args_of(api::export(dll, name)), want, "{name}");
    }
}
