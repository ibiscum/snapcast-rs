//! ChaCha20-Poly1305 decryption for audio chunks.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use snapcast_proto::f32lz4::{CRYPTO_HKDF_INFO, CRYPTO_KEY_LEN, CRYPTO_NONCE_LEN, CRYPTO_TAG_LEN};

fn derive_key(psk: &[u8], salt: &[u8]) -> [u8; CRYPTO_KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(Some(salt), psk);
    let mut key = [0u8; CRYPTO_KEY_LEN];
    hk.expand(CRYPTO_HKDF_INFO, &mut key)
        .expect("CRYPTO_KEY_LEN is a valid HKDF-SHA256 output length");
    key
}

/// Audio chunk decryptor.
pub struct ChunkDecryptor {
    cipher: ChaCha20Poly1305,
}

impl ChunkDecryptor {
    /// Create from PSK and session salt.
    pub fn new(psk: &str, salt: &[u8]) -> Self {
        let key = derive_key(psk.as_bytes(), salt);
        Self {
            cipher: ChaCha20Poly1305::new(&key.into()),
        }
    }

    /// Decrypt a chunk. Input: `[nonce][ciphertext + tag]`.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, chacha20poly1305::Error> {
        if data.len() < CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN {
            return Err(chacha20poly1305::Error);
        }
        let nonce = Nonce::from_slice(&data[..CRYPTO_NONCE_LEN]);
        self.cipher.decrypt(nonce, &data[CRYPTO_NONCE_LEN..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypt_chunk(psk: &str, salt: &[u8], nonce: [u8; CRYPTO_NONCE_LEN], plaintext: &[u8]) -> Vec<u8> {
        let key = derive_key(psk.as_bytes(), salt);
        let cipher = ChaCha20Poly1305::new(&key.into());
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .expect("test encryption should succeed");
        let mut out = Vec::with_capacity(CRYPTO_NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        out
    }

    #[test]
    fn derive_key_is_deterministic_and_salt_sensitive() {
        let psk = b"my-psk";
        let salt_a = b"salt-a";
        let salt_b = b"salt-b";
        let key1 = derive_key(psk, salt_a);
        let key2 = derive_key(psk, salt_a);
        let key3 = derive_key(psk, salt_b);
        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn decrypt_roundtrip_succeeds() {
        let psk = "key";
        let salt = b"salt";
        let nonce = [7u8; CRYPTO_NONCE_LEN];
        let plaintext = b"snapcast-audio-frame";
        let chunk = encrypt_chunk(psk, salt, nonce, plaintext);

        let dec = ChunkDecryptor::new(psk, salt);
        let out = dec.decrypt(&chunk).unwrap();
        assert_eq!(out, plaintext);
    }

    #[test]
    fn decrypt_short_input_fails() {
        let dec = ChunkDecryptor::new("key", b"salt");
        let short = vec![0u8; CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN - 1];
        assert!(dec.decrypt(&short).is_err());
    }

    #[test]
    fn decrypt_with_tampered_nonce_fails() {
        let psk = "key";
        let salt = b"salt";
        let nonce = [9u8; CRYPTO_NONCE_LEN];
        let plaintext = b"hello";
        let mut chunk = encrypt_chunk(psk, salt, nonce, plaintext);
        chunk[0] ^= 0x01;

        let dec = ChunkDecryptor::new(psk, salt);
        assert!(dec.decrypt(&chunk).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let salt = b"salt";
        let nonce = [1u8; CRYPTO_NONCE_LEN];
        let plaintext = b"authentic-frame";
        let chunk = encrypt_chunk("right-key", salt, nonce, plaintext);
        let dec = ChunkDecryptor::new("wrong-key", salt);
        assert!(dec.decrypt(&chunk).is_err());
    }
}
