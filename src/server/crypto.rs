//! Vanilla login cryptography through OpenSSL's maintained EVP implementation.
//!
//! RSA key generation runs once before serving. Private-key operations are
//! synchronous CPU kernels: callers must admit them to the shared CPU budget.
//! No primitive, padding scheme, or cipher mode is implemented here.

use openssl::{
    cipher::Cipher,
    cipher_ctx::CipherCtx,
    encrypt::Decrypter,
    hash::{Hasher, MessageDigest},
    pkey::{PKey, Private},
    rand::rand_bytes,
    rsa::{Padding, Rsa},
};
use std::fmt;

pub const RSA_CIPHERTEXT_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CryptoError {
    InvalidKeyResponse,
    LibraryFailure,
    InputTooLong,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidKeyResponse => "invalid login key response",
            Self::LibraryFailure => "cryptographic operation failed",
            Self::InputTooLong => "cipher input exceeds supported length",
        })
    }
}

impl std::error::Error for CryptoError {}

impl From<openssl::error::ErrorStack> for CryptoError {
    fn from(_: openssl::error::ErrorStack) -> Self {
        Self::LibraryFailure
    }
}

pub struct ServerKey {
    key: PKey<Private>,
    public_der: Vec<u8>,
}

/// Secrets intentionally have no Debug or Clone implementation.
pub struct LoginSecret {
    pub shared_secret: [u8; 16],
    pub server_hash: String,
}

impl ServerKey {
    pub fn generate() -> Result<Self, CryptoError> {
        let key = PKey::from_rsa(Rsa::generate(1024)?)?;
        let public_der = key.public_key_to_der()?;
        Ok(Self { key, public_der })
    }

    pub fn public_key_der(&self) -> &[u8] {
        &self.public_der
    }

    pub fn challenge(&self) -> Result<[u8; 4], CryptoError> {
        let mut challenge = [0; 4];
        rand_bytes(&mut challenge)?;
        Ok(challenge)
    }

    /// RSA ciphertexts up to 128 bytes are left-padded like JCE's RSA integer
    /// input. Reported failures, mismatched challenges and wrong secret sizes
    /// share one public error.
    /// EVP's default implicit-rejection behavior is never disabled.
    /// A synthetic secret from implicit rejection is not authentication: the
    /// caller must still verify the derived session hash with the auth service.
    pub fn verify_key_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_challenge: &[u8],
        expected: [u8; 4],
    ) -> Result<LoginSecret, CryptoError> {
        if encrypted_secret.is_empty()
            || encrypted_challenge.is_empty()
            || encrypted_secret.len() > RSA_CIPHERTEXT_BYTES
            || encrypted_challenge.len() > RSA_CIPHERTEXT_BYTES
        {
            return Err(CryptoError::InvalidKeyResponse);
        }
        let mut decrypter = Decrypter::new(&self.key)?;
        decrypter.set_rsa_padding(Padding::PKCS1)?;
        let mut padded = [0; RSA_CIPHERTEXT_BYTES];
        padded[RSA_CIPHERTEXT_BYTES - encrypted_challenge.len()..]
            .copy_from_slice(encrypted_challenge);
        let mut decoded = [0u8; RSA_CIPHERTEXT_BYTES];
        let challenge_len = decrypter
            .decrypt(&padded, &mut decoded)
            .map_err(|_| CryptoError::InvalidKeyResponse)?;
        if challenge_len != expected.len() || !openssl::memcmp::eq(&decoded[..4], &expected) {
            return Err(CryptoError::InvalidKeyResponse);
        }
        padded.fill(0);
        padded[RSA_CIPHERTEXT_BYTES - encrypted_secret.len()..].copy_from_slice(encrypted_secret);
        let secret_len = decrypter
            .decrypt(&padded, &mut decoded)
            .map_err(|_| CryptoError::InvalidKeyResponse)?;
        if secret_len != 16 {
            return Err(CryptoError::InvalidKeyResponse);
        }
        let mut shared_secret = [0; 16];
        shared_secret.copy_from_slice(&decoded[..16]);
        Ok(LoginSecret {
            server_hash: login_digest(&shared_secret, &self.public_der)?,
            shared_secret,
        })
    }
}

/// Dedicated Vanilla uses an empty server ID. SHA1 is a wire requirement here.
pub fn login_digest(secret: &[u8; 16], public_der: &[u8]) -> Result<String, CryptoError> {
    let mut hasher = Hasher::new(MessageDigest::sha1())?;
    hasher.update(secret)?;
    hasher.update(public_der)?;
    let digest = hasher.finish()?;
    let mut bytes = [0; 20];
    bytes.copy_from_slice(&digest);
    Ok(signed_hex(bytes))
}

fn signed_hex(mut bytes: [u8; 20]) -> String {
    let negative = bytes[0] & 0x80 != 0;
    if negative {
        let mut carry = true;
        for byte in bytes.iter_mut().rev() {
            let (value, overflow) = (!*byte).overflowing_add(u8::from(carry));
            *byte = value;
            carry = overflow;
        }
    }
    let mut text = String::with_capacity(41);
    if negative {
        text.push('-');
    }
    let digits = b"0123456789abcdef";
    let mut started = false;
    for byte in bytes {
        for nibble in [byte >> 4, byte & 15] {
            if started || nibble != 0 {
                text.push(char::from(digits[usize::from(nibble)]));
                started = true;
            }
        }
    }
    if !started {
        text.push('0');
    }
    text
}

/// Only the caller's explicit offline-mode branch should use this UUID.
pub fn offline_uuid(name: &str) -> Result<[u8; 16], CryptoError> {
    let mut md5 = Hasher::new(MessageDigest::md5())?;
    md5.update(b"OfflinePlayer:")?;
    md5.update(name.as_bytes())?;
    let digest = md5.finish()?;
    let mut uuid = [0; 16];
    uuid.copy_from_slice(&digest);
    uuid[6] = (uuid[6] & 0x0f) | 0x30;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    Ok(uuid)
}

pub fn random_uuid() -> Result<[u8; 16], CryptoError> {
    let mut uuid = [0; 16];
    rand_bytes(&mut uuid)?;
    uuid[6] = (uuid[6] & 0x0f) | 0x40;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    Ok(uuid)
}

pub struct EncryptCipher(CipherCtx);
pub struct DecryptCipher(CipherCtx);

pub struct CipherPair {
    encrypt: EncryptCipher,
    decrypt: DecryptCipher,
}

impl CipherPair {
    pub fn new(secret: [u8; 16]) -> Result<Self, CryptoError> {
        let mut encrypt = CipherCtx::new()?;
        encrypt.encrypt_init(Some(Cipher::aes_128_cfb8()), Some(&secret), Some(&secret))?;
        let mut decrypt = CipherCtx::new()?;
        decrypt.decrypt_init(Some(Cipher::aes_128_cfb8()), Some(&secret), Some(&secret))?;
        Ok(Self {
            encrypt: EncryptCipher(encrypt),
            decrypt: DecryptCipher(decrypt),
        })
    }

    pub fn into_parts(self) -> (EncryptCipher, DecryptCipher) {
        (self.encrypt, self.decrypt)
    }

    pub fn encrypt_in_place(&mut self, bytes: &mut [u8]) -> Result<(), CryptoError> {
        self.encrypt.encrypt_in_place(bytes)
    }

    pub fn decrypt_in_place(&mut self, bytes: &mut [u8]) -> Result<(), CryptoError> {
        self.decrypt.decrypt_in_place(bytes)
    }
}

impl EncryptCipher {
    pub fn encrypt_in_place(&mut self, bytes: &mut [u8]) -> Result<(), CryptoError> {
        apply(&mut self.0, bytes)
    }
}

impl DecryptCipher {
    pub fn decrypt_in_place(&mut self, bytes: &mut [u8]) -> Result<(), CryptoError> {
        apply(&mut self.0, bytes)
    }
}

fn apply(context: &mut CipherCtx, bytes: &mut [u8]) -> Result<(), CryptoError> {
    if bytes.len() > i32::MAX as usize {
        return Err(CryptoError::InputTooLong);
    }
    let written = context.cipher_update_inplace(bytes, bytes.len())?;
    if written != bytes.len() {
        return Err(CryptoError::LibraryFailure);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_sha1_format_matches_known_java_examples() {
        for (name, expected) in [
            ("Notch", "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"),
            ("jeb_", "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"),
            ("simon", "88e16a1019277b15d58faf0541e11910eb756f6"),
        ] {
            let digest = openssl::hash::hash(MessageDigest::sha1(), name.as_bytes()).unwrap();
            let mut bytes = [0; 20];
            bytes.copy_from_slice(&digest);
            assert_eq!(signed_hex(bytes), expected);
        }
        assert_eq!(signed_hex([0; 20]), "0");
        assert_eq!(signed_hex([255; 20]), "-1");
    }
}
