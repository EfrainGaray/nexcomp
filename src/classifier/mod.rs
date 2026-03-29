// NEXCOMP — Block Classifier (Stage 1)
// Detects domain type of a 64KB block in <1ms without neural models.
//
// Method: byte frequency histogram + bigram entropy estimate + magic bytes
// Reference: file(1) magic database + Shannon entropy thresholding
//
// Domains:
//   TEXT           — natural language UTF-8 text
//   CODE_SRC       — source code (C, Rust, Python, JS, etc.)
//   DNA_FASTA      — FASTA/FASTQ genomic sequences
//   FLOAT_TS       — floating-point time series data
//   IMAGE_RAW      — raw pixel data (BMP, PPM, uncompressed TIFF)
//   STRUCTURED_DATA — JSON, CSV, XML, YAML
//   BINARY_GENERIC — everything else (already compressed, executables, etc.)

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ClassifierError {
    #[error("Block is empty")]
    EmptyBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DomainType {
    Text = 0,
    CodeSrc = 1,
    DnaFasta = 2,
    FloatTs = 3,
    ImageRaw = 4,
    StructuredData = 5,
    BinaryGeneric = 6,
}

impl From<u8> for DomainType {
    fn from(v: u8) -> Self {
        match v {
            0 => DomainType::Text,
            1 => DomainType::CodeSrc,
            2 => DomainType::DnaFasta,
            3 => DomainType::FloatTs,
            4 => DomainType::ImageRaw,
            5 => DomainType::StructuredData,
            _ => DomainType::BinaryGeneric,
        }
    }
}

impl std::fmt::Display for DomainType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DomainType::Text => write!(f, "TEXT"),
            DomainType::CodeSrc => write!(f, "CODE_SRC"),
            DomainType::DnaFasta => write!(f, "DNA_FASTA"),
            DomainType::FloatTs => write!(f, "FLOAT_TS"),
            DomainType::ImageRaw => write!(f, "IMAGE_RAW"),
            DomainType::StructuredData => write!(f, "STRUCTURED_DATA"),
            DomainType::BinaryGeneric => write!(f, "BINARY_GENERIC"),
        }
    }
}

/// Byte frequency histogram for a block.
struct ByteHistogram {
    counts: [u32; 256],
    total: u32,
}

impl ByteHistogram {
    fn from_block(data: &[u8]) -> Self {
        let mut counts = [0u32; 256];
        for &b in data {
            counts[b as usize] += 1;
        }
        ByteHistogram {
            counts,
            total: data.len() as u32,
        }
    }

    /// Shannon entropy in bits per byte: H = -Σ p(x) log2(p(x))
    fn entropy(&self) -> f64 {
        let n = self.total as f64;
        if n == 0.0 {
            return 0.0;
        }
        let mut h = 0.0;
        for &c in &self.counts {
            if c > 0 {
                let p = c as f64 / n;
                h -= p * p.log2();
            }
        }
        h
    }

    /// Fraction of bytes that are printable ASCII (0x20..0x7E) + whitespace
    fn printable_ratio(&self) -> f64 {
        let printable: u32 = (0x20u8..=0x7E)
            .map(|b| self.counts[b as usize])
            .sum::<u32>()
            + self.counts[b'\t' as usize]
            + self.counts[b'\n' as usize]
            + self.counts[b'\r' as usize];
        printable as f64 / self.total.max(1) as f64
    }

    /// Fraction of bytes that are DNA bases: A, C, G, T, N (upper + lower)
    fn dna_ratio(&self) -> f64 {
        let dna: u32 = [b'A', b'C', b'G', b'T', b'N', b'a', b'c', b'g', b't', b'n']
            .iter()
            .map(|&b| self.counts[b as usize])
            .sum();
        dna as f64 / self.total.max(1) as f64
    }

    /// Number of distinct byte values present
    fn distinct_bytes(&self) -> u32 {
        self.counts.iter().filter(|&&c| c > 0).count() as u32
    }
}

/// Bigram entropy estimate: entropy of byte pairs.
/// Used to distinguish structured data (low bigram entropy) from random data.
fn bigram_entropy(data: &[u8]) -> f64 {
    if data.len() < 2 {
        return 0.0;
    }
    // Sample: use first 8192 bigrams to keep classifier < 1ms
    let sample_len = data.len().min(8193);
    let mut counts = std::collections::HashMap::new();
    for pair in data[..sample_len - 1].windows(2) {
        *counts.entry((pair[0], pair[1])).or_insert(0u32) += 1;
    }
    let n = (sample_len - 1) as f64;
    let mut h = 0.0;
    for &c in counts.values() {
        let p = c as f64 / n;
        h -= p * p.log2();
    }
    h
}

/// Check for known magic bytes at the start of the block.
fn check_magic(data: &[u8]) -> Option<DomainType> {
    if data.len() < 4 {
        return None;
    }
    // FASTA: starts with '>'
    if data[0] == b'>' && data.get(1).map_or(false, |&b| b.is_ascii_alphabetic()) {
        return Some(DomainType::DnaFasta);
    }
    // FASTQ: starts with '@'
    if data[0] == b'@' && data.len() > 10 {
        // Check if line 3 starts with '+' (FASTQ format)
        if let Some(pos) = data.iter().position(|&b| b == b'\n') {
            if pos + 1 < data.len() {
                // Could be FASTQ — check DNA content further
                let dna_check = ByteHistogram::from_block(&data[..data.len().min(1024)]);
                if dna_check.dna_ratio() > 0.7 {
                    return Some(DomainType::DnaFasta);
                }
            }
        }
    }
    // BMP image
    if data[0] == b'B' && data[1] == b'M' {
        return Some(DomainType::ImageRaw);
    }
    // PPM/PGM/PBM
    if data[0] == b'P' && (data[1] >= b'1' && data[1] <= b'6') && data[2].is_ascii_whitespace() {
        return Some(DomainType::ImageRaw);
    }
    // JSON
    if data[0] == b'{' || (data[0] == b'[' && data[1] == b'{') {
        return Some(DomainType::StructuredData);
    }
    // XML/HTML
    if data[0] == b'<' && (data[1] == b'?' || data[1] == b'!' || data[1].is_ascii_alphabetic()) {
        return Some(DomainType::StructuredData);
    }
    None
}

/// Classify a block of data into its domain type.
///
/// Algorithm:
///   1. Check magic bytes → may short-circuit
///   2. Compute byte histogram + entropy + bigram entropy
///   3. Apply decision tree:
///      - DNA: dna_ratio > 0.85
///      - Text/Code: printable_ratio > 0.90
///        - Code if contains {, }, ;, // with high frequency
///      - Float TS: entropy in [3.0, 5.0] + many '0'-'9' and '.' bytes
///      - Image: low entropy + many distinct byte values
///      - Binary: everything else
///
/// Performance: <1ms for 64KB block (no allocations beyond HashMap for bigrams).
pub fn classify_block(data: &[u8]) -> Result<DomainType, ClassifierError> {
    if data.is_empty() {
        return Err(ClassifierError::EmptyBlock);
    }

    // Step 1: Magic bytes
    if let Some(domain) = check_magic(data) {
        return Ok(domain);
    }

    // Step 2: Compute statistics
    let hist = ByteHistogram::from_block(data);
    let entropy = hist.entropy();
    let printable = hist.printable_ratio();
    let dna_ratio = hist.dna_ratio();
    let distinct = hist.distinct_bytes();
    let bi_entropy = bigram_entropy(data);

    // Step 3: Decision tree

    // DNA: >85% ACGTN characters, low entropy, AND very few null/control bytes.
    // Without the null-byte check, tar files with DNA content get misclassified.
    let null_count = hist.counts[0];
    let control_ratio = null_count as f64 / hist.total.max(1) as f64;
    if dna_ratio > 0.85 && entropy < 3.5 && control_ratio < 0.01 {
        return Ok(DomainType::DnaFasta);
    }

    // Float time series: lots of digits and decimal points
    let digit_dot_ratio = {
        let digits: u32 = (b'0'..=b'9')
            .map(|b| hist.counts[b as usize])
            .sum::<u32>()
            + hist.counts[b'.' as usize]
            + hist.counts[b'-' as usize]
            + hist.counts[b'e' as usize]
            + hist.counts[b'E' as usize];
        digits as f64 / hist.total.max(1) as f64
    };
    if digit_dot_ratio > 0.6 && printable > 0.95 {
        // Could be CSV of floats or structured data
        let comma_ratio = hist.counts[b',' as usize] as f64 / hist.total.max(1) as f64;
        if comma_ratio > 0.02 {
            return Ok(DomainType::StructuredData);
        }
        return Ok(DomainType::FloatTs);
    }

    // Printable text or code: >90% printable ASCII
    if printable > 0.90 {
        // Heuristic for code: presence of braces, semicolons, keywords
        let code_chars: u32 = hist.counts[b'{' as usize]
            + hist.counts[b'}' as usize]
            + hist.counts[b';' as usize]
            + hist.counts[b'(' as usize]
            + hist.counts[b')' as usize];
        let code_ratio = code_chars as f64 / hist.total.max(1) as f64;

        if code_ratio > 0.02 {
            return Ok(DomainType::CodeSrc);
        }
        return Ok(DomainType::Text);
    }

    // Structured data: moderate entropy, some printable
    if printable > 0.70 && bi_entropy < 10.0 {
        return Ok(DomainType::StructuredData);
    }

    // Image raw: many distinct byte values, moderate entropy
    if distinct > 200 && entropy > 5.0 && entropy < 7.5 {
        return Ok(DomainType::ImageRaw);
    }

    // High entropy (>7.5 bpb) = likely already compressed or encrypted
    Ok(DomainType::BinaryGeneric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_text() {
        let text = b"The quick brown fox jumps over the lazy dog. \
                     This is a sample of English text for classification. \
                     It should be detected as natural language text.";
        let domain = classify_block(text).expect("classify ok");
        assert_eq!(domain, DomainType::Text);
    }

    #[test]
    fn test_classify_code() {
        let code = b"fn main() { let x = 42; println!(\"{}\", x); } \
                     fn foo(a: i32) -> i32 { if a > 0 { a * 2 } else { -a } } \
                     struct Bar { field: String; count: usize; }";
        let domain = classify_block(code).expect("classify ok");
        assert_eq!(domain, DomainType::CodeSrc);
    }

    #[test]
    fn test_classify_dna() {
        let dna: Vec<u8> = "ACGTACGTNNACGTACGTACGTNNACGT"
            .repeat(100)
            .into_bytes();
        let domain = classify_block(&dna).expect("classify ok");
        assert_eq!(domain, DomainType::DnaFasta);
    }

    #[test]
    fn test_classify_json() {
        let json = b"{\"name\": \"test\", \"value\": 42, \"nested\": {\"a\": true}}";
        let domain = classify_block(json).expect("classify ok");
        assert_eq!(domain, DomainType::StructuredData);
    }

    #[test]
    fn test_classify_binary() {
        // High entropy random-looking data
        let binary: Vec<u8> = (0..4096).map(|i| ((i * 17 + 31) % 256) as u8).collect();
        let domain = classify_block(&binary).expect("classify ok");
        // Should be BinaryGeneric or ImageRaw depending on entropy
        assert!(
            domain == DomainType::BinaryGeneric || domain == DomainType::ImageRaw,
            "Random data should classify as Binary or ImageRaw, got {domain}"
        );
    }
}
