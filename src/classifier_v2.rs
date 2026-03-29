// NEXCOMP v1.2 — adaptive block classifier

/// Detailed block metrics used by the adaptive selector.
#[derive(Debug, Clone)]
pub struct BlockMetrics {
    pub entropy: f64,
    pub lz77_sample_ratio: f64,
    pub ascii_ratio: f64,
    pub autocorrelation: f64,
    pub unique_bytes: usize,
    pub max_run_length: usize,
    pub byte_variance: f64,
}

/// Codec choice for adaptive block compression.
///
/// Wire format ids match the v1.2 contract:
///   0 = baseline LZ77+Huffman
///   1 = LZMA-style
///   2 = Delta+ANS
///   3 = RLE+Huffman
///   4 = passthrough
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CodecChoice {
    Lz77Huffman = 0,
    LzmaStyle = 1,
    DeltaAns = 2,
    RleHuffman = 3,
    Passthrough = 4,
}

impl CodecChoice {
    pub fn from_codec_id(id: u8) -> Self {
        match id {
            0 => Self::Lz77Huffman,
            1 => Self::LzmaStyle,
            2 => Self::DeltaAns,
            3 => Self::RleHuffman,
            _ => Self::Passthrough,
        }
    }

    pub fn codec_id(self) -> u8 {
        self as u8
    }
}

impl std::fmt::Display for CodecChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecChoice::Lz77Huffman => write!(f, "Lz77Huffman"),
            CodecChoice::LzmaStyle => write!(f, "LzmaStyle"),
            CodecChoice::DeltaAns => write!(f, "DeltaAns"),
            CodecChoice::RleHuffman => write!(f, "RleHuffman"),
            CodecChoice::Passthrough => write!(f, "Passthrough"),
        }
    }
}

impl BlockMetrics {
    pub fn compute(data: &[u8]) -> Self {
        if data.is_empty() {
            return Self {
                entropy: 0.0,
                lz77_sample_ratio: 0.0,
                ascii_ratio: 0.0,
                autocorrelation: 0.0,
                unique_bytes: 0,
                max_run_length: 0,
                byte_variance: 0.0,
            };
        }

        let mut freq = [0u32; 256];
        let mut max_run_length = 1usize;
        let mut run = 1usize;
        for (idx, &b) in data.iter().enumerate() {
            freq[b as usize] += 1;
            if idx > 0 {
                if data[idx - 1] == b {
                    run += 1;
                    max_run_length = max_run_length.max(run);
                } else {
                    run = 1;
                }
            }
        }

        let len = data.len() as f64;
        let entropy: f64 = freq
            .iter()
            .filter(|&&count| count > 0)
            .map(|&count| {
                let p = count as f64 / len;
                -p * p.log2()
            })
            .sum();

        let ascii_count = data
            .iter()
            .filter(|&&b| (0x20..=0x7E).contains(&b))
            .count();
        let ascii_ratio = ascii_count as f64 / len;

        let unique_bytes = freq.iter().filter(|&&count| count > 0).count();

        let mean = data.iter().map(|&b| b as f64).sum::<f64>() / len;
        let byte_variance = data
            .iter()
            .map(|&b| {
                let diff = b as f64 - mean;
                diff * diff
            })
            .sum::<f64>()
            / len;

        let autocorrelation = if data.len() < 2 {
            0.0
        } else {
            let mut sum_xy = 0.0;
            let mut sum_x2 = 0.0;
            let mut sum_y2 = 0.0;
            for window in data.windows(2) {
                let x = window[0] as f64 - mean;
                let y = window[1] as f64 - mean;
                sum_xy += x * y;
                sum_x2 += x * x;
                sum_y2 += y * y;
            }
            if sum_x2 == 0.0 || sum_y2 == 0.0 {
                0.0
            } else {
                sum_xy / (sum_x2 * sum_y2).sqrt()
            }
        };

        Self {
            entropy,
            lz77_sample_ratio: sample_lz77_ratio(data),
            ascii_ratio,
            autocorrelation,
            unique_bytes,
            max_run_length,
            byte_variance,
        }
    }
}

pub fn classify(metrics: &BlockMetrics) -> CodecChoice {
    if metrics.entropy > 7.5 {
        return CodecChoice::Passthrough;
    }
    if metrics.unique_bytes <= 8
        && metrics.max_run_length > 20
        && metrics.autocorrelation > 0.85
    {
        return CodecChoice::RleHuffman;
    }
    // Text and structured data benefit from LZMA's context model
    // Use LZMA for anything with high ASCII ratio (text, code, markup)
    if metrics.ascii_ratio > 0.70 {
        return CodecChoice::LzmaStyle;
    }
    if metrics.autocorrelation > 0.60
        && metrics.ascii_ratio < 0.30
        && metrics.unique_bytes > 20
    {
        return CodecChoice::DeltaAns;
    }
    // Structured binary / geophysical data: high variance, many unique bytes,
    // low ASCII — stride-based delta in DeltaAns will find the best stride.
    if metrics.byte_variance > 3000.0
        && metrics.unique_bytes > 200
        && metrics.ascii_ratio < 0.40
    {
        return CodecChoice::DeltaAns;
    }
    // Binary structured data — try LZMA (selector will fall back to baseline if worse)
    CodecChoice::LzmaStyle
}

pub fn classify_block_v2(data: &[u8]) -> (CodecChoice, BlockMetrics) {
    let metrics = BlockMetrics::compute(data);
    (classify(&metrics), metrics)
}

fn sample_lz77_ratio(data: &[u8]) -> f64 {
    const MIN_MATCH: usize = 3;
    const MAX_MATCH_CHECK: usize = 32;
    const WINDOW: usize = 4096;

    if data.len() <= MIN_MATCH {
        return 0.0;
    }

    let sample_count = data.len().min(128);
    let step = ((data.len() - MIN_MATCH) / sample_count.max(1)).max(1);
    let mut sampled = 0usize;
    let mut matched = 0usize;

    let mut pos = 0usize;
    while pos + MIN_MATCH <= data.len() && sampled < sample_count {
        let start = pos.saturating_sub(WINDOW);
        let max_len = (data.len() - pos).min(MAX_MATCH_CHECK);
        let mut best = 0usize;

        for cand in start..pos {
            let mut len = 0usize;
            while len < max_len && data[cand + len] == data[pos + len] {
                len += 1;
            }
            best = best.max(len);
            if best == max_len {
                break;
            }
        }

        matched += best.min(MAX_MATCH_CHECK);
        sampled += MAX_MATCH_CHECK;
        pos = pos.saturating_add(step);
    }

    if sampled == 0 {
        0.0
    } else {
        matched as f64 / sampled as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_empty() {
        let metrics = BlockMetrics::compute(&[]);
        assert_eq!(metrics.entropy, 0.0);
        assert_eq!(metrics.lz77_sample_ratio, 0.0);
        assert_eq!(metrics.unique_bytes, 0);
        assert_eq!(metrics.max_run_length, 0);
    }

    #[test]
    fn english_maps_to_lzma() {
        let data = b"The quick brown fox jumps over the lazy dog. ".repeat(256);
        let (choice, metrics) = classify_block_v2(&data);
        assert_eq!(choice, CodecChoice::LzmaStyle);
        assert!(metrics.ascii_ratio > 0.75);
    }

    #[test]
    fn random_maps_to_passthrough() {
        let data: Vec<u8> = (0..65536u64)
            .scan(0x1234_5678u64, |state, _| {
                *state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1);
                Some((*state >> 24) as u8)
            })
            .collect();
        let (choice, metrics) = classify_block_v2(&data);
        assert_eq!(choice, CodecChoice::Passthrough);
        assert!(metrics.entropy > 7.5);
    }

    #[test]
    fn run_heavy_maps_to_rle() {
        let mut data = Vec::new();
        data.extend(std::iter::repeat_n(0u8, 400));
        data.extend(std::iter::repeat_n(255u8, 400));
        data.extend(std::iter::repeat_n(0u8, 400));
        let (choice, _) = classify_block_v2(&data);
        assert_eq!(choice, CodecChoice::RleHuffman);
    }

    #[test]
    fn correlated_binary_maps_to_delta() {
        let metrics = BlockMetrics {
            entropy: 5.2,
            lz77_sample_ratio: 0.18,
            ascii_ratio: 0.08,
            autocorrelation: 0.88,
            unique_bytes: 96,
            max_run_length: 4,
            byte_variance: 412.0,
        };
        assert_eq!(classify(&metrics), CodecChoice::DeltaAns);
    }

    #[test]
    fn structured_binary_maps_to_delta() {
        // Geo-like: high variance, many unique bytes, low ASCII, low autocorrelation
        let metrics = BlockMetrics {
            entropy: 5.65,
            lz77_sample_ratio: 0.10,
            ascii_ratio: 0.15,
            autocorrelation: -0.22,
            unique_bytes: 244,
            max_run_length: 3,
            byte_variance: 5200.0,
        };
        assert_eq!(classify(&metrics), CodecChoice::DeltaAns);
    }

    #[test]
    fn codec_ids_match_contract() {
        assert_eq!(CodecChoice::Lz77Huffman.codec_id(), 0);
        assert_eq!(CodecChoice::LzmaStyle.codec_id(), 1);
        assert_eq!(CodecChoice::DeltaAns.codec_id(), 2);
        assert_eq!(CodecChoice::RleHuffman.codec_id(), 3);
        assert_eq!(CodecChoice::Passthrough.codec_id(), 4);
    }
}
