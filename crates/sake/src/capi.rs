//! CryptoAPI provider contexts and per-installation RSA key containers. Public
//! APIs distinguish opening, creating and deleting a container. RSA material is
//! generated normally and persisted atomically; missing keys never succeed.
use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::sync::{Arc, Mutex, atomic::{AtomicU32, Ordering}};
use sha2::Digest;
use crate::cng::{self, Blob, Rsa};

#[derive(Clone)]
pub(crate) struct Key { pub rsa: Rsa, pub bits: u32, pub exportable: bool }
#[derive(Clone)]
pub(crate) struct Provider {
    path: Option<String>,
    pub rc4_bits: u32,
    rsa_bits: u32,
    generation: Vec<u8>,
    keys: Arc<Mutex<HashMap<u32, Key>>>,
}
static PROVIDERS: Mutex<Option<HashMap<u32, Provider>>> = Mutex::new(None);
static NEXT: AtomicU32 = AtomicU32::new(0x4b00_0000);
fn storage<T>(f: impl FnOnce() -> Result<T, u32>) -> Result<T, u32> {
    // The same lock is shared by the service and its callers. Key generation
    // stays outside it; only filesystem transactions are serialized here.
    crate::kern::with(|_| f()).0
}
pub(crate) fn provider(id: u32) -> Result<Provider, u32> {
    PROVIDERS.lock().unwrap().as_ref().and_then(|m| m.get(&id)).cloned().ok_or(0x8009_0001)
}
pub(crate) fn acquire(container: Option<&str>, name: Option<&str>, ty: u32, flags: u32) -> Result<u32, u32> {
    let name = name.unwrap_or("Microsoft Strong Cryptographic Provider");
    if !matches!(ty, 1 | 24) { return Err(0x8009_0017); }
    let base = name == "Microsoft Base Cryptographic Provider v1.0";
    if !base && !matches!(name, "Microsoft Strong Cryptographic Provider" | "Microsoft Enhanced Cryptographic Provider v1.0" |
        "Microsoft Enhanced RSA and AES Cryptographic Provider" | "Microsoft Enhanced RSA and AES Cryptographic Provider (Prototype)") {
        return Err(0x8009_0013);
    }
    if flags & !(0xf000_0000 | 8 | 16 | 32 | 64) != 0 || flags & 24 == 24 { return Err(cng::NTE_BAD_FLAGS); }
    let temporary = flags & 0xf000_0000 == 0xf000_0000;
    if flags & 0xf000_0000 != 0 && !temporary || temporary && flags & 24 != 0 { return Err(cng::NTE_BAD_FLAGS); }
    let path = if temporary { None } else {
        let container = container.unwrap_or("sake-default-container");
        let digest = sha2::Sha256::digest(format!("{ty}\0{name}\0{container}").as_bytes());
        let id: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let root = if flags & 32 != 0 { "C:\\ProgramData\\Microsoft\\Crypto\\RSA\\MachineKeys" }
            else { "C:\\users\\user\\AppData\\Roaming\\Microsoft\\Crypto\\RSA\\sake" };
        Some(crate::files::resolve_new(&format!("{root}\\{id}")))
    };
    let mut generation = Vec::new();
    if let Some(path) = &path {
        let marker = format!("{path}/keyset");
        let deleted = storage(|| {
          if flags & 16 != 0 {
            // Removing the marker prevents another opener from observing a
            // container while its two key files are being removed.
            kura::fs::remove_file(&marker).map_err(|_| cng::NTE_BAD_KEYSET)?;
            let _ = kura::fs::remove_file(format!("{path}/1.key"));
            let _ = kura::fs::remove_file(format!("{path}/2.key"));
            return Ok(true);
          }
          if flags & 8 != 0 {
            kura::fs::create_dir_all(path).map_err(|_| cng::NTE_PERM)?;
            let mut file = kura::fs::OpenOptions::new().write(true).create_new(true).open(&marker)
                .map_err(|e| if e.kind() == ErrorKind::AlreadyExists { cng::NTE_EXISTS } else { cng::NTE_PERM })?;
            let mut token = [0u8; 16];
            if unsafe { libc::getentropy(token.as_mut_ptr().cast(), token.len()) } != 0 || file.write_all(&token).is_err() {
                let _ = kura::fs::remove_file(&marker); return Err(cng::NTE_PERM);
            }
          }
          generation = kura::fs::read(&marker).map_err(|_| cng::NTE_BAD_KEYSET)?;
          Ok(false)
        })?;
        if deleted { return Ok(0); }
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    PROVIDERS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id,
        Provider { path, generation, rc4_bits: if base { 40 } else { 128 }, rsa_bits: if base { 512 } else { 1024 }, keys: Arc::new(Mutex::new(HashMap::new())) });
    Ok(id)
}
pub(crate) fn release(id: u32, flags: u32) -> Result<(), u32> {
    if flags != 0 { return Err(cng::NTE_BAD_FLAGS); }
    if PROVIDERS.lock().unwrap().as_mut().and_then(|m| m.remove(&id)).is_none() { return Err(0x8009_0001); }
    Ok(())
}
fn spec(alg: u32) -> Result<u32, u32> { match alg { 1 | 0xa400 => Ok(1), 2 | 0x2400 => Ok(2), _ => Err(0x8009_0008) } }
pub(crate) fn user_key(id: u32, key_spec: u32) -> Result<Key, u32> {
    let p = provider(id)?; let key_spec = spec(key_spec)?;
    if let Some(path) = &p.path {
        let bytes = storage(|| {
            if kura::fs::read(format!("{path}/keyset")).ok().as_ref() != Some(&p.generation) { return Err(cng::NTE_BAD_KEYSET); }
            kura::fs::read(format!("{path}/{key_spec}.key")).map_err(|_| cng::NTE_NO_KEY)
        })?;
        if bytes.get(..4) != Some(b"CAP1") || bytes.len() < 8 { return Err(cng::NTE_BAD_DATA); }
        let (rsa, bits) = Rsa::from_blob(Blob::FullPrivate, false, &bytes[8..])?;
        Ok(Key { rsa, bits, exportable: u32::from_le_bytes(bytes[4..8].try_into().unwrap()) & 1 != 0 })
    } else { p.keys.lock().unwrap().get(&key_spec).cloned().ok_or(cng::NTE_NO_KEY) }
}
pub(crate) fn generate(id: u32, alg: u32, flags: u32) -> Result<Key, u32> {
    let p = provider(id)?; let key_spec = spec(alg)?;
    if flags & 0xffff & !1 != 0 { return Err(cng::NTE_BAD_FLAGS); }
    let bits = match flags >> 16 { 0 => p.rsa_bits, b => b };
    if !(384..=16384).contains(&bits) || bits % 16 != 0 { return Err(87); }
    let rsa = Rsa::generate(bits)?;
    let key = Key { rsa, bits, exportable: flags & 1 != 0 };
    if let Some(path) = &p.path {
        let mut bytes = b"CAP1".to_vec(); bytes.extend_from_slice(&(flags & 1).to_le_bytes());
        bytes.extend(key.rsa.to_blob(Blob::FullPrivate, key.bits)?);
        let temp = format!("{path}/{key_spec}.{}.{}.tmp", crate::pid(), NEXT.fetch_add(1, Ordering::Relaxed));
        storage(|| {
          if kura::fs::read(format!("{path}/keyset")).ok().as_ref() != Some(&p.generation) { return Err(cng::NTE_BAD_KEYSET); }
          kura::fs::write(&temp, bytes).map_err(|_| cng::NTE_PERM)?;
          if kura::fs::rename(&temp, format!("{path}/{key_spec}.key")).is_err() {
            let _ = kura::fs::remove_file(temp); return Err(cng::NTE_PERM);
          }
          Ok(())
        })?;
    } else { p.keys.lock().unwrap().insert(key_spec, key.clone()); }
    Ok(key)
}
