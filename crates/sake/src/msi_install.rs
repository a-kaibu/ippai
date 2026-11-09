#![allow(non_snake_case)]
//! Installing: MsiInstallProduct, MsiDoAction, the sequence tables and the standard
//! actions (costing, folders, files from the source tree or cabinets, registry values,
//! product registration), custom actions, the UI message hooks (MsiSetInternalUI,
//! MsiSetExternalUI[Record], MsiProcessMessage) and the product queries that read the
//! registration back (MsiQueryProductState, MsiGetProductInfo, MsiEnumProducts,
//! MsiEnumRelatedProducts).
//!
//! Actions run at once in sequence order; deferred custom actions run where they are
//! scheduled, with CustomActionData set (no separate script phase, no rollback).

use std::sync::{Arc, Mutex};

use msidb::record::{Field, Record};
use msidb::*;
use msidb::transform::ERROR_INSTALL_TRANSFORM_FAILURE;

use crate::api::{implement, Ctx};
use crate::k32::arg_str;
use crate::mem::{self, r32, w32};
use crate::msi::{new_handle, out_str, rec_of, Obj};
use crate::msi_pkg::*;

pub const INSTALLMESSAGE_FATALEXIT: u32 = 0x0000_0000;
pub const INSTALLMESSAGE_ERROR: u32 = 0x0100_0000;
pub const INSTALLMESSAGE_WARNING: u32 = 0x0200_0000;
pub const INSTALLMESSAGE_USER: u32 = 0x0300_0000;
pub const INSTALLMESSAGE_INFO: u32 = 0x0400_0000;
pub const INSTALLMESSAGE_FILESINUSE: u32 = 0x0500_0000;
pub const INSTALLMESSAGE_RESOLVESOURCE: u32 = 0x0600_0000;
pub const INSTALLMESSAGE_OUTOFDISKSPACE: u32 = 0x0700_0000;
pub const INSTALLMESSAGE_ACTIONSTART: u32 = 0x0800_0000;
pub const INSTALLMESSAGE_ACTIONDATA: u32 = 0x0900_0000;
pub const INSTALLMESSAGE_PROGRESS: u32 = 0x0A00_0000;
pub const INSTALLMESSAGE_COMMONDATA: u32 = 0x0B00_0000;
pub const INSTALLMESSAGE_INITIALIZE: u32 = 0x0C00_0000;
pub const INSTALLMESSAGE_TERMINATE: u32 = 0x0D00_0000;
pub const INSTALLMESSAGE_SHOWDIALOG: u32 = 0x0E00_0000;

/// The UI the process asked for: (internal level, external handler, filter, context, wide,
/// record handler)
struct Ui {
    level: u32,
    parent: u32,
    handler: u32,
    filter: u32,
    ctx: u32,
    wide: bool,
    record: bool,
}

static UI: Mutex<Ui> = Mutex::new(Ui { parent: 0, level: INSTALLUILEVEL_DEFAULT, handler: 0, filter: 0, ctx: 0, wide: false, record: false });

pub const INSTALLUILEVEL_NOCHANGE: u32 = 0;
pub const INSTALLUILEVEL_DEFAULT: u32 = 1;
pub const INSTALLUILEVEL_NONE: u32 = 2;
pub const INSTALLUILEVEL_BASIC: u32 = 3;
pub const INSTALLUILEVEL_REDUCED: u32 = 4;
pub const INSTALLUILEVEL_FULL: u32 = 5;
/// HIDECANCEL, PROGRESSONLY, ENDDIALOG, SOURCERESONLY, UACONLY
const UI_FLAGS: u32 = 0x3e0;

/// A guest copy of `s` (ANSI or UTF-16), freed by the caller with mem::heap_free.
fn guest_str(s: &str, wide: bool) -> u32 {
    let b: Vec<u8> = if wide { s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect() } else { crate::text::s2a(s).into_iter().chain([0]).collect() };
    let p = mem::heap_alloc(b.len() as u32, false);
    mem::write(p, &b);
    p
}

/// Sends a message to the external UI handler, if it takes this kind: its answer (0: not
/// handled).
pub fn send(c: &mut Ctx, pkg: Option<&Arc<Mutex<Package>>>, kind: u32, rec: &Record) -> i32 {
    let (handler, filter, ctx, wide, record, level) = {
        let u = UI.lock().unwrap();
        (u.handler, u.filter, u.ctx, u.wide, u.record, u.level)
    };
    if crate::env_on!("SAKE_TRACE_MSI") {
        let text = match pkg {
            Some(p) => p.lock().unwrap().format_record(rec),
            None => format::format_record(rec, &format::NoPackage),
        };
        eprintln!("msi: message {:#x}: {text}", kind >> 24);
    }
    if handler != 0 && filter & (1 << (kind >> 24)) != 0 {
        let r = if record {
            let h = new_handle(Obj::Rec(rec.clone()));
            let r = c.call(handler, &[ctx, kind, h]) as i32;
            crate::msi::close(h);
            r
        } else {
            let text = match pkg {
                Some(p) => p.lock().unwrap().format_record(rec),
                None => format::format_record(rec, &format::NoPackage),
            };
            let s = guest_str(&text, wide);
            let r = c.call(handler, &[ctx, kind, s]) as i32;
            mem::heap_free(s);
            r
        };
        // (0: not handled, the internal UI's turn)
        if r != 0 {
            return r;
        }
    }
    internal_ui(kind, rec, level)
}

/// What the internal UI answers (it shows no window here): progress and action messages
/// are taken (IDOK), INITIALIZE / TERMINATE and the COMMONDATA it does not handle -1,
/// the rest 0. With no UI (INSTALLUILEVEL_NONE) everything is 0.
fn internal_ui(kind: u32, rec: &Record, level: u32) -> i32 {
    const IDOK: i32 = 1;
    if level & 0x1f == INSTALLUILEVEL_NONE {
        return 0;
    }
    match kind {
        INSTALLMESSAGE_ACTIONSTART | INSTALLMESSAGE_ACTIONDATA | INSTALLMESSAGE_PROGRESS => IDOK,
        INSTALLMESSAGE_INITIALIZE | INSTALLMESSAGE_TERMINATE => -1,
        // (field 1: 2 shows or hides the Cancel button)
        INSTALLMESSAGE_COMMONDATA => if rec.get(1).int() == 2 { IDOK } else { -1 },
        _ => 0,
    }
}

fn action_start(c: &mut Ctx, p: &Arc<Mutex<Package>>, name: &str) {
    let desc = crate::msi_pkg::query(&p.lock().unwrap().db, &format!("SELECT `Description`, `Template` FROM `ActionText` WHERE `Action` = '{}'", name.replace('\'', "")));
    let mut r = Record::new(3);
    r.f[0] = Field::Str("Action [Time]: [1]. [2]".into());
    r.f[1] = Field::Str(name.into());
    if let Some(d) = desc.first() {
        r.f[2] = d.get(1).clone();
        r.f[3] = d.get(2).clone();
    }
    send(c, Some(p), INSTALLMESSAGE_ACTIONSTART, &r);
}

// ---- sequences and actions

/// Runs a sequence table: each row whose condition holds, in order; stops at the first
/// failure (and returns it). Negative sequences are the dialogs for the end states.
pub fn run_sequence(c: &mut Ctx, p: &Arc<Mutex<Package>>, table: &str) -> u32 {
    let rows = {
        let pk = p.lock().unwrap();
        crate::msi_pkg::query(&pk.db, &format!("SELECT `Action`, `Condition`, `Sequence` FROM `{table}` WHERE `Sequence` > 0 ORDER BY `Sequence`"))
    };
    for r in rows {
        let (name, cond) = (r.get(1).text(), r.get(2).text());
        if !cond_true(&p.lock().unwrap(), &cond) {
            continue;
        }
        let e = do_action(c, p, &name, Some(table));
        match e {
            ERROR_SUCCESS | ERROR_FUNCTION_NOT_CALLED => {}
            // (ERROR_NO_MORE_ITEMS from a custom action: skip the rest, successfully)
            ERROR_NO_MORE_ITEMS => return ERROR_SUCCESS,
            e => return e,
        }
    }
    ERROR_SUCCESS
}

/// MsiDoAction: a standard action, a custom action, or a dialog.
pub fn do_action(c: &mut Ctx, p: &Arc<Mutex<Package>>, name: &str, table: Option<&str>) -> u32 {
    if crate::env_on!("SAKE_TRACE_MSI") {
        eprintln!("msi: action {name}");
    }
    action_start(c, p, name);
    let prev = std::mem::replace(&mut p.lock().unwrap().action, name.to_string());
    let r = do_action_inner(c, p, name, table);
    let mut pk = p.lock().unwrap();
    pk.action = prev;
    pk.sync_props();
    r
}

fn do_action_inner(c: &mut Ctx, p: &Arc<Mutex<Package>>, name: &str, table: Option<&str>) -> u32 {
    let r = match standard(c, p, name, table) {
        Some(r) => r,
        None => {
            let ca = crate::msi_pkg::query(&p.lock().unwrap().db, &format!("SELECT `Action`, `Type`, `Source`, `Target` FROM `CustomAction` WHERE `Action` = '{}'", name.replace('\'', "")));
            if let Some(ca) = ca.first() {
                custom_action(c, p, name, ca.get(2).int(), &ca.get(3).text(), &ca.get(4).text())
            } else {
                let dlg = crate::msi_pkg::query(&p.lock().unwrap().db, &format!("SELECT `Dialog` FROM `Dialog` WHERE `Dialog` = '{}'", name.replace('\'', "")));
                if !dlg.is_empty() {
                    crate::msi_ui::dialog(c, p, name)
                } else {
                    ERROR_FUNCTION_NOT_CALLED
                }
            }
        }
    };
    r
}

/// The standard actions (None: not one).
fn standard(c: &mut Ctx, p: &Arc<Mutex<Package>>, name: &str, _table: Option<&str>) -> Option<u32> {
    Some(match name {
        "CostInitialize" => {
            let mut pk = p.lock().unwrap();
            pk.props.insert("CostingComplete".into(), "0".into());
            pk.cost_started = true;
            pk.resolve_sources();
            ERROR_SUCCESS
        }
        "FileCost" => ERROR_SUCCESS,
        "CostFinalize" => {
            let mut pk = p.lock().unwrap();
            pk.resolve_dirs();
            pk.plan_states();
            pk.costed = true;
            pk.props.insert("CostingComplete".into(), "1".into());
            pk.props.insert("OutOfDiskSpace".into(), "0".into());
            pk.props.insert("OutOfNoRbDiskSpace".into(), "0".into());
            pk.props.insert("PrimaryVolumeSpaceAvailable".into(), "100000000".into());
            pk.props.insert("PrimaryVolumeSpaceRequired".into(), "0".into());
            pk.props.insert("PrimaryVolumeSpaceRemaining".into(), "100000000".into());
            if !pk.props.contains_key("PrimaryVolumePath") {
                pk.props.insert("PrimaryVolumePath".into(), "C:".into());
            }
            ERROR_SUCCESS
        }
        "LaunchConditions" => launch_conditions(c, p),
        "ExecuteAction" => {
            let ui = p.lock().unwrap().prop("EXECUTEACTION");
            // (EXECUTEACTION: the top-level action the UI sequence chose; INSTALL's sequence
            // is the only one here)
            let _ = ui;
            p.lock().unwrap().executed = true;
            run_sequence(c, p, "InstallExecuteSequence")
        }
        "InstallFiles" => install_files(c, p),
        "CreateFolders" => create_folders(p),
        "WriteRegistryValues" => write_registry(p),
        "WriteIniValues" => write_ini(p),
        "CreateShortcuts" => ERROR_SUCCESS,
        "RegisterProduct" | "PublishProduct" => {
            register_product(p);
            ERROR_SUCCESS
        }
        "InstallFinalize" => {
            let mut pk = p.lock().unwrap();
            for c in pk.comps.values_mut() {
                if c.action != INSTALLSTATE_UNKNOWN {
                    c.installed = c.action;
                }
            }
            for f in pk.features.iter_mut() {
                if f.action != INSTALLSTATE_UNKNOWN {
                    f.installed = f.action;
                }
            }
            // (nothing left: the product is no longer installed)
            if pk.prop("REMOVE").eq_ignore_ascii_case("ALL") || pk.features.iter().all(|f| !matches!(f.installed, INSTALLSTATE_LOCAL | INSTALLSTATE_SOURCE | INSTALLSTATE_ADVERTISED)) {
                unregister_product(&pk);
            } else {
                register_features(&pk);
            }
            ERROR_SUCCESS
        }
        "RemoveFiles" => remove_files(p),
        "RemoveRegistryValues" => remove_registry(p),
        "RemoveIniValues" => remove_ini(p),
        "RemoveFolders" => remove_folders(p),
        "AppSearch" => app_search(p),
        "FindRelatedProducts" => find_related(p),
        "ResolveSource" => {
            p.lock().unwrap().resolve_sources();
            ERROR_SUCCESS
        }
        "ProcessComponents" => process_components(p),
        "InstallValidate" | "InstallInitialize" | "ValidateProductID" | "IsolateComponents" | "MigrateFeatureStates"
        | "AllocateRegistrySpace" | "PublishFeatures" | "PublishComponents" | "RegisterUser" | "UnpublishComponents" | "UnpublishFeatures" | "StopServices"
        | "DeleteServices" | "UnregisterComPlus" | "SelfUnregModules" | "UnregisterTypeLibraries" | "RemoveODBC" | "UnregisterFonts"
        | "UnregisterClassInfo" | "UnregisterExtensionInfo" | "UnregisterProgIdInfo" | "UnregisterMIMEInfo" | "RemoveShortcuts"
        | "RemoveEnvironmentStrings" | "RemoveDuplicateFiles" | "MoveFiles" | "PatchFiles" | "BindImage" | "SetODBCFolders"
        | "InstallODBC" | "RegisterComPlus" | "MsiPublishAssemblies" | "MsiUnpublishAssemblies" | "MsiConfigureServices" | "ScheduleReboot" | "CCPSearch"
        | "RMCCPSearch" | "RemoveExistingProducts" | "InstallSFPCatalogFile" | "DisableRollback" | "InstallExecute" | "InstallExecuteAgain" | "InstallAdminPackage"
        | "ForceReboot" | "RegisterMIMEInfo" | "RegisterExtensionInfo" | "RegisterProgIdInfo" | "RegisterClassInfo" | "RegisterTypeLibraries" | "SelfRegModules"
        | "RegisterFonts" | "WriteEnvironmentStrings" | "InstallServices" | "StartServices" | "DuplicateFiles" | "MsiUnpublishAssemblies2" => ERROR_SUCCESS,
        _ => return None,
    })
}

fn launch_conditions(c: &mut Ctx, p: &Arc<Mutex<Package>>) -> u32 {
    let rows = crate::msi_pkg::query(&p.lock().unwrap().db, "SELECT `Condition`, `Description` FROM `LaunchCondition`");
    for r in rows {
        let ok = cond_true(&p.lock().unwrap(), &r.get(1).text());
        if !ok {
            let msg = p.lock().unwrap().format(&r.get(2).text());
            let mut rec = Record::new(0);
            rec.f[0] = Field::Str(msg);
            send(c, Some(p), INSTALLMESSAGE_ERROR, &rec);
            return ERROR_INSTALL_FAILURE;
        }
    }
    ERROR_SUCCESS
}

fn host_mkdirs(win: &str) {
    let _ = kura::fs::create_dir_all(crate::files::resolve_new(win.trim_end_matches('\\')));
}

fn create_folders(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    for r in crate::msi_pkg::query(&pk.db, "SELECT `Directory_`, `Component_` FROM `CreateFolder`") {
        if pk.comps.get(&r.get(2).text()).is_some_and(|c| c.action == INSTALLSTATE_LOCAL) {
            host_mkdirs(&pk.target(&r.get(1).text()));
        }
    }
    ERROR_SUCCESS
}

/// The files to install from each Media row: (cabinet, files by sequence up to LastSequence).
fn install_files(c: &mut Ctx, p: &Arc<Mutex<Package>>) -> u32 {
    let (files, media, compressed_pkg, short_names) = {
        let pk = p.lock().unwrap();
        let wc = match pk.summary.props.get(&msidb::suminfo::PID_WORDCOUNT) {
            Some(msidb::suminfo::Prop::I4(v)) => *v,
            _ => 0,
        };
        let files: Vec<(FileRec, String, String)> = pk
            .files
            .iter()
            .filter(|f| pk.comps.get(&f.comp).is_some_and(|c| c.action == INSTALLSTATE_LOCAL))
            .map(|f| {
                let comp = pk.comps.get(&f.comp).unwrap();
                let src = pk.dirs.get(&comp.dir).map(|d| d.source.clone()).unwrap_or_default();
                (f.clone(), pk.file_target(f), src)
            })
            .collect();
        let media: Vec<(i32, String)> = crate::msi_pkg::query(&pk.db, "SELECT `LastSequence`, `Cabinet` FROM `Media` ORDER BY `DiskId`").iter().map(|r| (r.get(1).int(), r.get(2).text())).collect();
        (files, media, wc & 2 != 0, wc & 1 != 0)
    };
    let total: u64 = files.iter().map(|f| f.0.size as u64).sum();
    progress(c, p, 0, total as i32);
    // (files in cabinets: each cabinet opened once)
    let mut by_cab: Vec<(String, Vec<(FileRec, String)>)> = Vec::new();
    for (f, target, src) in files {
        // msidbFileAttributesNoncompressed 0x2000, Compressed 0x4000
        let compressed = if f.attrs & 0x4000 != 0 { true } else if f.attrs & 0x2000 != 0 { false } else { compressed_pkg };
        let cab = media.iter().find(|m| m.0 >= f.seq).map(|m| m.1.clone()).unwrap_or_default();
        if compressed && !cab.is_empty() {
            match by_cab.iter_mut().find(|x| x.0 == cab) {
                Some(x) => x.1.push((f, target)),
                None => by_cab.push((cab, vec![(f, target)])),
            }
            continue;
        }
        let name = if short_names { &f.source_name } else { &f.name };
        let from = format!("{src}{name}");
        let Some(h) = crate::files::resolve(&from) else {
            eprintln!("msi: source file {from} is missing");
            return ERROR_INSTALL_FAILURE;
        };
        host_mkdirs(target.rsplit_once('\\').map_or("", |d| d.0));
        if kura::fs::copy(&h, crate::files::resolve_new(&target)).is_err() {
            return ERROR_INSTALL_FAILURE;
        }
        file_done(c, p, &f, &target);
    }
    for (cab, list) in by_cab {
        if let Err(e) = extract_cab(c, p, &cab, &list) {
            return e;
        }
    }
    ERROR_SUCCESS
}

fn progress(c: &mut Ctx, p: &Arc<Mutex<Package>>, kind: i32, n: i32) {
    let mut r = Record::new(4);
    r.f[1] = Field::Int(kind);
    r.f[2] = Field::Int(n);
    r.f[3] = Field::Int(0);
    r.f[4] = Field::Int(0);
    send(c, Some(p), INSTALLMESSAGE_PROGRESS, &r);
}

fn file_done(c: &mut Ctx, p: &Arc<Mutex<Package>>, f: &FileRec, target: &str) {
    let mut r = Record::new(3);
    r.f[1] = Field::Str(f.name.clone());
    r.f[3] = Field::Int(f.size as i32);
    r.f[2] = Field::Str(target.rsplit_once('\\').map_or(String::new(), |d| d.0.to_string()));
    send(c, Some(p), INSTALLMESSAGE_ACTIONDATA, &r);
    let mut pr = Record::new(3);
    pr.f[1] = Field::Int(2);
    pr.f[2] = Field::Int(f.size as i32);
    pr.f[3] = Field::Int(0);
    send(c, Some(p), INSTALLMESSAGE_PROGRESS, &pr);
}

/// A cabinet: "#stream" in the package, or a file beside it.
fn extract_cab(c: &mut Ctx, p: &Arc<Mutex<Package>>, cab: &str, list: &[(FileRec, String)]) -> R<()> {
    let tmp;
    let host = if let Some(stream) = cab.strip_prefix('#') {
        let pk = p.lock().unwrap();
        let d = pk.db.lock().unwrap();
        tmp = crate::files::resolve_new(&format!("C:\\windows\\temp\\sake-msi-{}.cab", crate::pid()));
        host_mkdirs("C:\\windows\\temp");
        let mut f = kura::fs::File::create(&tmp).map_err(|_| ERROR_INSTALL_FAILURE)?;
        d.db.copy_stream(stream, &mut f).map_err(|_| ERROR_INSTALL_FAILURE)?;
        tmp.clone()
    } else {
        let src = p.lock().unwrap().prop("SourceDir");
        let w = format!("{}{cab}", if src.ends_with('\\') { src } else { format!("{src}\\") });
        match crate::files::resolve(&w) {
            Some(h) => h,
            None => {
                eprintln!("msi: cabinet {w} is missing");
                return Err(ERROR_INSTALL_FAILURE);
            }
        }
    };
    let f = kura::fs::File::open(&host).map_err(|_| ERROR_INSTALL_FAILURE)?;
    let mut cabinet = cab::Cabinet::new(f).map_err(|e| {
        eprintln!("msi: cabinet {cab}: {e}");
        ERROR_INSTALL_FAILURE
    })?;
    let names: Vec<String> = cabinet.folder_entries().flat_map(|fo| fo.file_entries().map(|e| e.name().to_string()).collect::<Vec<_>>()).collect();
    for (f, target) in list {
        if !names.iter().any(|n| *n == f.key) {
            eprintln!("msi: {} is not in cabinet {cab}", f.key);
            return Err(ERROR_INSTALL_FAILURE);
        }
        let mut r = cabinet.read_file(&f.key).map_err(|_| ERROR_INSTALL_FAILURE)?;
        host_mkdirs(target.rsplit_once('\\').map_or("", |d| d.0));
        let mut out = kura::fs::File::create(crate::files::resolve_new(target)).map_err(|_| ERROR_INSTALL_FAILURE)?;
        std::io::copy(&mut r, &mut out).map_err(|_| ERROR_INSTALL_FAILURE)?;
        file_done(c, p, f, target);
    }
    if cab.starts_with('#') {
        let _ = kura::fs::remove_file(&host);
    }
    Ok(())
}

fn root_path(root: i32, all_users: bool) -> &'static str {
    match root {
        -1 => if all_users { "hklm" } else { "hkcu" },
        0 => "hkcr",
        1 => "hkcu",
        2 => "hklm",
        3 => "hku",
        _ => "hklm",
    }
}

fn wide_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// A Registry table value: "#123" DWORD, "#x0a0b" binary, "#%" expandable, "##" a "#"
/// string, "[~]" separated multi-strings, else a string.
pub fn reg_value(v: &str) -> (u32, Vec<u8>) {
    if let Some(h) = v.strip_prefix("#x").or_else(|| v.strip_prefix("#X")) {
        let b = (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()).collect();
        return (3, b);
    }
    if let Some(s) = v.strip_prefix("#%") {
        return (2, wide_bytes(s));
    }
    if let Some(s) = v.strip_prefix("##") {
        return (1, wide_bytes(&format!("#{s}")));
    }
    if let Some(n) = v.strip_prefix('#') {
        let n = n.trim();
        let i = if let Some(neg) = n.strip_prefix('-') { neg.parse::<i64>().map(|x| -x).unwrap_or(0) } else { n.trim_start_matches('+').parse::<i64>().unwrap_or(0) };
        return (4, (i as u32).to_le_bytes().to_vec());
    }
    if v.contains('\0') {
        let mut b: Vec<u8> = Vec::new();
        for part in v.split('\0').filter(|s| !s.is_empty()) {
            b.extend(wide_bytes(part));
        }
        b.extend([0, 0]);
        return (7, b);
    }
    (1, wide_bytes(v))
}

fn write_registry(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    let all = pk.prop("ALLUSERS") == "1" || pk.prop("ALLUSERS") == "2";
    for r in crate::msi_pkg::query(&pk.db, "SELECT `Root`, `Key`, `Name`, `Value`, `Component_` FROM `Registry`") {
        if !pk.comps.get(&r.get(5).text()).is_some_and(|c| c.action == INSTALLSTATE_LOCAL) {
            continue;
        }
        let key = format!("{}\\{}", root_path(r.get(1).int(), all), pk.format(&r.get(2).text()));
        let name = pk.format(&r.get(3).text());
        let value = r.get(4);
        if matches!(name.as_str(), "+" | "-" | "*") && value.is_null() {
            crate::misc::reg_create(&key);
            continue;
        }
        let (ty, data) = reg_value(&pk.format(&value.text()));
        crate::misc::reg_put(&key, &name, ty, data);
    }
    ERROR_SUCCESS
}

fn write_ini(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    for r in crate::msi_pkg::query(&pk.db, "SELECT `FileName`, `DirProperty`, `Section`, `Key`, `Value`, `Component_` FROM `IniFile`") {
        if !pk.comps.get(&r.get(6).text()).is_some_and(|c| c.action == INSTALLSTATE_LOCAL) {
            continue;
        }
        let file = ini_file(&pk, &r);
        host_mkdirs(file.rsplit_once('\\').map_or("", |d| d.0));
        crate::ini::write_entry(&file, &pk.format(&r.get(3).text()), &pk.format(&r.get(4).text()), &pk.format(&r.get(5).text()));
    }
    ERROR_SUCCESS
}

fn absent(pk: &Package, comp: &str) -> bool {
    pk.comps.get(comp).is_some_and(|c| c.action == INSTALLSTATE_ABSENT)
}

/// RemoveFiles: the files of components going away, and the RemoveFile table's (on removal
/// of its component: InstallMode 2 or 3).
fn remove_files(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    for f in pk.files.iter().filter(|f| absent(&pk, &f.comp)) {
        if let Some(h) = crate::files::resolve(&pk.file_target(f)) {
            let _ = kura::fs::remove_file(h);
        }
    }
    for r in crate::msi_pkg::query(&pk.db, "SELECT `Component_`, `FileName`, `DirProperty`, `InstallMode` FROM `RemoveFile`") {
        if !absent(&pk, &r.get(1).text()) || r.get(4).int() & 2 == 0 || r.get(2).is_null() {
            continue;
        }
        let dir = pk.prop(&r.get(3).text());
        let pat = long_name(&r.get(2).text()).to_lowercase();
        let Some(hd) = crate::files::resolve(dir.trim_end_matches('\\')) else { continue };
        for e in kura::fs::read_dir(&hd).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if wildcard(&pat, &n) && kura::fs::metadata(e.path()).is_ok_and(|m| m.is_file()) {
                let _ = kura::fs::remove_file(e.path());
            }
        }
    }
    // (the folders of the components that went, when that left them empty)
    let dirs = pk.comps.values().filter(|c| c.action == INSTALLSTATE_ABSENT).map(|c| pk.target(&c.dir)).collect();
    remove_empty(&pk, dirs);
    ERROR_SUCCESS
}

/// A RemoveFile name pattern: * any run, ? one character.
fn wildcard(p: &str, n: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (p.chars().collect(), n.chars().collect());
    fn m(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| m(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && m(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && m(&p[1..], &n[1..]),
        }
    }
    m(&p, &n)
}

/// RemoveRegistryValues: the Registry table's values of components going away (a key
/// marked "-" goes with everything under it).
fn remove_registry(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    let all = pk.prop("ALLUSERS") == "1" || pk.prop("ALLUSERS") == "2";
    for r in crate::msi_pkg::query(&pk.db, "SELECT `Root`, `Key`, `Name`, `Value`, `Component_` FROM `Registry`") {
        if !absent(&pk, &r.get(5).text()) {
            continue;
        }
        let key = format!("{}\\{}", root_path(r.get(1).int(), all), pk.format(&r.get(2).text()));
        let name = pk.format(&r.get(3).text());
        if matches!(name.as_str(), "-" | "*") && r.get(4).is_null() {
            crate::misc::reg_delete_tree(&key);
        } else if name != "+" {
            crate::misc::reg_delete_value(&key, &name);
            // (a key left with nothing in it goes too)
            if crate::misc::reg_values(&key).is_empty() && crate::misc::reg_subkeys(&key.to_lowercase()).is_empty() {
                crate::misc::reg_delete_tree(&key);
            }
        }
    }
    ERROR_SUCCESS
}

fn ini_file(pk: &Package, r: &Record) -> String {
    let dir = r.get(2).text();
    let dir = if dir.is_empty() {
        "C:\\windows\\".to_string()
    } else {
        let t = pk.target(&dir);
        if t.is_empty() { pk.prop(&dir) } else { t }
    };
    format!("{dir}{}", long_name(&r.get(1).text()))
}

fn remove_ini(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    for r in crate::msi_pkg::query(&pk.db, "SELECT `FileName`, `DirProperty`, `Section`, `Key`, `Value`, `Component_` FROM `IniFile`") {
        if absent(&pk, &r.get(6).text()) {
            crate::ini::delete_entry(&ini_file(&pk, &r), &pk.format(&r.get(3).text()), &pk.format(&r.get(4).text()));
        }
    }
    ERROR_SUCCESS
}

/// Removes the folders in `dirs` that are empty, deepest first, and their parents left empty
/// that are the package's own directories.
fn remove_empty(pk: &Package, dirs: Vec<String>) {
    let own: Vec<String> = pk.dirs.values().map(|d| d.target.to_lowercase()).collect();
    let mut todo = dirs;
    while let Some(d) = {
        todo.sort_by_key(|d| d.len());
        todo.pop()
    } {
        let Some(h) = crate::files::resolve(d.trim_end_matches('\\')) else { continue };
        if kura::fs::remove_dir(&h).is_ok() {
            if let Some((parent, _)) = d.trim_end_matches('\\').rsplit_once('\\') {
                let parent = format!("{parent}\\");
                if own.contains(&parent.to_lowercase()) && !todo.contains(&parent) {
                    todo.push(parent);
                }
            }
        }
    }
}

/// RemoveFolders: CreateFolder's folders of components going away, when empty.
fn remove_folders(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    let mut dirs = Vec::new();
    for r in crate::msi_pkg::query(&pk.db, "SELECT `Directory_`, `Component_` FROM `CreateFolder`") {
        if absent(&pk, &r.get(2).text()) {
            dirs.push(pk.target(&r.get(1).text()));
        }
    }
    remove_empty(&pk, dirs);
    ERROR_SUCCESS
}

/// AppSearch: properties from registry (RegLocator) and directory (DrLocator) searches.
fn app_search(p: &Arc<Mutex<Package>>) -> u32 {
    crate::msi_search::app_search(&mut p.lock().unwrap());
    ERROR_SUCCESS
}

/// FindRelatedProducts: the Upgrade table's codes against the products registered.
fn find_related(p: &Arc<Mutex<Package>>) -> u32 {
    let mut pk = p.lock().unwrap();
    for r in crate::msi_pkg::query(&pk.db, "SELECT `UpgradeCode`, `ActionProperty` FROM `Upgrade`") {
        let found = related_products(&r.get(1).text());
        if !found.is_empty() {
            let prop = r.get(2).text();
            pk.set_prop(&prop, &found.join(";"));
        }
    }
    ERROR_SUCCESS
}

// ---- custom actions

fn custom_action(c: &mut Ctx, p: &Arc<Mutex<Package>>, name: &str, ty: i32, source: &str, target: &str) -> u32 {
    let base = ty & 0x3f;
    let deferred = ty & 0x400 != 0;
    let rollback = ty & 0x700 == 0x500;
    if rollback {
        return ERROR_SUCCESS;
    }
    if deferred {
        // (CustomActionData: the property named after the action)
        let mut pk = p.lock().unwrap();
        let data = pk.prop(name);
        pk.props.insert("CustomActionData".into(), data);
    }
    let ignore = ty & 0x40 != 0;
    let r = match base {
        // set a property / a directory
        51 => {
            let mut pk = p.lock().unwrap();
            let (s, v) = (pk.format(source), pk.format(target));
            pk.set_prop(&s, &v);
            ERROR_SUCCESS
        }
        35 => {
            let mut pk = p.lock().unwrap();
            let v = pk.format(target);
            if pk.costed {
                pk.set_target(source, &v);
            } else {
                pk.props.insert(source.to_string(), v);
            }
            ERROR_SUCCESS
        }
        // error: show Target (formatted, or an Error table number) and end
        19 => {
            let msg = p.lock().unwrap().format(target);
            let mut rec = Record::new(0);
            rec.f[0] = Field::Str(msg);
            send(c, Some(p), INSTALLMESSAGE_ERROR, &rec);
            ERROR_INSTALL_FAILURE
        }
        // a DLL from the Binary table or an installed file
        1 | 17 => {
            let dll = if base == 1 { binary_to_file(p, source, "dll") } else { file_path(p, source) };
            match dll {
                Some(path) => call_dll(c, p, &path, target),
                None => ERROR_INSTALL_FAILURE,
            }
        }
        // programs: from the Binary table (2), an installed file (18), a directory (34), a
        // property (50)
        2 | 18 | 34 | 50 => {
            let (prog, args) = {
                let pk = p.lock().unwrap();
                match base {
                    2 => (binary_to_file_locked(&pk, source, "exe").unwrap_or_default(), pk.format(target)),
                    18 => (file_path_locked(&pk, source).unwrap_or_default(), pk.format(target)),
                    50 => (pk.prop(source), pk.format(target)),
                    _ => (String::new(), pk.format(target)),
                }
            };
            let cmd = if base == 34 { args } else { format!("\"{prog}\" {args}") };
            let wait = ty & 0x80 == 0;
            run_program(c, &cmd, wait)
        }
        _ => {
            eprintln!("msi: custom action {name} type {ty} is not supported");
            ERROR_SUCCESS
        }
    };
    if ignore { ERROR_SUCCESS } else { r }
}

fn binary_to_file(p: &Arc<Mutex<Package>>, key: &str, ext: &str) -> Option<String> {
    binary_to_file_locked(&p.lock().unwrap(), key, ext)
}

fn binary_to_file_locked(pk: &Package, key: &str, ext: &str) -> Option<String> {
    let rows = crate::msi_pkg::query(&pk.db, &format!("SELECT `Data` FROM `Binary` WHERE `Name` = '{}'", key.replace('\'', "")));
    let Field::Stream(data) = rows.first()?.get(1).clone() else { return None };
    host_mkdirs("C:\\windows\\temp");
    let w = format!("C:\\windows\\temp\\MSI{}_{}.{ext}", crate::pid(), key.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>());
    kura::fs::write(crate::files::resolve_new(&w), data).ok()?;
    Some(w)
}

fn file_path(p: &Arc<Mutex<Package>>, key: &str) -> Option<String> {
    file_path_locked(&p.lock().unwrap(), key)
}

fn file_path_locked(pk: &Package, key: &str) -> Option<String> {
    let f = pk.files.iter().find(|f| f.key == key)?;
    Some(pk.file_target(f))
}

/// Calls entry(hInstall) of a custom action DLL; its result (ERROR_SUCCESS,
/// ERROR_INSTALL_USEREXIT...).
fn call_dll(c: &mut Ctx, p: &Arc<Mutex<Package>>, path: &str, entry: &str) -> u32 {
    let base = crate::pe::load_library(c.cpu, path);
    if base == 0 {
        eprintln!("msi: custom action DLL {path} does not load");
        return ERROR_INSTALL_FAILURE;
    }
    let Some(f) = crate::pe::proc_address(base, crate::pe::Import::Name(entry)) else {
        eprintln!("msi: {path} has no {entry}");
        return ERROR_INSTALL_FAILURE;
    };
    let h = new_handle(Obj::Pkg(p.clone()));
    let r = c.call(f, &[h]);
    crate::msi::close(h);
    match r {
        // (ERROR_FUNCTION_NOT_CALLED 1626 and ERROR_NO_MORE_ITEMS 259 pass through)
        0 | 259 | 1626 | 1602 => r,
        _ => ERROR_INSTALL_FAILURE,
    }
}

/// Starts a program (CreateProcess) and, when `wait`, waits for it.
fn run_program(c: &mut Ctx, cmd: &str, wait: bool) -> u32 {
    let k32 = crate::pe::load_library(c.cpu, "kernel32.dll");
    let f = |n: &str| crate::pe::proc_address(k32, crate::pe::Import::Name(n));
    let (Some(create), Some(wait_fn), Some(close)) = (f("CreateProcessW"), f("WaitForSingleObject"), f("CloseHandle")) else {
        return ERROR_INSTALL_FAILURE;
    };
    let w: Vec<u8> = cmd.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect();
    let buf = mem::heap_alloc(w.len() as u32 + 256, true);
    mem::write(buf, &w);
    let si = buf + w.len() as u32 + 8 & !7;
    let pi = si + 128;
    w32(si, if mem::x64() { 104 } else { 68 });
    let ok = c.call(create, &[0, buf, 0, 0, 0, 0, 0, 0, si, pi]) != 0;
    if ok {
        let (hp, ht) = (mem::rp(pi), mem::rp(pi + mem::ps()));
        if wait {
            c.call(wait_fn, &[hp, u32::MAX]);
        }
        c.call(close, &[ht]);
        c.call(close, &[hp]);
    }
    mem::heap_free(buf);
    if ok { ERROR_SUCCESS } else { ERROR_INSTALL_FAILURE }
}

// ---- product registration

/// A GUID's packed form (the registry keys of installed products).
pub fn pack_guid(g: &str) -> String {
    let h: Vec<char> = g.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 32 {
        return String::new();
    }
    let rev = |s: &[char]| s.iter().rev().collect::<String>();
    let mut out = String::new();
    out += &rev(&h[0..8]);
    out += &rev(&h[8..12]);
    out += &rev(&h[12..16]);
    for i in (16..32).step_by(2) {
        out.push(h[i + 1]);
        out.push(h[i]);
    }
    out.to_uppercase()
}

pub fn unpack_guid(p: &str) -> String {
    let h: Vec<char> = p.chars().collect();
    if h.len() != 32 {
        return String::new();
    }
    let rev = |s: &[char]| s.iter().rev().collect::<String>();
    let mut b = String::new();
    for i in (16..32).step_by(2) {
        b.push(h[i + 1]);
        b.push(h[i]);
    }
    format!("{{{}-{}-{}-{}-{}}}", rev(&h[0..8]), rev(&h[8..12]), rev(&h[12..16]), &b[0..4], &b[4..16])
}

const PRODUCTS: &str = "hklm\\software\\microsoft\\windows\\currentversion\\installer\\userdata\\s-1-5-18\\products";
const UPGRADES: &str = "hklm\\software\\classes\\installer\\upgradecodes";
const UNINSTALL: &str = "hklm\\software\\microsoft\\windows\\currentversion\\uninstall";

fn put_str(key: &str, name: &str, v: &str) {
    crate::misc::reg_put(key, name, 1, wide_bytes(v));
}

fn get_str(key: &str, name: &str) -> Option<String> {
    let (ty, d) = crate::misc::reg_get(key, name)?;
    Some(match ty {
        4 if d.len() >= 4 => u32::from_le_bytes(d[..4].try_into().unwrap()).to_string(),
        _ => {
            let u: Vec<u16> = d.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
            String::from_utf16_lossy(&u)
        }
    })
}

fn register_product(p: &Arc<Mutex<Package>>) {
    let pk = p.lock().unwrap();
    let code = pk.prop("ProductCode");
    let packed = pack_guid(&code);
    if packed.is_empty() {
        return;
    }
    let ip = format!("{PRODUCTS}\\{packed}\\installproperties");
    let un = format!("{UNINSTALL}\\{code}");
    for key in [&ip, &un] {
        put_str(key, "DisplayName", &pk.prop("ProductName"));
        put_str(key, "DisplayVersion", &pk.prop("ProductVersion"));
        put_str(key, "Publisher", &pk.prop("Manufacturer"));
        put_str(key, "InstallLocation", &pk.prop("ARPINSTALLLOCATION"));
        put_str(key, "InstallSource", &pk.prop("SourceDir"));
        put_str(key, "LocalPackage", &pk.path);
        put_str(key, "Language", &pk.prop("ProductLanguage"));
        put_str(key, "UninstallString", &format!("MsiExec.exe /X{code}"));
        crate::misc::reg_put(key, "WindowsInstaller", 4, 1u32.to_le_bytes().to_vec());
    }
    put_str(&ip, "PackageCode", &pk.prop("PackageCode"));
    put_str(&ip, "ProductCode", &code);
    let up = pk.prop("UpgradeCode");
    let pu = pack_guid(&up);
    if !pu.is_empty() {
        put_str(&format!("{UPGRADES}\\{pu}"), &packed, "");
    }
}

/// The installed features (HKLM\\Software\\Classes\\Installer\\Features\\<packed code>: one
/// value per feature).
const FEATURES: &str = "hklm\\software\\classes\\installer\\features";
const USERDATA: &str = "hklm\\software\\microsoft\\windows\\currentversion\\installer\\userdata\\s-1-5-18";

/// The features: HKLM\Software\Classes\Installer\Features\<product> holds each one's
/// parent ("\x06" first when absent), UserData\...\Products\<product>\Features its
/// components (base 85 GUIDs).
fn register_features(pk: &Package) {
    let packed = pack_guid(&pk.prop("ProductCode"));
    if packed.is_empty() {
        return;
    }
    let key = format!("{FEATURES}\\{packed}");
    let ud = format!("{USERDATA}\\products\\{packed}\\features");
    for k in [&key, &ud] {
        crate::misc::reg_delete_tree(k);
        crate::misc::reg_create(k);
    }
    for f in &pk.features {
        let on = matches!(f.installed, INSTALLSTATE_LOCAL | INSTALLSTATE_SOURCE | INSTALLSTATE_ADVERTISED);
        put_str(&key, &f.name, &if on { f.parent.clone() } else { format!("\u{6}{}", f.parent) });
        if on {
            let comps: String = pk.uses.iter().filter(|u| u.0 == f.name).filter_map(|u| pk.comps.get(&u.1)).filter_map(|c| encode_guid85(&c.id)).collect();
            put_str(&ud, &f.name, &comps);
        }
    }
}

/// ProcessComponents: each component's path for this product (its key file; "01:" first
/// when it runs from source), gone when it is removed.
fn process_components(p: &Arc<Mutex<Package>>) -> u32 {
    let pk = p.lock().unwrap();
    let packed = pack_guid(&pk.prop("ProductCode"));
    if packed.is_empty() {
        return ERROR_SUCCESS;
    }
    for c in pk.comps.values().filter(|c| !c.id.is_empty() && c.enabled) {
        let key = format!("{USERDATA}\\components\\{}", pack_guid(&c.id));
        let file = pk.files.iter().find(|f| f.key == c.keypath);
        match c.action {
            INSTALLSTATE_LOCAL => {
                let path = file.map(|f| pk.file_target(f)).unwrap_or_else(|| pk.target(&c.dir));
                put_str(&key, &packed, &path);
            }
            INSTALLSTATE_SOURCE => {
                let src = pk.dirs.get(&c.dir).map(|d| d.source.clone()).unwrap_or_default();
                let root = pk.prop("SourceDir");
                let rel = src.strip_prefix(root.as_str()).unwrap_or(&src).to_string();
                put_str(&key, &packed, &format!("01:{rel}{}", file.map(|f| f.name.as_str()).unwrap_or("")));
            }
            INSTALLSTATE_ABSENT => {
                crate::misc::reg_delete_value(&key, &packed);
                if crate::misc::reg_values(&key).is_empty() {
                    crate::misc::reg_delete_tree(&key);
                }
            }
            _ => {}
        }
    }
    ERROR_SUCCESS
}

/// A component's state for `code` (LOCAL, SOURCE or ABSENT) and how many products use it.
pub fn component_registration(code: &str, id: &str) -> (i32, i32) {
    let key = format!("{USERDATA}\\components\\{}", pack_guid(id));
    let clients = crate::misc::reg_values(&key).len() as i32;
    let st = match get_str(&key, &pack_guid(code)) {
        None => INSTALLSTATE_ABSENT,
        Some(v) if is_source_path(&v) => INSTALLSTATE_SOURCE,
        Some(_) => INSTALLSTATE_LOCAL,
    };
    (st, clients)
}

/// Where a component is installed (MsiLocateComponent): its path for the first product
/// that has it locally.
pub fn locate_component(id: &str) -> Option<String> {
    // (the machine's, then each user's)
    const ROOT: &str = "hklm\\software\\microsoft\\windows\\currentversion\\installer\\userdata";
    let mut sids = vec!["s-1-5-18".to_string()];
    sids.extend(crate::misc::reg_subkeys(ROOT).into_iter().map(|s| s.to_lowercase()).filter(|s| s != "s-1-5-18"));
    sids.iter().find_map(|sid| {
        let key = format!("{ROOT}\\{sid}\\components\\{}", pack_guid(id));
        crate::misc::reg_values(&key).into_iter().filter_map(|n| get_str(&key, &n)).find(|v| !v.is_empty() && !is_source_path(v))
    })
}

fn is_source_path(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() > 2 && b[0].is_ascii_digit() && b[1].is_ascii_digit()
}

pub fn installed_features(code: &str) -> Vec<String> {
    let packed = pack_guid(code);
    if packed.is_empty() {
        return Vec::new();
    }
    let key = format!("{FEATURES}\\{packed}");
    crate::misc::reg_values(&key).into_iter().filter(|f| !get_str(&key, f).is_some_and(|v| v.starts_with('\u{6}'))).collect()
}

/// MsiQueryFeatureState: LOCAL, ABSENT, or UNKNOWN (no such product or feature).
pub fn feature_state(code: &str, feature: &str) -> i32 {
    let packed = pack_guid(code);
    if packed.is_empty() || !product_installed(code) {
        return INSTALLSTATE_UNKNOWN;
    }
    match get_str(&format!("{FEATURES}\\{packed}"), feature) {
        None => return INSTALLSTATE_UNKNOWN,
        Some(v) if v.starts_with('\u{6}') => return INSTALLSTATE_ABSENT,
        Some(_) => {}
    }
    // (from its components: one missing, advertised; one from source, source)
    let Some(comps) = get_str(&format!("{USERDATA}\\products\\{packed}\\features"), feature) else { return INSTALLSTATE_ADVERTISED };
    let c: Vec<char> = comps.chars().collect();
    let mut source = false;
    for g in c.chunks(20).take_while(|g| g.len() == 20 && g[0] != '\u{2}') {
        let Some(id) = decode_guid85(g) else { break };
        match get_str(&format!("{USERDATA}\\components\\{}", pack_guid(&id)), &packed) {
            None => return INSTALLSTATE_ADVERTISED,
            Some(v) if is_source_path(&v) => source = true,
            Some(_) => {}
        }
    }
    if source { INSTALLSTATE_SOURCE } else { INSTALLSTATE_LOCAL }
}

fn unregister_product(pk: &Package) {
    let code = pk.prop("ProductCode");
    let packed = pack_guid(&code);
    if packed.is_empty() {
        return;
    }
    crate::misc::reg_delete_tree(&format!("{PRODUCTS}\\{packed}"));
    crate::misc::reg_delete_tree(&format!("{UNINSTALL}\\{code}"));
    crate::misc::reg_delete_tree(&format!("{FEATURES}\\{packed}"));
    let pu = pack_guid(&pk.prop("UpgradeCode"));
    if !pu.is_empty() {
        let key = format!("{UPGRADES}\\{pu}");
        crate::misc::reg_delete_value(&key, &packed);
        if crate::misc::reg_values(&key).is_empty() {
            crate::misc::reg_delete_tree(&key);
        }
    }
    for c in pk.comps.values().filter(|c| !c.id.is_empty()) {
        let key = format!("hklm\\software\\microsoft\\windows\\currentversion\\installer\\userdata\\s-1-5-18\\components\\{}", pack_guid(&c.id));
        crate::misc::reg_delete_value(&key, &packed);
        if crate::misc::reg_values(&key).is_empty() {
            crate::misc::reg_delete_tree(&key);
        }
    }
}

pub fn product_installed(code: &str) -> bool {
    let p = pack_guid(code);
    !p.is_empty() && get_str(&format!("{PRODUCTS}\\{p}\\installproperties"), "ProductCode").is_some()
}

pub fn related_products(upgrade: &str) -> Vec<String> {
    let pu = pack_guid(upgrade);
    if pu.is_empty() {
        return Vec::new();
    }
    let key = format!("{UPGRADES}\\{pu}");
    crate::misc::reg_values(&key).into_iter().map(|n| unpack_guid(&n.to_uppercase())).filter(|g| !g.is_empty()).collect()
}

pub fn product_info(code: &str, prop: &str) -> Option<String> {
    let p = pack_guid(code);
    let key = format!("{PRODUCTS}\\{p}\\installproperties");
    get_str(&key, "ProductCode")?;
    let name = match prop {
        "ProductName" | "InstalledProductName" => "DisplayName",
        "VersionString" => "DisplayVersion",
        "Publisher" => "Publisher",
        "InstallLocation" => "InstallLocation",
        "InstallSource" => "InstallSource",
        "LocalPackage" => "LocalPackage",
        "Language" => "Language",
        "PackageCode" => "PackageCode",
        p => p,
    };
    get_str(&key, name)
}

// ---- installing a product

/// Properties from a command line: NAME=value NAME="value with spaces".
pub fn parse_props(cmd: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let c: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let st = i;
        while i < c.len() && c[i] != '=' && !c[i].is_whitespace() {
            i += 1;
        }
        let name: String = c[st..i].iter().collect();
        if i >= c.len() || c[i] != '=' {
            continue;
        }
        i += 1;
        let mut v = String::new();
        if i < c.len() && c[i] == '"' {
            i += 1;
            while i < c.len() {
                if c[i] == '"' {
                    if c.get(i + 1) == Some(&'"') {
                        v.push('"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                v.push(c[i]);
                i += 1;
            }
        } else {
            while i < c.len() && !c[i].is_whitespace() {
                v.push(c[i]);
                i += 1;
            }
        }
        if !name.is_empty() {
            out.push((name, v));
        }
    }
    out
}

/// MsiInstallProduct: opens the package, sets the command line's properties, runs the UI
/// sequence (full or reduced UI) or the execute sequence.
pub fn install_product(c: &mut Ctx, path: &str, cmdline: &str) -> u32 {
    let mut p = match open_package(path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // (TRANSFORMS: applied to the package before anything reads it)
    let props = parse_props(cmdline);
    if let Some((_, t)) = props.iter().rev().find(|(k, _)| k == "TRANSFORMS") {
        let (db, full) = {
            let pk = p.lock().unwrap();
            (pk.db.clone(), pk.path.clone())
        };
        let e = apply_transforms(&db, &full, t);
        if e != ERROR_SUCCESS {
            return e;
        }
        p = Arc::new(Mutex::new(Package::open(db, &full)));
    }
    let level = match UI.lock().unwrap().level & 0x1f {
        INSTALLUILEVEL_DEFAULT => INSTALLUILEVEL_BASIC,
        l => l,
    };
    {
        let mut pk = p.lock().unwrap();
        pk.ui_level = level;
        pk.props.insert("UILevel".into(), level.to_string());
        for (k, v) in props {
            pk.set_prop(&k, &v);
        }
        pk.props.entry("ACTION".into()).or_insert("INSTALL".into());
        let code = pk.prop("ProductCode");
        if product_installed(&code) {
            pk.props.insert("Installed".into(), "1".into());
            pk.props.insert("ProductState".into(), "5".into());
        }
    }
    let mut r = Record::new(0);
    send(c, Some(&p), INSTALLMESSAGE_INITIALIZE, &r);
    let seq = if level >= INSTALLUILEVEL_REDUCED && table_exists(&p, "InstallUISequence") { "InstallUISequence" } else { "InstallExecuteSequence" };
    let mut e = run_sequence(c, &p, seq);
    if seq == "InstallUISequence" && e == ERROR_SUCCESS && !ran_execute(&p) {
        e = run_sequence(c, &p, "InstallExecuteSequence");
    }
    // the end dialog: -1 success, -2 user exit, -3 failure
    let end = match e {
        ERROR_SUCCESS => -1,
        ERROR_INSTALL_USEREXIT => -2,
        _ => -3,
    };
    if level >= INSTALLUILEVEL_REDUCED {
        let rows = crate::msi_pkg::query(&p.lock().unwrap().db, &format!("SELECT `Action` FROM `InstallUISequence` WHERE `Sequence` = {end}"));
        if let Some(a) = rows.first() {
            let name = a.get(1).text();
            do_action(c, &p, &name, None);
        }
    }
    r.f[0] = Field::Null;
    send(c, Some(&p), INSTALLMESSAGE_TERMINATE, &r);
    e
}

/// The TRANSFORMS list ("a.mst;:Embedded;@b.mst"): ':' a storage in the package, '@' and
/// '|' (secured at the source / by full path) files; relative paths are the package's
/// folder's.
pub fn apply_transforms(db: &crate::msi::Db, package: &str, list: &str) -> u32 {
    let dir = package.rsplit_once('\\').map_or(String::new(), |d| format!("{}\\", d.0));
    for t in list.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        let r = apply_transform(db, &dir, t);
        if r != ERROR_SUCCESS {
            eprintln!("msi: transform {t} not applied ({r})");
            return ERROR_INSTALL_TRANSFORM_FAILURE;
        }
    }
    ERROR_SUCCESS
}

pub fn apply_transform(db: &crate::msi::Db, dir: &str, t: &str) -> u32 {
    let mut o = db.lock().unwrap();
    let data = if let Some(name) = t.strip_prefix(':') {
        o.db.storage(name)
    } else {
        let f = t.trim_start_matches(['@', '|']);
        let path = if f.contains(':') || f.starts_with('\\') { f.to_string() } else { format!("{dir}{f}") };
        crate::files::resolve(&path).and_then(|h| kura::fs::read(h).ok())
    };
    let Some(data) = data else { return ERROR_OPEN_FAILED };
    match o.db.apply_transform(&data) {
        Ok(()) => ERROR_SUCCESS,
        Err(e) => e,
    }
}

fn table_exists(p: &Arc<Mutex<Package>>, t: &str) -> bool {
    p.lock().unwrap().db.lock().unwrap().db.table(t).is_some()
}

fn ran_execute(p: &Arc<Mutex<Package>>) -> bool {
    // (the UI sequence's ExecuteAction ran the execute sequence)
    p.lock().unwrap().executed
}

// ---- entry points

fn install_product_api(c: &mut Ctx, wide: bool) -> u32 {
    let (p, cmd) = (c.a(0), c.a(1));
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let path = arg_str(p, wide);
    let cmd = if cmd == 0 { String::new() } else { arg_str(cmd, wide) };
    install_product(c, &path, &cmd)
}
fn MsiInstallProductA(c: &mut Ctx) -> u32 {
    install_product_api(c, false)
}
fn MsiInstallProductW(c: &mut Ctx) -> u32 {
    install_product_api(c, true)
}

fn do_action_api(c: &mut Ctx, wide: bool) -> u32 {
    let (h, a) = (c.a(0), c.a(1));
    if a == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    let name = arg_str(a, wide);
    do_action(c, &p, &name, None)
}
fn MsiDoActionA(c: &mut Ctx) -> u32 {
    do_action_api(c, false)
}
fn MsiDoActionW(c: &mut Ctx) -> u32 {
    do_action_api(c, true)
}

fn sequence_api(c: &mut Ctx, wide: bool) -> u32 {
    let (h, t) = (c.a(0), c.a(1));
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    if t == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let t = arg_str(t, wide);
    if !table_exists(&p, &t) {
        return ERROR_BAD_QUERY_SYNTAX;
    }
    run_sequence(c, &p, &t)
}
fn MsiSequenceA(c: &mut Ctx) -> u32 {
    sequence_api(c, false)
}
fn MsiSequenceW(c: &mut Ctx) -> u32 {
    sequence_api(c, true)
}

fn MsiSetInternalUI(c: &mut Ctx) -> u32 {
    // (dwUILevel: a level 0..5 and INSTALLUILEVEL_ flags above it; phWnd: the parent window,
    // swapped with the previous one)
    let (level, phwnd) = (c.a(0), c.a(1));
    let base = level & 0x1f;
    if base > INSTALLUILEVEL_FULL || level & !0x1f & !UI_FLAGS != 0 {
        return INSTALLUILEVEL_NOCHANGE;
    }
    let mut u = UI.lock().unwrap();
    let old = u.level;
    if phwnd != 0 {
        let prev = u.parent;
        u.parent = mem::rp(phwnd);
        mem::wp(phwnd, prev);
    }
    if base != INSTALLUILEVEL_NOCHANGE {
        u.level = level;
    }
    old
}

fn set_external(c: &mut Ctx, wide: bool) -> u32 {
    let (f, filter, ctx) = (c.a(0), c.a(1), c.a(2));
    let mut u = UI.lock().unwrap();
    let old = if u.record { 0 } else { u.handler };
    u.handler = f;
    u.filter = filter;
    u.ctx = ctx;
    u.wide = wide;
    u.record = false;
    old
}
fn MsiSetExternalUIA(c: &mut Ctx) -> u32 {
    set_external(c, false)
}
fn MsiSetExternalUIW(c: &mut Ctx) -> u32 {
    set_external(c, true)
}

fn MsiSetExternalUIRecord(c: &mut Ctx) -> u32 {
    let (f, filter, ctx, prev) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut u = UI.lock().unwrap();
    if prev != 0 {
        w32(prev, if u.record { u.handler } else { 0 });
    }
    u.handler = f;
    u.filter = filter;
    u.ctx = ctx;
    u.record = true;
    ERROR_SUCCESS
}

fn MsiProcessMessage(c: &mut Ctx) -> u32 {
    let (h, kind, hr) = (c.a(0), c.a(1), c.a(2));
    let Some(rec) = rec_of(hr) else { return u32::MAX };
    let p = pkg_of(h);
    if h != 0 && p.is_none() {
        return u32::MAX;
    }
    send(c, p.as_ref(), kind & 0xff00_0000, &rec) as u32
}

fn query_state(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return INSTALLSTATE_UNKNOWN as u32 - 1; // INSTALLSTATE_INVALIDARG -2
    }
    let code = arg_str(p, wide);
    if pack_guid(&code).is_empty() {
        return (-2i32) as u32;
    }
    if product_installed(&code) { INSTALLSTATE_DEFAULT as u32 } else { INSTALLSTATE_UNKNOWN as u32 }
}
fn query_feature(c: &mut Ctx, wide: bool) -> u32 {
    let (p, f) = (c.a(0), c.a(1));
    if p == 0 || f == 0 {
        return INSTALLSTATE_INVALIDARG as u32;
    }
    let code = arg_str(p, wide);
    if pack_guid(&code).is_empty() || !code.starts_with('{') {
        return INSTALLSTATE_INVALIDARG as u32;
    }
    feature_state(&code, &arg_str(f, wide)) as u32
}
fn MsiQueryFeatureStateA(c: &mut Ctx) -> u32 {
    query_feature(c, false)
}
fn MsiQueryFeatureStateW(c: &mut Ctx) -> u32 {
    query_feature(c, true)
}

fn MsiQueryProductStateA(c: &mut Ctx) -> u32 {
    query_state(c, false)
}
fn MsiQueryProductStateW(c: &mut Ctx) -> u32 {
    query_state(c, true)
}

fn product_info_api(c: &mut Ctx, wide: bool) -> u32 {
    let (p, a, buf, pcch) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if p == 0 || a == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let code = arg_str(p, wide);
    if pack_guid(&code).is_empty() {
        return ERROR_INVALID_PARAMETER;
    }
    if !product_installed(&code) {
        return ERROR_UNKNOWN_PRODUCT;
    }
    match product_info(&code, &arg_str(a, wide)) {
        Some(v) => out_str(buf, pcch, &v, wide),
        None => ERROR_UNKNOWN_PROPERTY,
    }
}
fn MsiGetProductInfoA(c: &mut Ctx) -> u32 {
    product_info_api(c, false)
}
fn MsiGetProductInfoW(c: &mut Ctx) -> u32 {
    product_info_api(c, true)
}

fn products() -> Vec<String> {
    crate::misc::reg_subkeys(PRODUCTS).into_iter().map(|k| unpack_guid(&k.to_uppercase())).filter(|g| !g.is_empty()).collect()
}

fn enum_products(c: &mut Ctx, wide: bool) -> u32 {
    let (i, out) = (c.a(0) as usize, c.a(1));
    if out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match products().get(i) {
        Some(g) => {
            let mut n = 39u32;
            let tmp = mem::heap_alloc(4, false);
            w32(tmp, n);
            out_str(out, tmp, g, wide);
            n = r32(tmp);
            let _ = n;
            mem::heap_free(tmp);
            ERROR_SUCCESS
        }
        None => ERROR_NO_MORE_ITEMS,
    }
}
fn MsiEnumProductsA(c: &mut Ctx) -> u32 {
    enum_products(c, false)
}
fn MsiEnumProductsW(c: &mut Ctx) -> u32 {
    enum_products(c, true)
}

fn enum_related(c: &mut Ctx, wide: bool) -> u32 {
    let (u, i, out) = (c.a(0), c.a(2) as usize, c.a(3));
    if u == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let up = arg_str(u, wide);
    if pack_guid(&up).is_empty() {
        return ERROR_INVALID_PARAMETER;
    }
    match related_products(&up).get(i) {
        Some(g) => {
            let tmp = mem::heap_alloc(4, false);
            w32(tmp, 39);
            out_str(out, tmp, g, wide);
            mem::heap_free(tmp);
            ERROR_SUCCESS
        }
        None => ERROR_NO_MORE_ITEMS,
    }
}
fn MsiEnumRelatedProductsA(c: &mut Ctx) -> u32 {
    enum_related(c, false)
}
fn MsiEnumRelatedProductsW(c: &mut Ctx) -> u32 {
    enum_related(c, true)
}

fn open_product(c: &mut Ctx, wide: bool) -> u32 {
    let (p, out) = (c.a(0), c.a(1));
    if p == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let code = arg_str(p, wide);
    if pack_guid(&code).is_empty() {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(local) = product_info(&code, "LocalPackage") else { return ERROR_UNKNOWN_PRODUCT };
    match open_package(&local) {
        Ok(pk) => {
            w32(out, new_handle(Obj::Pkg(pk)));
            ERROR_SUCCESS
        }
        Err(_) => ERROR_INSTALL_PACKAGE_OPEN_FAILED,
    }
}
fn MsiOpenProductA(c: &mut Ctx) -> u32 {
    open_product(c, false)
}
fn MsiOpenProductW(c: &mut Ctx) -> u32 {
    open_product(c, true)
}

fn product_property(c: &mut Ctx, wide: bool) -> u32 {
    let (h, n, buf, pcch) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    if n == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let name = arg_str(n, wide);
    let rows = crate::msi_pkg::query(&p.lock().unwrap().db, &format!("SELECT `Value` FROM `Property` WHERE `Property` = '{}'", name.replace('\'', "")));
    let v = rows.first().map(|r| r.get(1).text()).unwrap_or_default();
    out_str(buf, pcch, &v, wide)
}
fn MsiGetProductPropertyA(c: &mut Ctx) -> u32 {
    product_property(c, false)
}
fn MsiGetProductPropertyW(c: &mut Ctx) -> u32 {
    product_property(c, true)
}

fn source_list_enum(c: &mut Ctx, wide: bool) -> u32 {
    // (code, user sid, context, options, index, buf, pcch)
    let (p, i, buf, pcch) = (c.a(0), c.a(4), c.a(5), c.a(6));
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let code = arg_str(p, wide);
    if !product_installed(&code) {
        return ERROR_UNKNOWN_PRODUCT;
    }
    if i != 0 {
        return ERROR_NO_MORE_ITEMS;
    }
    let src = product_info(&code, "InstallSource").unwrap_or_default();
    out_str(buf, pcch, &src, wide)
}
fn MsiSourceListEnumSourcesA(c: &mut Ctx) -> u32 {
    source_list_enum(c, false)
}
fn MsiSourceListEnumSourcesW(c: &mut Ctx) -> u32 {
    source_list_enum(c, true)
}

/// A GUID in a descriptor: 20 characters, four little-endian dwords in base 85.
/// A GUID ("{...}") in base 85 (20 characters), as descriptors and feature registrations
/// write them.
pub fn encode_guid85(g: &str) -> Option<String> {
    const A: &[u8] = b"!$%&'()*+,-.0123456789=?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{}~";
    let h: Vec<u8> = g.bytes().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 32 {
        return None;
    }
    let hex = |s: &[u8]| u64::from_str_radix(std::str::from_utf8(s).unwrap(), 16).unwrap();
    let mut b = Vec::with_capacity(16);
    b.extend((hex(&h[0..8]) as u32).to_le_bytes());
    b.extend((hex(&h[8..12]) as u16).to_le_bytes());
    b.extend((hex(&h[12..16]) as u16).to_le_bytes());
    for i in 0..8 {
        b.push(hex(&h[16 + 2 * i..18 + 2 * i]) as u8);
    }
    let mut out = String::new();
    for k in 0..4 {
        let mut x = u32::from_le_bytes(b[4 * k..4 * k + 4].try_into().unwrap()) as u64;
        for _ in 0..5 {
            out.push(A[(x % 85) as usize] as char);
            x /= 85;
        }
    }
    Some(out)
}

pub fn decode_guid85(s: &[char]) -> Option<String> {
    const A: &str = "!$%&'()*+,-.0123456789=?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{}~";
    if s.len() < 20 {
        return None;
    }
    let mut d = [0u32; 4];
    for (k, v) in d.iter_mut().enumerate() {
        let mut x: u64 = 0;
        for i in (0..5).rev() {
            x = x * 85 + A.find(s[5 * k + i])? as u64;
        }
        *v = u32::try_from(x).ok()?;
    }
    let b: Vec<u8> = d.iter().flat_map(|v| v.to_le_bytes()).collect();
    Some(format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        d[0], u16::from_le_bytes([b[4], b[5]]), u16::from_le_bytes([b[6], b[7]]), b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

/// MsiDecomposeDescriptor's parts: (product, feature, component, characters used). A
/// descriptor is the product's GUID (base 85), the feature, then '>' and the component's
/// GUID, or '<' for none.
pub fn decompose_descriptor(d: &str) -> Option<(String, String, String, usize)> {
    let c: Vec<char> = d.chars().collect();
    let prod = decode_guid85(&c)?;
    let end = c[20..].iter().position(|&x| x == '>' || x == '<')? + 20;
    let feature: String = c[20..end].iter().collect();
    if feature.chars().count() > 38 {
        return None;
    }
    if c[end] == '<' {
        return Some((prod, feature, String::new(), end + 1));
    }
    let comp = decode_guid85(&c[end + 1..])?;
    Some((prod, feature, comp, end + 21))
}

fn decompose_api(c: &mut Ctx, wide: bool) -> u32 {
    let (d, prod, feat, comp, len) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if d == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some((p, f, k, n)) = decompose_descriptor(&arg_str(d, wide)) else { return ERROR_INVALID_PARAMETER };
    for (buf, v) in [(prod, &p), (feat, &f), (comp, &k)] {
        if buf != 0 {
            let s = guest_str(v, wide);
            let bytes = (v.chars().count() as u32 + 1) * if wide { 2 } else { 1 };
            mem::copy(buf, s, bytes);
            mem::heap_free(s);
        }
    }
    if len != 0 {
        w32(len, n as u32);
    }
    ERROR_SUCCESS
}
/// MsiDatabaseApplyTransform(db, transform file or ":storage", error conditions)
fn db_apply_transform(c: &mut Ctx, wide: bool) -> u32 {
    let (h, t) = (c.a(0), c.a(1));
    let Some(db) = crate::msi::db_of(h) else { return ERROR_INVALID_HANDLE };
    if t == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let t = arg_str(t, wide);
    let full = if t.starts_with(':') { t.clone() } else { crate::files::full_path(&t) };
    match apply_transform(&db, "", &full) {
        ERROR_OPEN_FAILED => ERROR_OPEN_FAILED,
        ERROR_SUCCESS => ERROR_SUCCESS,
        _ => ERROR_INSTALL_TRANSFORM_FAILURE,
    }
}
fn MsiDatabaseApplyTransformA(c: &mut Ctx) -> u32 {
    db_apply_transform(c, false)
}
fn MsiDatabaseApplyTransformW(c: &mut Ctx) -> u32 {
    db_apply_transform(c, true)
}

fn MsiDecomposeDescriptorA(c: &mut Ctx) -> u32 {
    decompose_api(c, false)
}
fn MsiDecomposeDescriptorW(c: &mut Ctx) -> u32 {
    decompose_api(c, true)
}

fn MsiGetLastErrorRecord(_c: &mut Ctx) -> u32 {
    0
}

fn MsiEnableLogA(_c: &mut Ctx) -> u32 {
    ERROR_SUCCESS
}
fn MsiEnableLogW(_c: &mut Ctx) -> u32 {
    ERROR_SUCCESS
}

fn verify_package(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    match crate::msi::open_db(&arg_str(p, wide), crate::msi::MSIDBOPEN_READONLY, None) {
        Ok(_) => ERROR_SUCCESS,
        Err(_) => ERROR_INSTALL_PACKAGE_INVALID,
    }
}
fn MsiVerifyPackageA(c: &mut Ctx) -> u32 {
    verify_package(c, false)
}
fn MsiVerifyPackageW(c: &mut Ctx) -> u32 {
    verify_package(c, true)
}

pub fn init() {
    macro_rules! reg { ($($f:ident),* $(,)?) => { $( implement("msi", stringify!($f), $f); )* }; }
    reg!(
        MsiInstallProductA, MsiInstallProductW, MsiQueryFeatureStateA, MsiQueryFeatureStateW, MsiDecomposeDescriptorA, MsiDecomposeDescriptorW, MsiDatabaseApplyTransformA, MsiDatabaseApplyTransformW, MsiDoActionA, MsiDoActionW, MsiSequenceA, MsiSequenceW, MsiSetInternalUI,
        MsiSetExternalUIA, MsiSetExternalUIW, MsiSetExternalUIRecord, MsiProcessMessage, MsiQueryProductStateA, MsiQueryProductStateW,
        MsiGetProductInfoA, MsiGetProductInfoW, MsiEnumProductsA, MsiEnumProductsW, MsiEnumRelatedProductsA, MsiEnumRelatedProductsW,
        MsiOpenProductA, MsiOpenProductW, MsiGetProductPropertyA, MsiGetProductPropertyW, MsiSourceListEnumSourcesA,
        MsiSourceListEnumSourcesW, MsiGetLastErrorRecord, MsiEnableLogA, MsiEnableLogW, MsiVerifyPackageA, MsiVerifyPackageW,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors() {
        // (Wine's db tests' descriptors)
        let d = "']gAVn-}f(ZXfeAR6.jiFollowTheWhiteRabbit>3w2x^IGfe?CxI5heAvk.";
        assert_eq!(
            decompose_descriptor(d),
            Some(("{90110409-6000-11D3-8CFE-0150048383C9}".into(), "FollowTheWhiteRabbit".into(), "{A7CD68DB-EF74-49C8-FBB2-A7C463B2AC24}".into(), d.len()))
        );
        assert_eq!(
            decompose_descriptor("yh1BVN)8A$!!!!!MKKSkAlwaysInstalledIntl_1033<"),
            Some(("{90150000-006E-0409-0000-0000000FF1CE}".into(), "AlwaysInstalledIntl_1033".into(), String::new(), 45))
        );
        assert_eq!(decompose_descriptor("']gAVn-}f(ZXfeAR6.jiThisWillFailIfTheresMoreThanAGuidsChars>3w2x^IGfe?CxI5heAvk."), None);
        assert_eq!(decompose_descriptor("short"), None);
    }

    #[test]
    fn guid85_known() {
        // (the descriptors' GUIDs above, encoded back)
        assert_eq!(encode_guid85("{90110409-6000-11D3-8CFE-0150048383C9}").as_deref(), Some("']gAVn-}f(ZXfeAR6.ji"));
        assert_eq!(encode_guid85("{A7CD68DB-EF74-49C8-FBB2-A7C463B2AC24}").as_deref(), Some("3w2x^IGfe?CxI5heAvk."));
        assert_eq!(encode_guid85("not a guid"), None);
    }

    proptest::proptest! {
        /// Any GUID survives base 85 and back.
        #[test]
        fn guid85_round_trip(b in proptest::array::uniform16(0u8..)) {
            let g = format!(
                "{{{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
            );
            let e = encode_guid85(&g).unwrap();
            proptest::prop_assert_eq!(e.chars().count(), 20);
            let c: Vec<char> = e.chars().collect();
            proptest::prop_assert_eq!(decode_guid85(&c), Some(g));
        }
    }

    #[test]
    fn guids() {
        let g = "{12345678-9ABC-DEF0-1234-56789ABCDEF0}";
        assert_eq!(pack_guid(g), "87654321CBA90FED21436587A9CBED0F");
        assert_eq!(unpack_guid(&pack_guid(g)), g);
    }

    #[test]
    fn registry_values() {
        assert_eq!(reg_value("#16"), (4, vec![16, 0, 0, 0]));
        assert_eq!(reg_value("#-1"), (4, vec![255; 4]));
        assert_eq!(reg_value("#x0aFF"), (3, vec![10, 255]));
        assert_eq!(reg_value("#%a").0, 2);
        assert_eq!(reg_value("##x"), (1, wide_bytes("#x")));
        assert_eq!(reg_value("a\0b"), (7, [wide_bytes("a"), wide_bytes("b"), vec![0, 0]].concat()));
    }

    #[test]
    fn command_line() {
        assert_eq!(parse_props(r#"INSTALLDIR="C:\Game Dir\" ALLUSERS=1 /qb"#), [("INSTALLDIR".to_string(), "C:\\Game Dir\\".to_string()), ("ALLUSERS".into(), "1".into())]);
    }
}
