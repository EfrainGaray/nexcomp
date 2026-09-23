//! Delta + rANS codec for numeric / highly correlated byte streams.

use crate::entropy::{
    build_decode_table, build_table, normalize_freqs, normalize_freqs_unpinned, rans_decode,
    rans_encode, RansError,
};

const NUM_LANES: usize = 4;

/// XOR-delta with configurable stride (stride=1 is classic byte delta).
fn xor_delta_stride(data: &[u8], stride: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for i in 0..data.len() {
        if i < stride {
            out.push(data[i]);
        } else {
            out.push(data[i] ^ data[i - stride]);
        }
    }
    out
}

fn xor_delta_stride_inverse(data: &[u8], stride: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for i in 0..data.len() {
        if i < stride {
            out.push(data[i]);
        } else {
            out.push(data[i] ^ out[i - stride]);
        }
    }
    out
}

fn arith_delta(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    if data.is_empty() {
        return out;
    }
    out.push(data[0]);
    for i in 1..data.len() {
        out.push(data[i].wrapping_sub(data[i - 1]));
    }
    out
}

fn arith_delta_inverse(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    if data.is_empty() {
        return out;
    }
    out.push(data[0]);
    for i in 1..data.len() {
        out.push(data[i].wrapping_add(out[i - 1]));
    }
    out
}

fn byte_entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len = data.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

fn lane_entropy(data: &[u8]) -> f64 {
    let lanes = split_lanes(data, NUM_LANES);
    lanes.iter().map(|lane| byte_entropy(lane)).sum()
}

fn split_lanes(data: &[u8], lanes: usize) -> Vec<Vec<u8>> {
    let mut out = vec![Vec::with_capacity(data.len() / lanes + 1); lanes];
    for (idx, &byte) in data.iter().enumerate() {
        out[idx % lanes].push(byte);
    }
    out
}

fn merge_lanes(lanes: &[Vec<u8>], total_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(total_len);
    let mut positions = vec![0usize; lanes.len()];
    for idx in 0..total_len {
        let lane = idx % lanes.len();
        out.push(lanes[lane][positions[lane]]);
        positions[lane] += 1;
    }
    out
}

/// Delta mode stored in the header byte.
///
/// 0 = XOR delta stride-1 (original default)
/// 1 = arithmetic delta stride-1
/// 2 = XOR delta stride-2
/// 3 = XOR delta stride-4
/// 4 = no delta (raw)
///
/// Encode payload as:
/// [delta_mode:1][lanes:1][orig_len:4]
/// repeated lanes:
///   [sym_len:4][counts:256*4][enc_len:4][enc_bytes]
/// Set on the delta type of a payload whose rANS tables are derived with the
/// pinned tie order. Payloads written before that was pinned do not carry it,
/// and are decoded with [`normalize_freqs_unpinned`] so they keep reading.
const PINNED_TABLE: u8 = 0x80;

pub fn delta_ans_encode(data: &[u8]) -> Result<Vec<u8>, RansError> {
    if data.is_empty() {
        return Ok(vec![0, NUM_LANES as u8, 0, 0, 0, 0]);
    }

    // Build candidates: (mode_id, residual)
    let candidates: Vec<(u8, Vec<u8>)> = vec![
        (0, xor_delta_stride(data, 1)),
        (1, arith_delta(data)),
        (2, xor_delta_stride(data, 2)),
        (3, xor_delta_stride(data, 4)),
        (4, data.to_vec()),
    ];

    // Pick the candidate with lowest lane entropy
    let (delta_type, residual) = candidates
        .into_iter()
        .min_by(|a, b| lane_entropy(&a.1).partial_cmp(&lane_entropy(&b.1)).unwrap())
        .unwrap();

    let lanes = split_lanes(&residual, NUM_LANES);
    let mut out = Vec::new();
    out.push(delta_type | PINNED_TABLE);
    out.push(NUM_LANES as u8);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());

    for lane in lanes {
        out.extend_from_slice(&(lane.len() as u32).to_le_bytes());

        let mut counts = [0u64; 256];
        for &b in &lane {
            counts[b as usize] += 1;
        }
        for &count in &counts {
            out.extend_from_slice(&(count as u32).to_le_bytes());
        }

        if lane.is_empty() {
            out.extend_from_slice(&0u32.to_le_bytes());
            continue;
        }

        let freqs = normalize_freqs(&counts, 256);
        let table = build_table(&freqs)?;
        let encoded = rans_encode(&lane, &table)?;
        out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        out.extend_from_slice(&encoded);
    }

    Ok(out)
}

pub fn delta_ans_decode(payload: &[u8]) -> Result<Vec<u8>, RansError> {
    if payload.len() < 6 {
        return Ok(Vec::new());
    }

    let pinned_table = payload[0] & PINNED_TABLE != 0;
    let delta_type = payload[0] & !PINNED_TABLE;
    let lane_count = payload[1] as usize;
    let orig_len = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]) as usize;
    if orig_len == 0 {
        return Ok(Vec::new());
    }

    let mut pos = 6;
    let mut lanes = Vec::with_capacity(lane_count);

    for _ in 0..lane_count {
        let sym_len = u32::from_le_bytes([
            payload[pos],
            payload[pos + 1],
            payload[pos + 2],
            payload[pos + 3],
        ]) as usize;
        pos += 4;

        let mut counts = [0u64; 256];
        for count in &mut counts {
            *count = u32::from_le_bytes([
                payload[pos],
                payload[pos + 1],
                payload[pos + 2],
                payload[pos + 3],
            ]) as u64;
            pos += 4;
        }

        let enc_len = u32::from_le_bytes([
            payload[pos],
            payload[pos + 1],
            payload[pos + 2],
            payload[pos + 3],
        ]) as usize;
        pos += 4;

        if sym_len == 0 {
            lanes.push(Vec::new());
            continue;
        }

        let encoded = &payload[pos..pos + enc_len];
        pos += enc_len;

        let freqs = if pinned_table {
            normalize_freqs(&counts, 256)
        } else {
            normalize_freqs_unpinned(&counts, 256)
        };
        let table = build_table(&freqs)?;
        let dtable = build_decode_table(&table);
        let decoded = rans_decode(encoded, &dtable, sym_len)?;
        lanes.push(decoded);
    }

    let residual = merge_lanes(&lanes, orig_len);
    Ok(match delta_type {
        0 => xor_delta_stride_inverse(&residual, 1),
        1 => arith_delta_inverse(&residual),
        2 => xor_delta_stride_inverse(&residual, 2),
        3 => xor_delta_stride_inverse(&residual, 4),
        4 => residual, // no delta
        _ => xor_delta_stride_inverse(&residual, 1), // fallback
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every symbol appears in every lane, so the rANS tables are full of
    /// tied fractional parts — the case where the tie order decides the
    /// table, and where an unpinned order used to make the decoder disagree
    /// with the encoder that wrote the file.
    #[test]
    fn roundtrip_lanes_with_every_symbol() {
        let mut data = Vec::with_capacity(16384);
        for i in 0..16384u32 {
            data.push((i.wrapping_mul(2654435761) >> 13) as u8);
        }
        let encoded = delta_ans_encode(&data).unwrap();
        assert_eq!(encoded[0] & PINNED_TABLE, PINNED_TABLE, "new payloads pin the table");
        assert_eq!(delta_ans_decode(&encoded).unwrap(), data);
    }

    /// A payload written before the tie order was pinned carries no flag and
    /// must keep decoding through the old derivation.
    #[test]
    fn payload_without_the_flag_still_decodes() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();
        let mut encoded = delta_ans_encode(&data).unwrap();
        encoded[0] &= !PINNED_TABLE;
        assert_eq!(delta_ans_decode(&encoded).unwrap(), data);
    }

    #[test]
    fn roundtrip_delta_ans_correlated() {
        let mut data = vec![0u8; 8192];
        let mut value = 100u8;
        for (idx, byte) in data.iter_mut().enumerate() {
            value = value.wrapping_add((idx % 3) as u8);
            *byte = value;
        }

        let encoded = delta_ans_encode(&data).expect("encode ok");
        let decoded = delta_ans_decode(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_delta_ans_random() {
        let data: Vec<u8> = (0..4096).map(|i| ((i * 37) & 0xFF) as u8).collect();
        let encoded = delta_ans_encode(&data).expect("encode ok");
        let decoded = delta_ans_decode(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn xor_delta_stride_roundtrip() {
        let data: Vec<u8> = (0..256).map(|i| (i & 0xFF) as u8).collect();
        for stride in [1, 2, 4, 8] {
            let encoded = xor_delta_stride(&data, stride);
            let decoded = xor_delta_stride_inverse(&encoded, stride);
            assert_eq!(decoded, data, "roundtrip failed for stride {stride}");
        }
    }

    #[test]
    fn roundtrip_stride4_structured() {
        // Simulate 32-bit structured values (like geo file)
        let mut data = Vec::with_capacity(8192);
        let mut val: u32 = 1000;
        for _ in 0..2048 {
            data.extend_from_slice(&val.to_le_bytes());
            val = val.wrapping_add(7);
        }
        let encoded = delta_ans_encode(&data).expect("encode ok");
        let decoded = delta_ans_decode(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_no_delta_high_entropy() {
        // High entropy data where no delta should be best (mode 4)
        let data: Vec<u8> = (0..4096u64)
            .scan(0xDEAD_BEEFu64, |state, _| {
                *state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                Some((*state >> 33) as u8)
            })
            .collect();
        let encoded = delta_ans_encode(&data).expect("encode ok");
        let decoded = delta_ans_decode(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn geo_file_roundtrip_and_bpb() {
        let path = "/tmp/nexcomp_corpora/calgary/geo";
        let Ok(data) = std::fs::read(path) else {
            eprintln!("skipping geo test: {path} not found");
            return;
        };
        let encoded = delta_ans_encode(&data).expect("encode ok");
        let decoded = delta_ans_decode(&encoded).expect("decode ok");
        assert_eq!(decoded, data, "geo roundtrip failed");

        let bpb = encoded.len() as f64 * 8.0 / data.len() as f64;
        let mode = encoded[0];
        eprintln!(
            "geo: {} bytes -> {} bytes ({:.3} bpb, mode={mode})",
            data.len(),
            encoded.len(),
            bpb
        );
        // Stride-4 XOR entropy is ~5.10 bpb; rANS output should be in that range
        assert!(bpb < 7.0, "geo bpb {bpb:.3} should be under 7.0");
    }
}
