//! Versioned, authenticated envelopes. Data keys are independent of the wrapping key.
use anyhow::{anyhow, ensure, Result};
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| anyhow!("OS random source failed"))?;
    Ok(out)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub object: Uuid,
    pub key_id: String,
    pub nonce: [u8; 12],
    pub wrapped_key: Vec<u8>,
}

fn crypt_key(key: &[u8]) -> Result<aead::LessSafeKey> {
    Ok(aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key)
            .map_err(|_| anyhow!("invalid key length"))?,
    ))
}

fn seal(key: &[u8], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let nonce = random::<12>()?;
    let mut data = plain.to_vec();
    crypt_key(key)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut data,
        )
        .map_err(|_| anyhow!("encryption failed"))?;
    Ok([nonce.as_slice(), &data].concat())
}

fn open(key: &[u8], aad: &[u8], cipher: &[u8]) -> Result<Vec<u8>> {
    ensure!(cipher.len() >= 28, "truncated ciphertext");
    let mut data = cipher[12..].to_vec();
    let plain = crypt_key(key)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key(cipher[..12].try_into()?),
            aead::Aad::from(aad),
            &mut data,
        )
        .map_err(|_| anyhow!("authentication failed"))?;
    Ok(plain.to_vec())
}

impl Envelope {
    fn aad(&self) -> Vec<u8> {
        format!(
            "ippai:envelope:{}:{}:{}",
            self.version, self.object, self.key_id
        )
        .into_bytes()
    }

    pub fn encrypt(master: &[u8; 32], key_id: &str, plain: &[u8]) -> Result<(Self, Vec<u8>)> {
        let key = random::<32>()?;
        let mut e = Self {
            version: 1,
            object: Uuid::new_v4(),
            key_id: key_id.into(),
            nonce: [0; 12],
            wrapped_key: vec![],
        };
        let ciphertext = seal(&key, e.object.as_bytes(), plain)?;
        e.nonce.copy_from_slice(&ciphertext[..12]);
        e.wrapped_key = seal(master, &e.aad(), &key)?;
        Ok((e, ciphertext))
    }

    pub fn decrypt(&self, master: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>> {
        ensure!(self.version == 1, "unsupported envelope version");
        ensure!(
            ciphertext.get(..12) == Some(self.nonce.as_slice()),
            "nonce mismatch"
        );
        let key = open(master, &self.aad(), &self.wrapped_key)?;
        open(&key, self.object.as_bytes(), ciphertext)
    }

    pub fn rewrap(&self, old: &[u8; 32], new: &[u8; 32], key_id: &str) -> Result<Self> {
        ensure!(self.version == 1, "unsupported envelope version");
        let key = open(old, &self.aad(), &self.wrapped_key)?;
        let mut e = self.clone();
        e.key_id = key_id.into();
        e.wrapped_key = seal(new, &e.aad(), &key)?;
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_does_not_change_object_or_ciphertext() {
        let (e, cipher) = Envelope::encrypt(&[1; 32], "old", b"game data").unwrap();
        let updated = e.rewrap(&[1; 32], &[2; 32], "new").unwrap();
        assert_eq!(updated.object, e.object);
        assert_eq!(updated.decrypt(&[2; 32], &cipher).unwrap(), b"game data");
        assert!(updated.decrypt(&[1; 32], &cipher).is_err());
    }
    #[test]
    fn rejects_tampering_and_substitution() {
        let (e, mut cipher) = Envelope::encrypt(&[1; 32], "key", b"save").unwrap();
        let mut swapped = e.clone();
        swapped.object = Uuid::new_v4();
        assert!(swapped.decrypt(&[1; 32], &cipher).is_err());
        cipher[12] ^= 1;
        assert!(e.decrypt(&[1; 32], &cipher).is_err());
        assert!(e.decrypt(&[1; 32], b"short").is_err());
    }
}
