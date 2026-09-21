// NEXCOMP — Authenticated Encryption (Stage 6)
// Protocol: ChaCha20-Poly1305 (RFC 8439)
// Key derivation: Argon2id(master_key, salt=random_32B), 19 MiB, 2 passes
//
// Security properties:
//   - Confidentiality: ChaCha20 stream cipher (256-bit key, 96-bit nonce)
//   - Integrity + Authenticity: Poly1305 MAC (128-bit tag)
//   - AAD: file header is authenticated but not encrypted
//
// Order: compress-then-encrypt (§7A proves this is correct)
// Mitigates: CRIME/BREACH by isolating compression from network channel

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use argon2::Argon2;
use thiserror::Error;

/// Encryption overhead: 12B nonce + 16B Poly1305 tag = 28 bytes
pub const ENCRYPTION_OVERHEAD: usize = 12 + 16;

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("Argon2id key derivation failed")]
    KdfError,
    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),
    #[error("Decryption failed: authentication tag mismatch")]
    DecryptionFailed,
    #[error("Invalid ciphertext: too short (need at least {ENCRYPTION_OVERHEAD} bytes)")]
    CiphertextTooShort,
    #[error("Master key must be at least 1 byte")]
    KeyTooShort,
}

/// Derive a 256-bit encryption key from a master key or password with Argon2id.
///
/// Parameters:
///   master_key: user-provided key material or password (>= 1 byte)
///   salt: 32 random bytes (stored alongside ciphertext)
///
/// Argon2id with the crate defaults (19 MiB, 2 passes, 1 lane) makes every
/// password guess cost memory-hard work, unlike a plain hash.
fn derive_key(master_key: &[u8], salt: &[u8; 32]) -> Result<[u8; 32], CryptoError> {
    let mut okm = [0u8; 32];
    Argon2::default()
        .hash_password_into(master_key, salt, &mut okm)
        .map_err(|_| CryptoError::KdfError)?;
    Ok(okm)
}

/// Encrypt compressed data with ChaCha20-Poly1305.
///
/// Frame format:
///   [32B salt][12B nonce][variable ciphertext][16B tag]
///
/// AAD (Additional Authenticated Data): the file header bytes.
///   - Not encrypted, but integrity-protected by Poly1305 tag.
///   - Ensures header tampering is detected on decryption.
///
/// Total overhead: 32 (salt) + 12 (nonce) + 16 (tag) = 60 bytes
pub fn encrypt(
    plaintext: &[u8],
    master_key: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if master_key.is_empty() {
        return Err(CryptoError::KeyTooShort);
    }

    // Generate random salt and nonce
    let mut salt = [0u8; 32];
    let mut nonce_bytes = [0u8; 12];
    getrandom(&mut salt);
    getrandom(&mut nonce_bytes);

    let key = derive_key(master_key, &salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let payload = Payload {
        msg: plaintext,
        aad,
    };

    let ciphertext = cipher
        .encrypt(nonce, payload)
        .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;

    // Output: salt || nonce || ciphertext (includes tag appended by AEAD)
    let mut output = Vec::with_capacity(32 + 12 + ciphertext.len());
    output.extend_from_slice(&salt);
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);

    Ok(output)
}

/// Decrypt data encrypted by `encrypt()`.
///
/// Expects format: [32B salt][12B nonce][ciphertext + 16B tag]
/// AAD must match exactly what was passed during encryption.
pub fn decrypt(
    encrypted: &[u8],
    master_key: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if encrypted.len() < 32 + ENCRYPTION_OVERHEAD {
        return Err(CryptoError::CiphertextTooShort);
    }

    let salt: [u8; 32] = encrypted[..32]
        .try_into()
        .map_err(|_| CryptoError::CiphertextTooShort)?;
    let nonce_bytes = &encrypted[32..44];
    let ciphertext = &encrypted[44..];

    let key = derive_key(master_key, &salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;
    let nonce = Nonce::from_slice(nonce_bytes);

    let payload = Payload {
        msg: ciphertext,
        aad,
    };

    cipher
        .decrypt(nonce, payload)
        .map_err(|_| CryptoError::DecryptionFailed)
}

/// Platform-independent random byte generation (wraps rand crate).
fn getrandom(buf: &mut [u8]) {
    use rand::RngCore;
    rand::thread_rng().fill_bytes(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let plaintext = b"NEXCOMP compressed data payload for testing";
        let key = b"my-secret-master-key-32b!!!!!!!!";
        let aad = b"NXC\x01"; // file header

        let encrypted = encrypt(plaintext, key, aad).expect("encrypt ok");
        assert!(encrypted.len() > plaintext.len(), "ciphertext should be larger");

        let decrypted = decrypt(&encrypted, key, aad).expect("decrypt ok");
        assert_eq!(plaintext.as_slice(), decrypted.as_slice());
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let plaintext = b"sensitive data";
        let key = b"another-test-key-here!!!";
        let aad = b"header";

        let mut encrypted = encrypt(plaintext, key, aad).expect("encrypt ok");
        // Tamper with ciphertext byte
        let mid = encrypted.len() / 2;
        encrypted[mid] ^= 0xFF;

        let result = decrypt(&encrypted, key, aad);
        assert!(result.is_err(), "Tampered ciphertext must fail authentication");
    }

    #[test]
    fn test_wrong_aad_fails() {
        let plaintext = b"data";
        let key = b"key-for-aad-test-16b";
        let aad = b"correct-header";

        let encrypted = encrypt(plaintext, key, aad).expect("encrypt ok");
        let result = decrypt(&encrypted, key, b"wrong-header");
        assert!(result.is_err(), "Wrong AAD must fail authentication");
    }

    #[test]
    fn test_overhead_size() {
        let plaintext = b"test payload";
        let key = b"overhead-test-key!!!!!!!";
        let aad = b"";

        let encrypted = encrypt(plaintext, key, aad).expect("encrypt ok");
        // Overhead = 32 (salt) + 12 (nonce) + 16 (tag) = 60
        assert_eq!(
            encrypted.len(),
            plaintext.len() + 60,
            "Overhead should be exactly 60 bytes (32 salt + 12 nonce + 16 tag)"
        );
    }

    #[test]
    fn test_key_derivation_is_argon2id() {
        // Reference from libargon2 (argon2-cffi): Argon2id v19, m=19456 KiB, t=2, p=1.
        let salt: [u8; 32] = std::array::from_fn(|i| i as u8);
        let key = derive_key(b"correct horse battery staple", &salt).expect("kdf ok");
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "092d6e91987840e63e2fac5e187ac5d29b489f05597971fd6554555a1a20ce2a");
    }
}
