//! Fill the original SDRT 5.0.9.0 DMM dialog, then queue its real Submit button.
//! No account protocol or licensing decision is implemented here.
use super::*;
use sha2::{Digest, Sha256};

const EMAIL_MAX: usize = 320;
const PASSWORD_MAX: usize = 1024;
const WORDS: usize = 2 + EMAIL_MAX + PASSWORD_MAX;
static ACTIVE_DIALOG: AtomicU32 = AtomicU32::new(0);

pub(super) fn status(dialog: u32, id: u32, text: &[u16]) {
    if id != 1204 || ACTIVE_DIALOG.load(Ordering::Acquire) != dialog { return; }
    let text = String::from_utf16_lossy(text);
    let Some((_, tail)) = text.split_once("[error-code]") else { return };
    let Some(code) = tail.trim().split_whitespace().next().and_then(|v| v.parse::<u32>().ok()).filter(|n| *n <= 65535) else { return };
    #[cfg(target_os = "emscripten")]
    unsafe {
        unsafe extern "C" { fn sake_account_status(code: u32); }
        sake_account_status(code);
    }
    #[cfg(not(target_os = "emscripten"))]
    let _ = code;
}

struct Credentials(Vec<u16>);
impl Drop for Credentials {
    fn drop(&mut self) {
        for word in &mut self.0 {
            unsafe { std::ptr::write_volatile(word, 0) };
        }
    }
}
fn lengths(words: &[u16]) -> Option<(usize, usize)> {
    let (email, password) = (*words.first()? as usize, *words.get(1)? as usize);
    if words.len() != WORDS || email == 0 || password == 0 || email > EMAIL_MAX || password > PASSWORD_MAX {
        return None;
    }
    let a = &words[2..2 + email];
    let b = &words[2 + EMAIL_MAX..2 + EMAIL_MAX + password];
    if a.contains(&0) || b.contains(&0) || char::decode_utf16(a.iter().copied()).any(|c| c.is_err()) || char::decode_utf16(b.iter().copied()).any(|c| c.is_err()) {
        return None;
    }
    Some((email, password))
}
fn trusted(inst: u32) -> bool {
    let modules = [
        ("ucopgdlgres.dll", "5bdedc8d30278a18a53367c7ad526dca2676c07efc620b14c2e493fbe6250818"),
        ("ucopgdlg.dll", "fe13622f748e0e63c357eb3eeadd75de5dca11275c5cb7989d42f19bc9e5b649"),
        ("satx4.ocx", "06a1a6d8454a042a6a58933720e1a8927c25465c7cfac45ae8d9d7d3b0b4c953"),
    ];
    if crate::pe::by_name(modules[0].0) != Some(inst) { return false; }
    modules.iter().all(|(name, expected)| {
        let Some(base) = crate::pe::by_name(name) else { return false };
        let Some(path) = crate::pe::with(base, |m| m.path.clone()) else { return false };
        let Some(path) = crate::files::resolve(&path) else { return false };
        let Ok(bytes) = kura::fs::read(path) else { return false };
        let digest: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        digest == *expected
    })
}

pub(super) fn fill(c: &mut Ctx, dlg: u32, inst: u32) {
    // Check both the original binaries and the dialog layout before touching
    // the one-shot buffer. A game merely naming an Edit is insufficient.
    let (email, password, submit) = (dlg_item(dlg, 1200), dlg_item(dlg, 1201), dlg_item(dlg, 1203));
    let layout = with(|s| {
        let edit = |h, secret| s.wins.get(&h).is_some_and(|w| {
            w.parent == dlg && w.enabled && s.classes[w.class].name.eq_ignore_ascii_case("edit")
                && (w.style & 0x20 != 0) == secret
        });
        edit(email, false) && edit(password, true) && s.wins.get(&submit).is_some_and(|w| {
            w.parent == dlg && w.enabled && s.classes[w.class].name.eq_ignore_ascii_case("button")
        })
    });
    if !layout || !trusted(inst) { return; }
    let mut packet = Credentials(vec![0; WORDS]);
    #[cfg(target_os = "emscripten")]
    let ready = unsafe {
        unsafe extern "C" { fn sake_account_take(dst: *mut u16, cap: usize) -> i32; }
        sake_account_take(packet.0.as_mut_ptr(), WORDS * 2) != 0
    };
    #[cfg(not(target_os = "emscripten"))]
    let ready = { let _ = &mut packet; false };
    if !ready { return; }
    let Some((en, pn)) = lengths(&packet.0) else { return };
    ACTIVE_DIALOG.store(dlg, Ordering::Release);
    for (control, words) in [(email, &packet.0[2..2 + en]), (password, &packet.0[2 + EMAIL_MAX..2 + EMAIL_MAX + pn])] {
        let bytes = (words.len() as u32 + 1) * 2;
        let ptr = mem::heap_alloc(bytes, true);
        if ptr == 0 { return; }
        for (i, &word) in words.iter().enumerate() { w16(ptr + i as u32 * 2, word); }
        as_wide(true, || send(c, control, 0xc, 0, ptr));
        mem::fill(ptr, bytes, 0);
        mem::heap_free(ptr);
        invalidate(control, None, true);
    }
    // Browser settings own persistence; leave the vendor remember checkbox off.
    let remember = dlg_item(dlg, 1202);
    if remember != 0 { send(c, remember, 0xf1, 0, 0); }
    // Run the real handler after initialization. Never manufacture success or
    // automatically replay a failed authentication POST.
    post(submit, 0xf5, 0, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_packet_rejects_truncation_nuls_and_invalid_utf16() {
        assert!(lengths(&[]).is_none());
        let mut p = vec![0; WORDS]; p[0] = 1; p[1] = 1; p[2] = 97; p[2 + EMAIL_MAX] = 98;
        assert_eq!(lengths(&p), Some((1, 1)));
        p[1] = 1025; assert!(lengths(&p).is_none());
        p[1] = 1; p[2 + EMAIL_MAX] = 0; assert!(lengths(&p).is_none());
        p[2 + EMAIL_MAX] = 0xd800; assert!(lengths(&p).is_none());
    }
}
