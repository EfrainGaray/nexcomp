//! BCJ (Branch-Call-Jump) x86 filter for executable/object files.
//!
//! In x86 code, CALL (0xE8) and JMP (0xE9) instructions use relative addresses.
//! Converting them to absolute addresses makes the targets repeat more often,
//! improving LZ77/BWT compression significantly for executable and object files.

/// Apply BCJ x86 filter: convert relative CALL/JMP addresses to absolute.
pub fn bcj_encode(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    let mut i = 0;
    while i + 4 < out.len() {
        if out[i] == 0xE8 || out[i] == 0xE9 {
            let rel = i32::from_le_bytes([out[i + 1], out[i + 2], out[i + 3], out[i + 4]]);
            let abs = rel.wrapping_add(i as i32);
            let bytes = abs.to_le_bytes();
            out[i + 1] = bytes[0];
            out[i + 2] = bytes[1];
            out[i + 3] = bytes[2];
            out[i + 4] = bytes[3];
            i += 5;
        } else {
            i += 1;
        }
    }
    out
}

/// Reverse BCJ filter: convert absolute addresses back to relative.
pub fn bcj_decode(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    let mut i = 0;
    while i + 4 < out.len() {
        if out[i] == 0xE8 || out[i] == 0xE9 {
            let abs = i32::from_le_bytes([out[i + 1], out[i + 2], out[i + 3], out[i + 4]]);
            let rel = abs.wrapping_sub(i as i32);
            let bytes = rel.to_le_bytes();
            out[i + 1] = bytes[0];
            out[i + 2] = bytes[1];
            out[i + 3] = bytes[2];
            out[i + 4] = bytes[3];
            i += 5;
        } else {
            i += 1;
        }
    }
    out
}

/// Detect if data looks like x86 executable/object code.
///
/// Checks for known binary headers (ELF, PE, Mach-O, COFF .obj) first,
/// then falls back to a heuristic based on E8/E9 byte frequency.
pub fn is_likely_x86(data: &[u8]) -> bool {
    if data.len() < 100 {
        return false;
    }

    // Check for ELF header
    if data.starts_with(b"\x7fELF") {
        return true;
    }
    // Check for PE/COFF (MZ header)
    if data.starts_with(b"MZ") {
        return true;
    }
    // Check for Mach-O (little-endian and big-endian, 32/64-bit)
    if data.starts_with(&[0xCF, 0xFA, 0xED, 0xFE])
        || data.starts_with(&[0xCE, 0xFA, 0xED, 0xFE])
        || data.starts_with(&[0xFE, 0xED, 0xFA, 0xCF])
        || data.starts_with(&[0xFE, 0xED, 0xFA, 0xCE])
    {
        return true;
    }
    // COFF object files: first two bytes are machine type.
    // x86 COFF: 0x4C01 (i386) or 0x6486 (x86-64) in little-endian
    if data.len() >= 20 {
        let machine = u16::from_le_bytes([data[0], data[1]]);
        if machine == 0x014C || machine == 0x8664 {
            // Additional sanity: COFF has a section count at offset 2
            let num_sections = u16::from_le_bytes([data[2], data[3]]);
            if num_sections > 0 && num_sections < 200 {
                return true;
            }
        }
    }

    // Heuristic: E8/E9 frequency between 1% and 10% suggests x86 code
    let call_count = data.iter().filter(|&&b| b == 0xE8 || b == 0xE9).count();
    let ratio = call_count as f64 / data.len() as f64;
    ratio > 0.01 && ratio < 0.10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bcj_roundtrip_empty() {
        let data: &[u8] = &[];
        assert_eq!(bcj_decode(&bcj_encode(data)), data);
    }

    #[test]
    fn test_bcj_roundtrip_short() {
        // Shorter than 5 bytes -- no transformation should occur
        let data = vec![0xE8, 0x01, 0x02, 0x03];
        assert_eq!(bcj_decode(&bcj_encode(&data)), data);
    }

    #[test]
    fn test_bcj_roundtrip_single_call() {
        let data = vec![0xE8, 0x10, 0x00, 0x00, 0x00, 0x90];
        let encoded = bcj_encode(&data);
        // The CALL at position 0 with relative offset 0x10 should become absolute 0x10
        assert_eq!(bcj_decode(&encoded), data);
    }

    #[test]
    fn test_bcj_roundtrip_multiple_calls() {
        let mut data = vec![0x90u8; 256]; // NOP sled
        // Insert some CALL instructions
        data[10] = 0xE8;
        data[11] = 0x50;
        data[12] = 0x00;
        data[13] = 0x00;
        data[14] = 0x00;

        data[50] = 0xE9; // JMP
        data[51] = 0xFF;
        data[52] = 0xFF;
        data[53] = 0xFF;
        data[54] = 0xFF;

        data[100] = 0xE8;
        data[101] = 0x20;
        data[102] = 0x01;
        data[103] = 0x00;
        data[104] = 0x00;

        let encoded = bcj_encode(&data);
        let decoded = bcj_decode(&encoded);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_bcj_roundtrip_all_e8() {
        // Pathological: all 0xE8 bytes
        let data = vec![0xE8; 300];
        let encoded = bcj_encode(&data);
        let decoded = bcj_decode(&encoded);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_bcj_roundtrip_random_data() {
        // Pseudo-random data using LCG
        let mut data = vec![0u8; 8192];
        let mut state: u64 = 0xCAFE_BABE;
        for b in data.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (state >> 33) as u8;
        }
        let encoded = bcj_encode(&data);
        let decoded = bcj_decode(&encoded);
        assert_eq!(decoded, data, "BCJ roundtrip failed on random data");
    }

    #[test]
    fn test_bcj_roundtrip_e8_at_end() {
        // E8 near end of buffer -- not enough bytes for a full instruction
        let mut data = vec![0x90u8; 100];
        data[97] = 0xE8; // Only 2 bytes follow, not enough for i32
        let encoded = bcj_encode(&data);
        let decoded = bcj_decode(&encoded);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_bcj_roundtrip_consecutive_calls() {
        // Back-to-back CALL instructions
        let data = vec![
            0xE8, 0x10, 0x00, 0x00, 0x00, // CALL +16
            0xE8, 0x20, 0x00, 0x00, 0x00, // CALL +32
            0xE9, 0xF0, 0xFF, 0xFF, 0xFF, // JMP -16
            0xE8, 0x05, 0x00, 0x00, 0x00, // CALL +5
            0x90, // NOP
        ];
        let encoded = bcj_encode(&data);
        let decoded = bcj_decode(&encoded);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_is_likely_x86_elf() {
        let mut data = vec![0u8; 200];
        data[0] = 0x7F;
        data[1] = b'E';
        data[2] = b'L';
        data[3] = b'F';
        assert!(is_likely_x86(&data));
    }

    #[test]
    fn test_is_likely_x86_pe() {
        let mut data = vec![0u8; 200];
        data[0] = b'M';
        data[1] = b'Z';
        assert!(is_likely_x86(&data));
    }

    #[test]
    fn test_is_likely_x86_text() {
        let data = b"This is plain text without any binary content whatsoever. \
                     It should not be detected as x86 code by the heuristic. \
                     Adding more text to make it long enough for the size check.";
        assert!(!is_likely_x86(data));
    }

    #[test]
    fn test_is_likely_x86_too_short() {
        let data = vec![0xE8; 50]; // Too short
        assert!(!is_likely_x86(&data));
    }

    #[test]
    fn test_bcj_encode_actually_transforms() {
        // Verify that encode actually changes the data when there are CALL/JMP instructions
        let data = vec![
            0xE8, 0x10, 0x00, 0x00, 0x00, // CALL +16 at position 0
            0x90, 0x90,
        ];
        let _encoded = bcj_encode(&data);
        // At position 0, relative 0x10 -> absolute 0x10 + 0 = 0x10 (same because pos=0)
        // But for position != 0, it should differ
        let data2 = vec![
            0x90, 0x90, 0x90, 0x90, 0x90, // 5 NOPs
            0xE8, 0x10, 0x00, 0x00, 0x00, // CALL +16 at position 5
            0x90,
        ];
        let encoded2 = bcj_encode(&data2);
        // At position 5: relative 0x10 -> absolute 0x10 + 5 = 0x15
        assert_eq!(encoded2[6], 0x15); // 0x10 + 5
        assert_eq!(encoded2[7], 0x00);
        assert_eq!(encoded2[8], 0x00);
        assert_eq!(encoded2[9], 0x00);
        // And it roundtrips
        assert_eq!(bcj_decode(&encoded2), data2);
    }
}
