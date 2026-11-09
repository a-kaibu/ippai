#![allow(non_snake_case)]
//! Windows Installer packages: MsiOpenPackage, properties, conditions, formatting with the
//! package's properties, directories, features and components. The sequences and actions
//! that install a product are msi_install.rs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use msidb::record::{Field, Record};
use msidb::suminfo::{Prop, SummaryInfo};
use msidb::*;

use crate::api::{implement, Ctx};
use crate::k32::arg_str;
use crate::mem::w32;
use crate::msi::{db_of, new_handle, out_str, rec_of, Db, Obj};

pub const INSTALLSTATE_INVALIDARG: i32 = -2;
pub const INSTALLSTATE_UNKNOWN: i32 = -1;
pub const INSTALLSTATE_ADVERTISED: i32 = 1;
pub const INSTALLSTATE_ABSENT: i32 = 2;
pub const INSTALLSTATE_LOCAL: i32 = 3;
pub const INSTALLSTATE_SOURCE: i32 = 4;
pub const INSTALLSTATE_DEFAULT: i32 = 5;

#[derive(Clone, Debug, Default)]
pub struct Dir {
    pub parent: String,
    pub target_name: String,
    pub source_name: String,
    /// resolved, ending in a backslash
    pub target: String,
    pub source: String,
}

#[derive(Clone, Debug, Default)]
pub struct Feature {
    pub name: String,
    pub parent: String,
    pub level: i32,
    pub attrs: i32,
    pub installed: i32,
    /// the action requested (what MsiGetFeatureState answers)
    pub action: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Component {
    pub name: String,
    pub id: String,
    pub dir: String,
    pub attrs: i32,
    pub cond: String,
    pub keypath: String,
    pub installed: i32,
    pub action: i32,
    /// its Condition is not false
    pub enabled: bool,
    /// it has a compressed file: it cannot run from source
    pub force_local: bool,
    /// products registered as using it
    pub clients: i32,
}

#[derive(Clone, Debug, Default)]
pub struct FileRec {
    pub key: String,
    pub comp: String,
    pub name: String,
    pub source_name: String,
    pub size: u32,
    pub attrs: i32,
    pub seq: i32,
}

pub struct Package {
    pub db: Db,
    pub props: HashMap<String, String>,
    pub dirs: HashMap<String, Dir>,
    pub features: Vec<Feature>,
    pub comps: HashMap<String, Component>,
    pub files: Vec<FileRec>,
    pub summary: SummaryInfo,
    pub path: String,
    /// MSIRUNMODE bits MsiGetMode answers
    pub mode: u32,
    pub ui_level: u32,
    pub costed: bool,
    /// the action running (MsiGetProperty of "CustomActionData" etc.)
    pub action: String,
    /// the execute sequence ran (ExecuteAction, or the sequence itself)
    pub executed: bool,
    /// the package's folder (SourceDir once CostInitialize or ResolveSource ran)
    pub pkg_dir: String,
    /// MsiGetSourcePath's answers, fixed at CostInitialize / ResolveSource
    pub sources: Option<HashMap<String, String>>,
    /// CostInitialize ran: features and components are known
    pub cost_started: bool,
    /// FeatureComponents: (feature, component)
    pub uses: Vec<(String, String)>,
}

/// The package handle's package.
pub fn pkg_of(h: u32) -> Option<Arc<Mutex<Package>>> {
    let o = crate::msi::get(h)?;
    let o = o.lock().unwrap();
    match &*o {
        Obj::Pkg(p) => Some(p.clone()),
        _ => None,
    }
}

fn int(f: &Field) -> i32 {
    f.int()
}

/// Rows of `sql` as records (no table: none).
pub fn query(db: &Db, sql: &str) -> Vec<Record> {
    let mut d = db.lock().unwrap();
    let Ok(mut v) = View::new(&d.db, sql) else { return Vec::new() };
    if v.execute(&mut d.db, None).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    while let Ok(r) = v.fetch(&d.db) {
        out.push(r);
    }
    out
}

/// The long name of a "short|long" file or directory name.
pub fn long_name(s: &str) -> &str {
    s.rsplit_once('|').map_or(s, |x| x.1)
}

/// A path as MsiSetTargetPath keeps it: the spaces around each part and empty parts
/// dropped, ending in a backslash.
pub fn clean_path(p: &str) -> String {
    let unc = p.starts_with("\\\\");
    let parts: Vec<&str> = p.split('\\').map(str::trim).filter(|x| !x.is_empty()).collect();
    format!("{}{}\\", if unc { "\\\\" } else { "" }, parts.join("\\"))
}

fn slash(s: &str) -> String {
    if s.is_empty() || s.ends_with('\\') { s.to_string() } else { format!("{s}\\") }
}

/// The properties Windows Installer sets before the Property table's.
fn system_props(p: &mut HashMap<String, String>) {
    let v = crate::k32::OS_VERSION_REAL;
    let sf = |c: u32| slash(crate::misc::special_folder(c).unwrap_or("C:\\"));
    let set = [
        ("VersionNT", "603".to_string()),
        ("WindowsBuild", "9600".into()),
        ("ServicePackLevel", "0".into()),
        ("ServicePackLevelMinor", "0".into()),
        ("VersionMsi", "5.00".into()),
        ("Intel", "6".into()),
        ("MsiNTProductType", "1".into()),
        ("Privileged", "1".into()),
        ("AdminUser", "1".into()),
        ("MsiTrueAdminUser", "1".into()),
        ("WindowsFolder", "C:\\windows\\".into()),
        ("WindowsVolume", "C:\\".into()),
        ("ROOTDRIVE", "C:\\".into()),
        ("SystemFolder", "C:\\windows\\system32\\".into()),
        ("System16Folder", "C:\\windows\\system\\".into()),
        ("TempFolder", "C:\\windows\\temp\\".into()),
        ("FontsFolder", "C:\\windows\\Fonts\\".into()),
        ("ProgramFilesFolder", "C:\\Program Files\\".into()),
        ("CommonFilesFolder", "C:\\Program Files\\Common Files\\".into()),
        ("CommonAppDataFolder", "C:\\ProgramData\\".into()),
        ("AppDataFolder", sf(0x1a)),
        ("LocalAppDataFolder", sf(0x1c)),
        ("PersonalFolder", sf(5)),
        ("DesktopFolder", sf(0x10)),
        ("ProgramMenuFolder", format!("{}Start Menu\\Programs\\", sf(2))),
        ("StartMenuFolder", format!("{}Start Menu\\", sf(2))),
        ("StartupFolder", format!("{}Start Menu\\Programs\\Startup\\", sf(2))),
        ("SendToFolder", format!("{}SendTo\\", sf(2))),
        ("FavoritesFolder", format!("{}Favorites\\", sf(0x28))),
        ("MyPicturesFolder", sf(0x27)),
        ("NetHoodFolder", format!("{}Network Shortcuts\\", sf(2))),
        ("PrintHoodFolder", format!("{}Printer Shortcuts\\", sf(2))),
        ("RecentFolder", format!("{}Recent\\", sf(2))),
        ("TemplateFolder", format!("{}Templates\\", sf(2))),
        ("AdminToolsFolder", format!("{}Start Menu\\Programs\\Administrative Tools\\", sf(2))),
        ("ComputerName", "SAKE".into()),
        ("LogonUser", "user".into()),
        ("UserLanguageID", "1041".into()),
        ("SystemLanguageID", "1041".into()),
        ("ScreenX", "1920".into()),
        ("ScreenY", "1080".into()),
        ("ColorBits", "32".into()),
        ("TTCSupport", "1".into()),
        ("PhysicalMemory", "4096".into()),
        ("VirtualMemory", "4096".into()),
        ("MsiWin32AssemblySupport", "6.1.7601.17514".into()),
        ("RedirectedDllSupport", "2".into()),
        ("Time", "00:00:00".into()),
        ("Date", "2026/01/01".into()),
        ("UserSID", "S-1-5-21-0-0-0-1000".into()),
    ];
    let _ = v;
    for (k, val) in set {
        p.insert(k.to_string(), val);
    }
}

impl Package {
    pub fn open(db: Db, path: &str) -> Package {
        {
            let mut o = db.lock().unwrap();
            if o.db.table_index("_Property").is_none() {
                let cols = vec![
                    msidb::Col { name: "Property".into(), ty: 0x2d48 | msidb::db::T_TEMPORARY },
                    msidb::Col { name: "Value".into(), ty: 0x0d00 | msidb::db::T_NULLABLE | msidb::db::T_TEMPORARY },
                ];
                let _ = o.db.create_table("_Property", cols, false);
            }
        }
        let summary = db.lock().unwrap().db.summary.as_deref().map(SummaryInfo::parse).unwrap_or_default();
        let mut props = HashMap::new();
        system_props(&mut props);
        for r in query(&db, "SELECT `Property`, `Value` FROM `Property`") {
            props.insert(r.get(1).text(), r.get(2).text());
        }
        // (the package's folder: a relative path is taken from the current directory)
        // ("#handle": the database's file)
        let file = if path.starts_with('#') { db.lock().unwrap().path.clone() } else { path.to_string() };
        let full = if file.is_empty() { String::new() } else { crate::files::full_path(&file) };
        let dir = full.rsplit_once('\\').map_or(String::new(), |d| slash(d.0));
        props.insert("DATABASE".into(), path.to_string());
        props.insert("OriginalDatabase".into(), path.to_string());
        if let Some(Prop::Str(code)) = summary.props.get(&msidb::suminfo::PID_REVNUMBER) {
            props.insert("PackageCode".into(), code.clone());
        }
        if let Some(Prop::I4(v)) = summary.props.get(&msidb::suminfo::PID_PAGECOUNT) {
            props.insert("VersionDatabase".into(), v.to_string());
        }
        // (a product registered already: Installed, ProductState 5, ProductToBeRegistered)
        let code = props.get("ProductCode").cloned().unwrap_or_default();
        if crate::msi_install::product_installed(&code) {
            props.insert("Installed".into(), "1".into());
            props.insert("ProductState".into(), "5".into());
            props.insert("ProductToBeRegistered".into(), "1".into());
        } else {
            props.insert("ProductState".into(), "-1".into());
        }
        props.entry("UILevel".into()).or_insert("5".into());
        let mut p = Package {
            db,
            props,
            dirs: HashMap::new(),
            features: Vec::new(),
            comps: HashMap::new(),
            files: Vec::new(),
            summary,
            path: path.to_string(),
            mode: 0,
            ui_level: 5,
            costed: false,
            action: String::new(),
            executed: false,
            pkg_dir: dir,
            sources: None,
            cost_started: false,
            uses: Vec::new(),
        };
        p.load_tables();
        p.sync_props();
        p
    }

    fn load_tables(&mut self) {
        // (msidbSumInfoSourceTypeSFN: the source tree has the short names)
        let sfn = matches!(self.summary.props.get(&msidb::suminfo::PID_WORDCOUNT), Some(Prop::I4(v)) if v & 1 != 0);
        for r in query(&self.db, "SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`") {
            let def = r.get(3).text();
            let (t, s) = match def.split_once(':') {
                Some((t, s)) => (t.to_string(), s.to_string()),
                None => (def.clone(), def.clone()),
            };
            self.dirs.insert(r.get(1).text(), Dir { parent: r.get(2).text(), target_name: long_name(&t).into(), source_name: if sfn { s.split('|').next().unwrap_or("").into() } else { long_name(&s).into() }, ..Dir::default() });
        }
        for r in query(&self.db, "SELECT `Feature`, `Feature_Parent`, `Level`, `Attributes` FROM `Feature`") {
            self.features.push(Feature { name: r.get(1).text(), parent: r.get(2).text(), level: int(r.get(3)).max(0), attrs: int(r.get(4)).max(0), installed: INSTALLSTATE_UNKNOWN, action: INSTALLSTATE_UNKNOWN });
        }
        for r in query(&self.db, "SELECT `Component`, `ComponentId`, `Directory_`, `Attributes`, `Condition`, `KeyPath` FROM `Component`") {
            let name = r.get(1).text();
            self.comps.insert(name.clone(), Component { name, id: r.get(2).text(), dir: r.get(3).text(), attrs: int(r.get(4)).max(0), cond: r.get(5).text(), keypath: r.get(6).text(), installed: INSTALLSTATE_UNKNOWN, action: INSTALLSTATE_UNKNOWN, enabled: true, force_local: false, clients: 0 });
        }
        for r in query(&self.db, "SELECT `File`, `Component_`, `FileName`, `FileSize`, `Attributes`, `Sequence` FROM `File`") {
            let n = r.get(3).text();
            let (short, long) = n.split_once('|').map_or((n.clone(), n.clone()), |(a, b)| (a.to_string(), b.to_string()));
            self.files.push(FileRec { key: r.get(1).text(), comp: r.get(2).text(), name: long, source_name: short, size: int(r.get(4)).max(0) as u32, attrs: int(r.get(5)).max(0), seq: int(r.get(6)) });
        }
        self.files.sort_by_key(|f| f.seq);
        self.uses = query(&self.db, "SELECT `Feature_`, `Component_` FROM `FeatureComponents`").iter().map(|r| (r.get(1).text(), r.get(2).text())).collect();
    }

    pub fn prop(&self, name: &str) -> String {
        self.props.get(name).cloned().unwrap_or_default()
    }

    pub fn set_prop(&mut self, name: &str, v: &str) {
        if v.is_empty() {
            self.props.remove(name);
        } else {
            self.props.insert(name.to_string(), v.to_string());
        }
        self.sync_prop(name);
        // (a directory's property sets its target, as Windows' costing reads them back)
        if self.costed && self.dirs.contains_key(name) && !v.is_empty() {
            self.set_target(name, v);
        }
    }

    /// The active database's temporary _Property table, where Windows keeps a package's
    /// properties (a SELECT on it sees them): `name`'s row as the property is now.
    pub fn sync_prop(&self, name: &str) {
        let mut o = self.db.lock().unwrap();
        let db = &mut o.db;
        let Some(ti) = db.table_index("_Property") else { return };
        if let Some(ri) = db.row_by_keys(ti, &[0], &[Field::Str(name.to_string())]) {
            db.delete(ti, ri);
        }
        if let Some(v) = self.props.get(name) {
            let _ = db.insert(ti, &[Field::Str(name.to_string()), Field::Str(v.clone())], true);
        }
    }

    /// Every property into _Property (after actions that set many).
    pub fn sync_props(&self) {
        let names: Vec<String> = self.props.keys().cloned().collect();
        for n in names {
            self.sync_prop(&n);
        }
    }

    /// The target path of every directory, from TARGETDIR down (CostFinalize).
    pub fn resolve_dirs(&mut self) {
        let names: Vec<String> = self.dirs.keys().cloned().collect();
        let mut done: HashMap<String, (String, String)> = HashMap::new();
        for n in &names {
            self.resolve(n, &mut done, 0);
        }
        for (n, (t, s)) in done {
            if let Some(d) = self.dirs.get_mut(&n) {
                d.target = t.clone();
                d.source = s;
            }
            self.props.insert(n, t);
        }
    }

    fn resolve(&self, n: &str, done: &mut HashMap<String, (String, String)>, depth: u32) -> (String, String) {
        if let Some(v) = done.get(n) {
            return v.clone();
        }
        let src_root = slash(&Some(self.prop("SourceDir")).filter(|v| !v.is_empty()).unwrap_or_else(|| self.pkg_dir.clone()));
        let Some(d) = self.dirs.get(n).cloned() else { return (slash(&self.prop("ROOTDRIVE")), src_root) };
        let own = self.props.get(n).filter(|v| !v.is_empty()).cloned();
        let (pt, ps) = if d.parent.is_empty() || d.parent == n || depth > 64 {
            (slash(&self.props.get("TARGETDIR").cloned().filter(|v| !v.is_empty()).unwrap_or_else(|| self.prop("ROOTDRIVE"))), src_root.clone())
        } else {
            self.resolve(&d.parent, done, depth + 1)
        };
        let add = |base: &str, name: &str| if name == "." || name.is_empty() { base.to_string() } else { slash(&format!("{base}{name}")) };
        let target = match own {
            // (a root's own property, or one set from outside, wins)
            Some(v) if d.parent.is_empty() || n == "TARGETDIR" || !self.is_system_folder(n) || true => slash(&v),
            _ => if d.parent.is_empty() { pt.clone() } else { add(&pt, &d.target_name) },
        };
        let source = if d.parent.is_empty() { ps } else { add(&ps, &d.source_name) };
        done.insert(n.to_string(), (target.clone(), source.clone()));
        (target, source)
    }

    /// CostInitialize / ResolveSource: SourceDir is the package's folder, and each
    /// directory's source is fixed (a compressed package's are all the root).
    pub fn resolve_sources(&mut self) {
        if self.prop("SourceDir").is_empty() {
            self.props.insert("SourceDir".into(), self.pkg_dir.clone());
        }
        if self.prop("SOURCEDIR").is_empty() {
            let v = self.prop("SourceDir");
            self.props.insert("SOURCEDIR".into(), v);
        }
        let root = slash(&self.prop("SourceDir"));
        let compressed = matches!(self.summary.props.get(&msidb::suminfo::PID_WORDCOUNT), Some(Prop::I4(v)) if v & 2 != 0);
        let mut out = HashMap::new();
        let names: Vec<String> = self.dirs.keys().cloned().collect();
        for n in &names {
            let v = if compressed { root.clone() } else { self.source_of(n, &root, 0) };
            out.insert(n.clone(), v);
        }
        out.entry("SourceDir".into()).or_insert(root.clone());
        if compressed {
            out.entry("SOURCEDIR".into()).or_insert(root);
        }
        self.sources = Some(out);
        self.sync_prop("SourceDir");
        self.sync_prop("SOURCEDIR");
    }

    fn source_of(&self, n: &str, root: &str, depth: u32) -> String {
        let Some(d) = self.dirs.get(n) else { return root.to_string() };
        if d.parent.is_empty() || d.parent == n || depth > 64 {
            return root.to_string();
        }
        let base = self.source_of(&d.parent, root, depth + 1);
        if d.source_name == "." || d.source_name.is_empty() { base } else { slash(&format!("{base}{}", d.source_name)) }
    }

    fn is_system_folder(&self, _n: &str) -> bool {
        false
    }

    /// MsiSetTargetPath / a type 35 custom action: the directory and those under it move.
    pub fn set_target(&mut self, n: &str, path: &str) {
        let path = clean_path(path);
        self.props.insert(n.to_string(), path.clone());
        let old = self.dirs.get(n).map(|d| d.target.clone()).unwrap_or_default();
        if old.is_empty() {
            return;
        }
        let kids: Vec<(String, String)> = self.dirs.iter().filter(|(k, d)| k.as_str() != n && d.target.starts_with(&old)).map(|(k, d)| (k.clone(), format!("{path}{}", &d.target[old.len()..]))).collect();
        if let Some(d) = self.dirs.get_mut(n) {
            d.target = path;
        }
        for (k, t) in kids {
            self.props.insert(k.clone(), t.clone());
            self.dirs.get_mut(&k).unwrap().target = t;
        }
    }

    pub fn target(&self, dir: &str) -> String {
        self.dirs.get(dir).map(|d| d.target.clone()).unwrap_or_default()
    }

    pub fn file_target(&self, f: &FileRec) -> String {
        let dir = self.comps.get(&f.comp).map(|c| self.target(&c.dir)).unwrap_or_default();
        format!("{dir}{}", f.name)
    }

    /// Feature and component states for a first install (CostFinalize): features at or
    /// under INSTALLLEVEL (or named by ADDLOCAL / REMOVE) and the components they use.
    /// A file comes out of a cabinet (msidbFileAttributesCompressed 0x4000, Noncompressed
    /// 0x2000, else the package's word count bit 2).
    pub fn file_compressed(&self, f: &FileRec) -> bool {
        let wc = match self.summary.props.get(&msidb::suminfo::PID_WORDCOUNT) {
            Some(Prop::I4(v)) => *v,
            _ => 0,
        };
        if f.attrs & 0x4000 != 0 { true } else if f.attrs & 0x2000 != 0 { false } else { wc & 2 != 0 }
    }

    fn feature_comps(&self, f: &str) -> Vec<String> {
        self.uses.iter().filter(|u| u.0 == f).map(|u| u.1.clone()).collect()
    }

    /// CostFinalize: the installed states (the registration), the states asked for
    /// (ADDLOCAL, REMOVE, ADDSOURCE, REINSTALL, ADVERTISE, in that order, else INSTALLLEVEL
    /// and the features' favoured states) and the components' from their features'.
    pub fn plan_states(&mut self) {
        let code = self.prop("ProductCode");
        for n in self.comps.keys().cloned().collect::<Vec<_>>() {
            let c = self.comps.get(&n).unwrap().clone();
            let enabled = c.cond.is_empty() || !matches!(evaluate(self, &c.cond), cond::Cond::False);
            let (installed, clients) = if c.id.is_empty() { (INSTALLSTATE_UNKNOWN, 0) } else { crate::msi_install::component_registration(&code, &c.id) };
            let c = self.comps.get_mut(&n).unwrap();
            c.enabled = enabled;
            c.installed = installed;
            c.clients = clients;
        }
        for i in 0..self.features.len() {
            let st = crate::msi_install::feature_state(&code, &self.features[i].name);
            self.features[i].installed = if st == INSTALLSTATE_UNKNOWN || st == INSTALLSTATE_INVALIDARG { INSTALLSTATE_ABSENT } else { st };
        }
        let mut preselected = false;
        for (prop, state) in [("ADDLOCAL", INSTALLSTATE_LOCAL), ("REMOVE", INSTALLSTATE_ABSENT), ("ADDSOURCE", INSTALLSTATE_SOURCE), ("REINSTALL", INSTALLSTATE_UNKNOWN), ("ADVERTISE", INSTALLSTATE_ADVERTISED)] {
            let v = self.prop(prop);
            if v.is_empty() {
                continue;
            }
            preselected = true;
            let all = v.eq_ignore_ascii_case("ALL");
            let names: Vec<&str> = v.split(',').collect();
            for f in self.features.iter_mut().filter(|f| f.level > 0) {
                if !all && !names.contains(&f.name.as_str()) {
                    continue;
                }
                f.action = match prop {
                    "REINSTALL" if f.installed == INSTALLSTATE_ABSENT => INSTALLSTATE_UNKNOWN,
                    "REINSTALL" => f.installed,
                    "REMOVE" if f.installed == INSTALLSTATE_ABSENT => INSTALLSTATE_UNKNOWN,
                    _ => state,
                };
            }
        }
        if preselected {
            self.props.insert("Preselected".into(), "1".into());
            self.sync_prop("Preselected");
        } else {
            for r in query(&self.db, "SELECT `Feature_`, `Level`, `Condition` FROM `Condition`") {
                if matches!(evaluate(self, &r.get(3).text()), cond::Cond::True) {
                    let (f, l) = (r.get(1).text(), int(r.get(2)));
                    if let Some(x) = self.features.iter_mut().find(|x| x.name == f) {
                        x.level = l;
                    }
                }
            }
        }
        // (a component with a file in a cabinet is installed, not run from source)
        for f in &self.files {
            if self.file_compressed(f) {
                if let Some(c) = self.comps.get_mut(&f.comp).filter(|c| c.enabled) {
                    c.force_local = true;
                }
            }
        }
        if self.prop("INSTALLLEVEL").is_empty() {
            self.set_prop("INSTALLLEVEL", "1");
        }
        let level: i32 = self.prop("INSTALLLEVEL").trim().parse().unwrap_or(1);
        let selected = |f: &Feature| f.level > 0 && f.level <= level;
        let fresh = self.prop("Installed").is_empty() && self.prop("Preselected").is_empty();
        for f in self.features.iter_mut() {
            if !selected(f) || f.action != INSTALLSTATE_UNKNOWN {
                continue;
            }
            if self.props.contains_key("Preselected") {
                f.action = if f.installed == INSTALLSTATE_ABSENT { INSTALLSTATE_UNKNOWN } else { f.installed };
            } else if fresh {
                f.action = favoured(f.attrs);
            }
        }
        // (features under one not selected are off; FollowParent ones take their parent's)
        for _ in 0..self.features.len().min(64) {
            let mut changed = false;
            for i in 0..self.features.len() {
                let parent = self.features[i].parent.clone();
                if parent.is_empty() || parent == self.features[i].name {
                    continue;
                }
                let Some(p) = self.features.iter().find(|x| x.name == parent).cloned() else { continue };
                let f = &mut self.features[i];
                if !selected(&p) && (f.level != p.level || f.action != INSTALLSTATE_UNKNOWN) {
                    f.level = p.level;
                    f.action = INSTALLSTATE_UNKNOWN;
                    changed = true;
                } else if selected(&p) && f.attrs & 2 != 0 && f.action != p.action {
                    f.action = p.action;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // (a feature with a component that must be local is local)
        for i in 0..self.features.len() {
            if self.features[i].action == INSTALLSTATE_SOURCE && self.feature_comps(&self.features[i].name).iter().any(|c| self.comps.get(c).is_some_and(|c| c.force_local)) {
                self.features[i].action = INSTALLSTATE_LOCAL;
            }
        }
        // what the features using each component want: (local, source, advertised, absent)
        let mut want: HashMap<String, [bool; 4]> = HashMap::new();
        for f in &self.features {
            let st = match f.action {
                INSTALLSTATE_DEFAULT => favoured(f.attrs),
                INSTALLSTATE_UNKNOWN => f.installed,
                a => a,
            };
            let k = match st {
                INSTALLSTATE_LOCAL => 0,
                INSTALLSTATE_SOURCE => 1,
                INSTALLSTATE_ADVERTISED => 2,
                INSTALLSTATE_ABSENT if f.action == INSTALLSTATE_ABSENT => 3,
                _ => continue,
            };
            for c in self.feature_comps(&f.name) {
                want.entry(c).or_default()[k] = true;
            }
        }
        for (n, c) in self.comps.iter_mut() {
            let [local, source, adv, absent] = want.get(n).copied().unwrap_or_default();
            let a = if c.attrs & 2 == 0 && (local || source) {
                if c.attrs & 1 != 0 && !c.force_local { INSTALLSTATE_SOURCE } else { INSTALLSTATE_LOCAL }
            } else if local {
                INSTALLSTATE_LOCAL
            } else if source {
                INSTALLSTATE_SOURCE
            } else if adv {
                INSTALLSTATE_ADVERTISED
            } else if absent && !c.id.is_empty() {
                INSTALLSTATE_ABSENT
            } else {
                c.action
            };
            c.action = if a == INSTALLSTATE_DEFAULT { INSTALLSTATE_LOCAL } else { a };
            if c.action == INSTALLSTATE_SOURCE && c.installed == INSTALLSTATE_SOURCE && source {
                c.action = INSTALLSTATE_UNKNOWN;
            }
            // (another product still uses it: it stays)
            if c.action == INSTALLSTATE_ABSENT && c.clients - 1 > 0 {
                c.action = INSTALLSTATE_UNKNOWN;
            }
        }
    }

    /// MsiSetFeatureState: the feature, its components and the features under it.
    pub fn set_feature_state(&mut self, name: &str, state: i32, depth: u32) {
        let Some(i) = self.features.iter().position(|f| f.name == name) else { return };
        let state = if state == INSTALLSTATE_ADVERTISED && self.features[i].attrs & 8 != 0 { INSTALLSTATE_ABSENT } else { state };
        self.features[i].action = state;
        let new = if state == INSTALLSTATE_ABSENT { INSTALLSTATE_UNKNOWN } else { state };
        for cn in self.feature_comps(name) {
            let Some(c) = self.comps.get(&cn).filter(|c| c.enabled).cloned() else { continue };
            let mut a = new;
            if new != INSTALLSTATE_LOCAL {
                // (another feature that wants it keeps it)
                for f in self.features.iter().filter(|f| f.action == INSTALLSTATE_LOCAL || f.action == INSTALLSTATE_SOURCE) {
                    if self.uses.iter().any(|u| u.0 == f.name && u.1 == cn) {
                        a = if c.attrs & 2 != 0 {
                            if f.attrs & 1 != 0 { INSTALLSTATE_SOURCE } else { INSTALLSTATE_LOCAL }
                        } else if c.attrs & 1 != 0 {
                            INSTALLSTATE_SOURCE
                        } else {
                            INSTALLSTATE_LOCAL
                        };
                    }
                }
            }
            self.comps.get_mut(&cn).unwrap().action = a;
        }
        if depth < 64 {
            let kids: Vec<String> = self.features.iter().filter(|f| f.parent == name && f.name != name).map(|f| f.name.clone()).collect();
            for k in kids {
                self.set_feature_state(&k, state, depth + 1);
            }
        }
    }

    pub fn format(&self, s: &str) -> String {
        let mut r = Record::new(0);
        r.f[0] = Field::Str(s.to_string());
        self.format_record(&r)
    }

    pub fn format_record(&self, r: &Record) -> String {
        format::format_record(r, &FmtEnv(self))
    }
}

struct FmtEnv<'a>(&'a Package);
impl format::Env for FmtEnv<'_> {
    fn has_package(&self) -> bool {
        true
    }
    fn property(&self, n: &str) -> Option<String> {
        Some(self.0.prop(n))
    }
    fn env(&self, n: &str) -> String {
        crate::k32::env_get(n).unwrap_or_default()
    }
    fn file(&self, key: &str, _short: bool) -> Option<String> {
        let f = self.0.files.iter().find(|f| f.key == key)?;
        Some(self.0.file_target(f))
    }
    fn component(&self, key: &str) -> Option<String> {
        let c = self.0.comps.get(key)?;
        // (run from source: the source folder)
        if c.action == INSTALLSTATE_SOURCE {
            return Some(self.0.dirs.get(&c.dir).map(|d| d.source.clone()).unwrap_or_default());
        }
        Some(self.0.target(&c.dir))
    }
}

struct CondEnv<'a>(&'a Package);
impl cond::Env for CondEnv<'_> {
    fn property(&self, n: &str) -> String {
        self.0.prop(n)
    }
    fn env(&self, n: &str) -> String {
        crate::k32::env_get(n).unwrap_or_default()
    }
    fn component(&self, n: &str) -> Option<(i32, i32)> {
        self.0.comps.get(n).map(|c| (c.installed, c.action))
    }
    fn feature(&self, n: &str) -> Option<(i32, i32)> {
        self.0.features.iter().find(|f| f.name == n).map(|f| (f.installed, f.action))
    }
}

/// A feature's state when nothing asks for another (msidbFeatureAttributesFavorSource 1,
/// FavorAdvertise 4).
fn favoured(attrs: i32) -> i32 {
    if attrs & 1 != 0 {
        INSTALLSTATE_SOURCE
    } else if attrs & 4 != 0 {
        INSTALLSTATE_ADVERTISED
    } else {
        INSTALLSTATE_LOCAL
    }
}

pub fn evaluate(p: &Package, c: &str) -> cond::Cond {
    cond::evaluate(c, &CondEnv(p))
}

/// A sequence or table condition: an empty one is true.
pub fn cond_true(p: &Package, c: &str) -> bool {
    matches!(evaluate(p, c), cond::Cond::True | cond::Cond::None)
}

// ---- entry points

/// Opens a package file or "#handle" of an open database.
pub fn open_package(path: &str) -> R<Arc<Mutex<Package>>> {
    let db = if let Some(h) = path.strip_prefix('#') {
        db_of(h.parse().map_err(|_| ERROR_INVALID_HANDLE)?).ok_or(ERROR_INVALID_HANDLE)?
    } else {
        match crate::msi::open_db(path, crate::msi::MSIDBOPEN_READONLY, None) {
            Ok(o) => Arc::new(Mutex::new(o)),
            Err(ERROR_OPEN_FAILED) => return Err(ERROR_INSTALL_PACKAGE_OPEN_FAILED),
            Err(e) => return Err(e),
        }
    };
    // (a database with a _Property table of its own is not a package: that name is the
    // package's properties')
    if db.lock().unwrap().db.table("_Property").is_some_and(|t| t.persistent) {
        return Err(ERROR_INSTALL_PACKAGE_INVALID);
    }
    let full = if path.starts_with('#') { path.to_string() } else { crate::files::full_path(path) };
    Ok(Arc::new(Mutex::new(Package::open(db, &full))))
}

fn open_pkg(c: &mut Ctx, wide: bool, ex: bool) -> u32 {
    let (p, out) = (c.a(0), if ex { c.a(2) } else { c.a(1) });
    if p == 0 || out == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let path = arg_str(p, wide);
    if path.is_empty() {
        return ERROR_INVALID_PARAMETER;
    }
    match open_package(&path) {
        Ok(pk) => {
            w32(out, new_handle(Obj::Pkg(pk)));
            ERROR_SUCCESS
        }
        Err(e) => e,
    }
}
fn MsiOpenPackageA(c: &mut Ctx) -> u32 {
    open_pkg(c, false, false)
}
fn MsiOpenPackageW(c: &mut Ctx) -> u32 {
    open_pkg(c, true, false)
}
fn MsiOpenPackageExA(c: &mut Ctx) -> u32 {
    open_pkg(c, false, true)
}
fn MsiOpenPackageExW(c: &mut Ctx) -> u32 {
    open_pkg(c, true, true)
}

fn get_property(c: &mut Ctx, wide: bool) -> u32 {
    let (h, n, buf, pcch) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if n == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    let v = p.lock().unwrap().prop(&arg_str(n, wide));
    out_str(buf, pcch, &v, wide)
}
fn MsiGetPropertyA(c: &mut Ctx) -> u32 {
    get_property(c, false)
}
fn MsiGetPropertyW(c: &mut Ctx) -> u32 {
    get_property(c, true)
}

fn set_property(c: &mut Ctx, wide: bool) -> u32 {
    let (h, n, v) = (c.a(0), c.a(1), c.a(2));
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    if n == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let name = arg_str(n, wide);
    if name.is_empty() {
        return ERROR_SUCCESS;
    }
    let val = if v == 0 { String::new() } else { arg_str(v, wide) };
    p.lock().unwrap().set_prop(&name, &val);
    ERROR_SUCCESS
}
fn MsiSetPropertyA(c: &mut Ctx) -> u32 {
    set_property(c, false)
}
fn MsiSetPropertyW(c: &mut Ctx) -> u32 {
    set_property(c, true)
}

fn eval_condition(c: &mut Ctx, wide: bool) -> u32 {
    let (h, s) = (c.a(0), c.a(1));
    let Some(p) = pkg_of(h) else { return cond::Cond::Error as u32 };
    if s == 0 {
        return cond::Cond::None as u32;
    }
    let s = arg_str(s, wide);
    let p = p.lock().unwrap();
    evaluate(&p, &s) as u32
}
fn MsiEvaluateConditionA(c: &mut Ctx) -> u32 {
    eval_condition(c, false)
}
fn MsiEvaluateConditionW(c: &mut Ctx) -> u32 {
    eval_condition(c, true)
}

fn MsiGetActiveDatabase(c: &mut Ctx) -> u32 {
    match pkg_of(c.a(0)) {
        Some(p) => {
            let db = p.lock().unwrap().db.clone();
            new_handle(Obj::Db(db))
        }
        None => 0,
    }
}

fn MsiGetLanguage(c: &mut Ctx) -> u32 {
    match pkg_of(c.a(0)) {
        Some(p) => p.lock().unwrap().prop("ProductLanguage").parse().unwrap_or(0),
        None => 0,
    }
}

fn MsiGetMode(c: &mut Ctx) -> u32 {
    let (h, m) = (c.a(0), c.a(1));
    let Some(p) = pkg_of(h) else { return 0 };
    let p = p.lock().unwrap();
    match m {
        // MSIRUNMODE_SOURCESHORTNAMES 9 / TARGETSHORTNAMES 10: as the summary says
        9 => (p.summary.props.get(&msidb::suminfo::PID_WORDCOUNT).is_some_and(|x| matches!(x, Prop::I4(v) if v & 1 != 0))) as u32,
        _ => (p.mode >> m.min(31)) & 1,
    }
}

fn MsiSetMode(c: &mut Ctx) -> u32 {
    let (h, m, on) = (c.a(0), c.a(1), c.a(2) != 0);
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    // (only REBOOTATEND 4 and REBOOTNOW 5 can be set)
    if m != 4 && m != 5 {
        return ERROR_ACCESS_DENIED;
    }
    let mut p = p.lock().unwrap();
    if on {
        p.mode |= 1 << m;
    } else {
        p.mode &= !(1 << m);
    }
    ERROR_SUCCESS
}

fn target_path(c: &mut Ctx, wide: bool, source: bool) -> u32 {
    let (h, d, buf, pcch) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if d == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    let name = arg_str(d, wide);
    let p = p.lock().unwrap();
    if source {
        if pcch == 0 {
            return ERROR_INVALID_PARAMETER;
        }
        let Some(v) = p.sources.as_ref().and_then(|s| s.get(&name)) else { return ERROR_DIRECTORY };
        return out_str(buf, pcch, v, wide);
    }
    let Some(dir) = p.dirs.get(&name) else { return ERROR_DIRECTORY };
    if !p.costed {
        return ERROR_FUNCTION_NOT_CALLED;
    }
    out_str(buf, pcch, &dir.target, wide)
}
fn MsiGetTargetPathA(c: &mut Ctx) -> u32 {
    target_path(c, false, false)
}
fn MsiGetTargetPathW(c: &mut Ctx) -> u32 {
    target_path(c, true, false)
}
fn MsiGetSourcePathA(c: &mut Ctx) -> u32 {
    target_path(c, false, true)
}
fn MsiGetSourcePathW(c: &mut Ctx) -> u32 {
    target_path(c, true, true)
}

fn set_target_path(c: &mut Ctx, wide: bool) -> u32 {
    let (h, d, v) = (c.a(0), c.a(1), c.a(2));
    if d == 0 || v == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    let (name, path) = (arg_str(d, wide), arg_str(v, wide));
    let mut p = p.lock().unwrap();
    if !p.costed {
        return ERROR_FUNCTION_NOT_CALLED;
    }
    if !p.dirs.contains_key(&name) {
        return ERROR_DIRECTORY;
    }
    p.set_target(&name, &path);
    ERROR_SUCCESS
}
fn MsiSetTargetPathA(c: &mut Ctx) -> u32 {
    set_target_path(c, false)
}
fn MsiSetTargetPathW(c: &mut Ctx) -> u32 {
    set_target_path(c, true)
}

fn state(c: &mut Ctx, wide: bool, feature: bool) -> u32 {
    let (h, n, pi, pa) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if n == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    let name = arg_str(n, wide);
    let p = p.lock().unwrap();
    let unknown = if feature { ERROR_UNKNOWN_FEATURE } else { ERROR_UNKNOWN_COMPONENT };
    if !p.cost_started {
        return unknown;
    }
    let s = if feature {
        p.features.iter().find(|f| f.name == name).map(|f| (f.installed, f.action))
    } else {
        p.comps.get(&name).map(|x| if x.enabled { (x.installed, x.action) } else { (INSTALLSTATE_UNKNOWN, INSTALLSTATE_UNKNOWN) })
    };
    let Some((i, a)) = s else { return unknown };
    if pi != 0 {
        w32(pi, i as u32);
    }
    if pa != 0 {
        w32(pa, a as u32);
    }
    ERROR_SUCCESS
}
fn MsiGetFeatureStateA(c: &mut Ctx) -> u32 {
    state(c, false, true)
}
fn MsiGetFeatureStateW(c: &mut Ctx) -> u32 {
    state(c, true, true)
}
fn MsiGetComponentStateA(c: &mut Ctx) -> u32 {
    state(c, false, false)
}
fn MsiGetComponentStateW(c: &mut Ctx) -> u32 {
    state(c, true, false)
}

fn set_feature_state(c: &mut Ctx, wide: bool) -> u32 {
    let (h, n, s) = (c.a(0), c.a(1), c.i(2));
    if n == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    let name = arg_str(n, wide);
    let mut p = p.lock().unwrap();
    if !p.cost_started || !p.features.iter().any(|f| f.name == name) {
        return ERROR_UNKNOWN_FEATURE;
    }
    p.set_feature_state(&name, s, 0);
    ERROR_SUCCESS
}
fn MsiSetFeatureStateA(c: &mut Ctx) -> u32 {
    set_feature_state(c, false)
}
fn MsiSetFeatureStateW(c: &mut Ctx) -> u32 {
    set_feature_state(c, true)
}

/// MsiSetComponentState: a component's action (one whose condition is false keeps none).
fn set_component_state(c: &mut Ctx, wide: bool) -> u32 {
    let (h, n, s) = (c.a(0), c.a(1), c.i(2));
    if n == 0 {
        return ERROR_INVALID_PARAMETER;
    }
    let Some(p) = pkg_of(h) else { return ERROR_INVALID_HANDLE };
    let name = arg_str(n, wide);
    let mut p = p.lock().unwrap();
    if !p.cost_started {
        return ERROR_UNKNOWN_COMPONENT;
    }
    let Some(comp) = p.comps.get_mut(&name) else { return ERROR_UNKNOWN_COMPONENT };
    if comp.enabled {
        comp.action = s;
    }
    ERROR_SUCCESS
}
fn MsiSetComponentStateA(c: &mut Ctx) -> u32 {
    set_component_state(c, false)
}
fn MsiSetComponentStateW(c: &mut Ctx) -> u32 {
    set_component_state(c, true)
}

fn MsiCreateRecordFromPkg(_c: &mut Ctx) -> u32 {
    0
}

pub fn rec(h: u32) -> Option<Record> {
    rec_of(h)
}

pub fn init() {
    macro_rules! reg { ($($f:ident),* $(,)?) => { $( implement("msi", stringify!($f), $f); )* }; }
    reg!(
        MsiOpenPackageA, MsiOpenPackageW, MsiOpenPackageExA, MsiOpenPackageExW, MsiGetPropertyA, MsiGetPropertyW, MsiSetPropertyA,
        MsiSetPropertyW, MsiEvaluateConditionA, MsiEvaluateConditionW, MsiGetActiveDatabase, MsiGetLanguage, MsiGetMode, MsiSetMode,
        MsiGetTargetPathA, MsiGetTargetPathW, MsiGetSourcePathA, MsiGetSourcePathW, MsiSetTargetPathA, MsiSetTargetPathW,
        MsiGetFeatureStateA, MsiGetFeatureStateW, MsiGetComponentStateA, MsiGetComponentStateW, MsiSetFeatureStateA, MsiSetFeatureStateW,
        MsiSetComponentStateA, MsiSetComponentStateW,
    );
    let _ = MsiCreateRecordFromPkg;
    crate::msi_install::init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_paths() {
        // (Windows' answers, from Wine's package tests)
        assert_eq!(clean_path("C:\\\\one\\\\two  "), "C:\\one\\two\\");
        assert_eq!(clean_path("C:\\\\ Program Files \\\\ "), "C:\\Program Files\\");
        assert_eq!(clean_path("C:\\one\\two\\three"), "C:\\one\\two\\three\\");
    }
}
