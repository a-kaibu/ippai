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

    /// The object's own key, unwrapped (what a browser decrypts the object with:
    /// AES-256-GCM, the first 12 bytes the nonce, the object's UUID bytes the associated data).
    pub fn data_key(&self, master: &[u8; 32]) -> Result<[u8; 32]> {
        ensure!(self.version == 1, "unsupported envelope version");
        let k = open(master, &self.aad(), &self.wrapped_key)?;
        k.try_into().map_err(|_| anyhow!("bad data key length"))
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
    fn keys_and_nonces_are_random() {
        let (a, b) = (random::<32>().unwrap(), random::<32>().unwrap());
        assert!(a != b && a != [0; 32] && a != [1; 32]);
        // the same data twice: other data keys, other ciphertexts
        let (e1, c1) = Envelope::encrypt(&[1; 32], "k", b"same").unwrap();
        let (e2, c2) = Envelope::encrypt(&[1; 32], "k", b"same").unwrap();
        assert_ne!(c1, c2);
        assert_ne!(e1.object, e2.object);
    }

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

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Whatever is sealed opens with its master key, also after a rewrap to another.
        #[test]
        fn seal_open_rewrap(plain in proptest::collection::vec(any::<u8>(), 0..2000), a in any::<[u8; 32]>(), b in any::<[u8; 32]>()) {
            let (e, c) = Envelope::encrypt(&a, "k1", &plain).unwrap();
            prop_assert_eq!(e.decrypt(&a, &c).unwrap(), plain.clone());
            let r = e.rewrap(&a, &b, "k2").unwrap();
            prop_assert_eq!(r.decrypt(&b, &c).unwrap(), plain);
            if a != b {
                prop_assert!(r.decrypt(&a, &c).is_err());
                prop_assert!(e.decrypt(&b, &c).is_err());
            }
        }

        /// One flipped bit anywhere (ciphertext, wrapped key, key id) or a cut: refused.
        #[test]
        fn any_change_is_refused(plain in proptest::collection::vec(any::<u8>(), 1..300), at in any::<prop::sample::Index>(), bit in 0u8..8, which in 0u8..4) {
            let (mut e, mut c) = Envelope::encrypt(&[9; 32], "key", &plain).unwrap();
            match which {
                0 => { let i = at.index(c.len()); c[i] ^= 1 << bit }
                1 => { let i = at.index(e.wrapped_key.len()); e.wrapped_key[i] ^= 1 << bit }
                2 => e.key_id.push('x'),
                _ => c.truncate(at.index(c.len())),
            }
            prop_assert!(e.decrypt(&[9; 32], &c).is_err());
        }
    }
}
