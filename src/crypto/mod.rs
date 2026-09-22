// NEXCOMP — Authenticated Encryption (Stage 6)
// Protocol: ChaCha20-Poly1305 (RFC 8439)
// Key derivation: Argon2id(password, salt=random_32B) with explicit, versioned costs
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
use argon2::{Algorithm, Argon2, Params, Version};
use rand::{rngs::OsRng, TryRngCore};
use thiserror::Error;
use zeroize::Zeroizing;

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
    #[error("key derivation parameters out of range")]
    KdfParamsOutOfRange,
    #[error("the operating system random source failed: {0}")]
    RandomSource(String),
    #[error("unknown key derivation function id {0}")]
    UnknownKdf(u8),
    #[error("encrypted file header is truncated")]
    TruncatedHeader,
    #[error("not an encrypted file")]
    NotSealed,
}

/// Argon2id (version 0x13) costs. They are part of the file format: NXE2
/// pins them, NXE3 stores them in its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

/// The costs every NXE2 file was written with (argon2 0.5 defaults, pinned).
pub const NXE2_KDF: KdfParams = KdfParams { m_cost_kib: 19_456, t_cost: 2, p_cost: 1 };

/// Costs for new files: 64 MiB and 3 passes (RFC 9106's second option, one lane).
pub const DEFAULT_KDF: KdfParams = KdfParams { m_cost_kib: 65_536, t_cost: 3, p_cost: 1 };

/// Largest costs a file may request, so a hostile header cannot demand
/// gigabytes of memory or minutes of work before authentication fails.
pub const MAX_KDF: KdfParams = KdfParams { m_cost_kib: 1 << 20, t_cost: 16, p_cost: 16 };

impl KdfParams {
    pub fn within_limits(&self) -> bool {
        self.m_cost_kib <= MAX_KDF.m_cost_kib && self.t_cost <= MAX_KDF.t_cost && self.p_cost <= MAX_KDF.p_cost
    }
}

/// Derive a 256-bit key from a password with Argon2id; the key is wiped on drop.
fn derive_key(password: &[u8], salt: &[u8; 32], kdf: KdfParams) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    if !kdf.within_limits() {
        return Err(CryptoError::KdfParamsOutOfRange);
    }
    let params = Params::new(kdf.m_cost_kib, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|_| CryptoError::KdfParamsOutOfRange)?;
    let mut okm = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password, salt, okm.as_mut())
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
pub fn encrypt(plaintext: &[u8], master_key: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    encrypt_with(plaintext, master_key, aad, NXE2_KDF)
}

/// [`encrypt`] with explicit key-derivation costs.
pub fn encrypt_with(
    plaintext: &[u8],
    master_key: &[u8],
    aad: &[u8],
    kdf: KdfParams,
) -> Result<Vec<u8>, CryptoError> {
    if master_key.is_empty() {
        return Err(CryptoError::KeyTooShort);
    }

    // Generate random salt and nonce
    let mut salt = [0u8; 32];
    let mut nonce_bytes = [0u8; 12];
    OsRng.try_fill_bytes(&mut salt).map_err(|e| CryptoError::RandomSource(e.to_string()))?;
    OsRng.try_fill_bytes(&mut nonce_bytes).map_err(|e| CryptoError::RandomSource(e.to_string()))?;

    let key = derive_key(master_key, &salt, kdf)?;
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref())
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
pub fn decrypt(encrypted: &[u8], master_key: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    decrypt_with(encrypted, master_key, aad, NXE2_KDF)
}

/// [`decrypt`] with explicit key-derivation costs.
pub fn decrypt_with(
    encrypted: &[u8],
    master_key: &[u8],
    aad: &[u8],
    kdf: KdfParams,
) -> Result<Vec<u8>, CryptoError> {
    if encrypted.len() < 32 + ENCRYPTION_OVERHEAD {
        return Err(CryptoError::CiphertextTooShort);
    }

    let salt: [u8; 32] = encrypted[..32]
        .try_into()
        .map_err(|_| CryptoError::CiphertextTooShort)?;
    let nonce_bytes = &encrypted[32..44];
    let ciphertext = &encrypted[44..];

    let key = derive_key(master_key, &salt, kdf)?;
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref())
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

// Encrypted files. The header is the AEAD associated data and is followed by
// the frame written by `encrypt`:
//   NXE2: [4B "NXE2"][4B input length u32]                         (Argon2id at NXE2_KDF)
//   NXE3: [4B "NXE3"][1B kdf id][4B m KiB][4B t][4B p][8B input length u64]

pub const NXE2_MAGIC: &[u8; 4] = b"NXE2";
pub const NXE3_MAGIC: &[u8; 4] = b"NXE3";
const NXE2_HEADER_LEN: usize = 8;
const NXE3_HEADER_LEN: usize = 25;
/// NXE3 kdf id: Argon2id, version 0x13.
const KDF_ARGON2ID_V13: u8 = 1;

/// Whether `file` starts with an encrypted-file magic.
pub fn is_sealed(file: &[u8]) -> bool {
    file.starts_with(NXE2_MAGIC) || file.starts_with(NXE3_MAGIC)
}

/// Encrypt `payload`, the compressed form of `input_len` bytes, as an NXE3 file.
pub fn seal(payload: &[u8], password: &[u8], input_len: u64, kdf: KdfParams) -> Result<Vec<u8>, CryptoError> {
    let mut file = NXE3_MAGIC.to_vec();
    file.push(KDF_ARGON2ID_V13);
    for cost in [kdf.m_cost_kib, kdf.t_cost, kdf.p_cost] {
        file.extend_from_slice(&cost.to_le_bytes());
    }
    file.extend_from_slice(&input_len.to_le_bytes());
    let frame = encrypt_with(payload, password, &file, kdf)?;
    file.extend_from_slice(&frame);
    Ok(file)
}

/// Decrypt an NXE2 or NXE3 file back to its payload. NXE3 costs are checked
/// against [`MAX_KDF`] before any key derivation.
pub fn open(file: &[u8], password: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let (header_len, kdf) = if file.starts_with(NXE3_MAGIC) {
        let h = file.get(..NXE3_HEADER_LEN).ok_or(CryptoError::TruncatedHeader)?;
        if h[4] != KDF_ARGON2ID_V13 {
            return Err(CryptoError::UnknownKdf(h[4]));
        }
        let cost = |at: usize| u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
        (NXE3_HEADER_LEN, KdfParams { m_cost_kib: cost(5), t_cost: cost(9), p_cost: cost(13) })
    } else if file.starts_with(NXE2_MAGIC) {
        (NXE2_HEADER_LEN, NXE2_KDF)
    } else {
        return Err(CryptoError::NotSealed);
    };
    if file.len() < header_len {
        return Err(CryptoError::TruncatedHeader);
    }
    let (aad, frame) = file.split_at(header_len);
    decrypt_with(frame, password, aad, kdf)
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
        let key = derive_key(b"correct horse battery staple", &salt, NXE2_KDF).expect("kdf ok");
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "092d6e91987840e63e2fac5e187ac5d29b489f05597971fd6554555a1a20ce2a");
    }

    /// Cheap costs so the wrapper tests stay fast.
    const FAST: KdfParams = KdfParams { m_cost_kib: 64, t_cost: 1, p_cost: 1 };

    #[test]
    fn test_nxe3_roundtrip_records_the_kdf_costs() {
        let file = seal(b"payload", b"pw", 7, FAST).expect("seal ok");
        assert_eq!(&file[..5], b"NXE3\x01");
        assert_eq!(&file[5..17], [64u32, 1, 1].map(u32::to_le_bytes).concat());
        assert_eq!(&file[17..25], 7u64.to_le_bytes());
        assert_eq!(open(&file, b"pw").expect("open ok"), b"payload");
        assert!(matches!(open(&file, b"other"), Err(CryptoError::DecryptionFailed)));
    }

    #[test]
    fn test_nxe3_header_is_authenticated() {
        let mut file = seal(b"payload", b"pw", 7, FAST).expect("seal ok");
        file[24] ^= 1; // declared input length
        assert!(matches!(open(&file, b"pw"), Err(CryptoError::DecryptionFailed)));
    }

    #[test]
    fn test_nxe2_files_still_open() {
        let header = [&NXE2_MAGIC[..], &7u32.to_le_bytes()].concat();
        let file = [header.clone(), encrypt(b"payload", b"pw", &header).expect("encrypt ok")].concat();
        assert_eq!(open(&file, b"pw").expect("open ok"), b"payload");
    }

    #[test]
    fn test_hostile_nxe3_headers_are_rejected_before_key_derivation() {
        let file = seal(b"payload", b"pw", 7, FAST).expect("seal ok");
        let with = |at: usize, bytes: &[u8]| {
            let mut f = file.clone();
            f[at..at + bytes.len()].copy_from_slice(bytes);
            open(&f, b"pw")
        };
        assert!(matches!(with(5, &u32::MAX.to_le_bytes()), Err(CryptoError::KdfParamsOutOfRange)));
        assert!(matches!(with(9, &u32::MAX.to_le_bytes()), Err(CryptoError::KdfParamsOutOfRange)));
        assert!(matches!(with(13, &u32::MAX.to_le_bytes()), Err(CryptoError::KdfParamsOutOfRange)));
        assert!(matches!(with(9, &0u32.to_le_bytes()), Err(CryptoError::KdfParamsOutOfRange)));
        assert!(matches!(with(4, &[2]), Err(CryptoError::UnknownKdf(2))));
        for len in 0..NXE3_HEADER_LEN + 60 {
            assert!(open(&file[..len.min(file.len() - 1)], b"pw").is_err(), "prefix of {len} bytes");
        }
    }
}
