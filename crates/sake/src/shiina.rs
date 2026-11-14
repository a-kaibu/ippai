//! ShiinaRio (椎名里緒) games check that they were installed: the engine reads the registry key
//! its INI names (`Reg=` in the "[椎名里緒 vX.YY]" section of RIO.INI or <exe>.INI) and stops
//! with "インストールが正しく行われていません" when the values the official SETUP.exe writes
//! there are missing, then reads its data from the folder they name (#49).
//!
//! A game folder brought in from an installed copy, or a trial that ships SETUP.exe next to the
//! game, has no such registry on sake. Here those values are what SETUP.exe writes for a game
//! installed where it is (recorded from bitch sister 体験版's SETUP.exe, 椎名里緒 v2.50, run on
//! sake): InstMode 0 (DWORD), InstPath the game's folder, DataPath its savedata folder (the
//! installer's default save place). Values the game or an installer wrote win: nothing is set
//! when InstPath is there.

/// The registry key (under HKEY_CURRENT_USER) a ShiinaRio game's INI names, if it is one.
pub fn reg_key(ini: &str) -> Option<String> {
    let mut ours = false;
    for l in ini.lines() {
        let l = l.trim();
        if let Some(s) = l.strip_prefix('[') {
            ours = s.starts_with("椎名里緒");
            continue;
        }
        if !ours {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('\\');
            if k.trim().eq_ignore_ascii_case("reg") && !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// REG_SZ data: UTF-16 with its NUL.
fn sz(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect()
}

/// The values SETUP.exe writes into the key for a game installed in `dir` (a Windows path):
/// (name, type, data).
pub fn values(dir: &str) -> Vec<(&'static str, u32, Vec<u8>)> {
    let dir = dir.trim_end_matches('\\');
    vec![("InstMode", 4, 0u32.to_le_bytes().to_vec()), ("InstPath", 1, sz(dir)), ("DataPath", 1, sz(&format!("{dir}\\savedata\\")))]
}

/// The INIs a ShiinaRio exe reads its settings from: <exe>.INI (older versions), RIO.INI.
pub fn inis(exe: &str) -> Option<(String, [String; 2])> {
    let (dir, file) = exe.rsplit_once('\\')?;
    let stem = file.rsplit_once('.').map_or(file, |x| x.0);
    Some((dir.to_string(), [format!("{dir}\\{stem}.INI"), format!("{dir}\\RIO.INI")]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// bitch sister 体験版's RIO.INI
    const RIO: &str = "[椎名里緒 v2.50]\r\nReg=software\\ｏｎｏｍａｔｏｐｅ＊\\ビッチ姉妹が清純なはずがないっ！！　体験版（ＣｙｃｌｏｎｅＸ１０ＵＳＢ対応）\r\nWindowWidth=1280\r\nScn=start.scn\r\n";

    #[test]
    fn reg_of_rio_ini() {
        assert_eq!(reg_key(RIO).as_deref(), Some("software\\ｏｎｏｍａｔｏｐｅ＊\\ビッチ姉妹が清純なはずがないっ！！　体験版（ＣｙｃｌｏｎｅＸ１０ＵＳＢ対応）"));
        // ENGAGE LINKS 体験版 (v2.45): no Reg=, no check
        assert_eq!(reg_key("[椎名里緒 v2.45]\nWindowWidth=800\nScn=start.scn\n"), None);
        // another section's Reg=
        assert_eq!(reg_key("[Other]\nReg=software\\x\n[椎名里緒 v2.50]\nScn=a\n"), None);
        assert_eq!(reg_key("Reg=software\\x\n"), None);
        assert_eq!(reg_key("[椎名里緒 v2.50]\nREG = software\\x\\\n").as_deref(), Some("software\\x"));
    }

    /// What SETUP.exe wrote on sake (SAKE_TRACE_REG) for the default install folder.
    #[test]
    fn values_as_setup_writes_them() {
        let dir = "C:\\ｏｎｏｍａｔｏｐｅ＊\\ビッチ姉妹が清純なはずがないっ！！　体験版（ＣｙｃｌｏｎｅＸ１０ＵＳＢ対応）";
        let v = values(dir);
        assert_eq!(v[0], ("InstMode", 4, vec![0, 0, 0, 0]));
        assert_eq!(v[1], ("InstPath", 1, sz(dir)));
        assert_eq!(v[2], ("DataPath", 1, sz(&format!("{dir}\\savedata\\"))));
        assert_eq!(values(&format!("{dir}\\")), v);
    }

    #[test]
    fn inis_of_exe() {
        let (d, i) = inis("Z:\\masu\\game\\bs\\BITCHES2_TKB.exe").unwrap();
        assert_eq!(d, "Z:\\masu\\game\\bs");
        assert_eq!(i, ["Z:\\masu\\game\\bs\\BITCHES2_TKB.INI".to_string(), "Z:\\masu\\game\\bs\\RIO.INI".to_string()]);
        assert_eq!(inis("noslash.exe"), None);
    }

    proptest! {
        /// Any key the section names comes back as written (trimmed of backslashes), whatever
        /// the other lines are.
        #[test]
        fn reg_round_trip(key in "[a-zA-Z0-9ｏｎ＊\\\\ ]{1,40}", junk in proptest::collection::vec("[a-zA-Z]{1,8}=[a-z0-9]{0,8}", 0..5)) {
            let k = key.trim().trim_matches('\\').to_string();
            prop_assume!(!k.is_empty());
            let mut ini = String::from("[椎名里緒 v2.50]\n");
            for j in &junk { ini += j; ini += "\n"; }
            ini += &format!("Reg={key}\n");
            let got = reg_key(&ini);
            // (a junk line named "reg" comes first)
            if junk.iter().any(|j| j.split_once('=').is_some_and(|(n, v)| n.eq_ignore_ascii_case("reg") && !v.is_empty())) {
                prop_assert!(got.is_some());
            } else {
                prop_assert_eq!(got, Some(k));
            }
        }

        /// The strings are NUL-terminated UTF-16 and DataPath is InstPath's savedata folder.
        #[test]
        fn values_shape(dir in "[A-Z]:(\\\\[a-zA-Z0-9ぁ-ん　]{1,12}){0,4}") {
            let v = values(&dir);
            let s = |b: &[u8]| String::from_utf16(&b.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect::<Vec<_>>()).unwrap();
            let inst = s(&v[1].2);
            let data = s(&v[2].2);
            prop_assert!(inst.ends_with('\0') && data.ends_with('\0'));
            prop_assert_eq!(format!("{}\\savedata\\\0", inst.trim_end_matches('\0')), data);
            prop_assert!(!inst.trim_end_matches('\0').ends_with('\\'));
        }
    }
}
