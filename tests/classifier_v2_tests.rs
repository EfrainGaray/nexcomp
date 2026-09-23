// Integration tests for classifier_v2
use nexcomp::classifier_v2::{BlockMetrics, CodecChoice, classify_block_v2};

// ---- Metrics computation on known data ----

#[test]
fn test_metrics_all_zeros() {
    let data = vec![0u8; 1024];
    let m = BlockMetrics::compute(&data);
    assert_eq!(m.entropy, 0.0, "all-zeros entropy must be 0");
    assert_eq!(m.unique_bytes, 1);
    assert_eq!(m.max_run_length, 1024);
    assert_eq!(m.ascii_ratio, 0.0);
    assert_eq!(m.byte_variance, 0.0);
    assert!(m.lz77_sample_ratio >= 0.0 && m.lz77_sample_ratio <= 1.0);
}

#[test]
fn test_metrics_english_text() {
    let text = b"The quick brown fox jumps over the lazy dog. \
                 This is a sample of English text for classification testing. \
                 It should have moderate entropy, high ASCII ratio, and low zero ratio.";
    let m = BlockMetrics::compute(text);
    assert!(m.entropy > 3.0 && m.entropy < 5.5, "english entropy ~4 bpb, got {}", m.entropy);
    assert!(m.ascii_ratio > 0.95, "english ascii_ratio should be near 1.0, got {}", m.ascii_ratio);
    assert!(m.unique_bytes > 15, "english text uses many distinct bytes");
    assert!(m.lz77_sample_ratio >= 0.0 && m.lz77_sample_ratio <= 1.0);
}

#[test]
fn test_metrics_pseudo_random() {
    let data: Vec<u8> = (0..65536u64)
        .scan(42u64, |state, _| {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            Some((*state >> 33) as u8)
        })
        .collect();
    let m = BlockMetrics::compute(&data);
    assert!(m.entropy > 7.0, "pseudo-random entropy should be high, got {}", m.entropy);
    assert!(m.unique_bytes >= 200, "pseudo-random should have many distinct bytes");
    assert!(m.autocorrelation.abs() < 0.1, "pseudo-random autocorrelation should be near 0");
}

#[test]
fn test_metrics_binary_pattern() {
    let data: Vec<u8> = (0..1024).map(|i| if i % 2 == 0 { 0x00 } else { 0xFF }).collect();
    let m = BlockMetrics::compute(&data);
    assert_eq!(m.unique_bytes, 2);
    assert!((m.entropy - 1.0).abs() < 0.01, "two-value entropy should be ~1.0, got {}", m.entropy);
    assert_eq!(m.max_run_length, 1);
    assert_eq!(m.ascii_ratio, 0.0);
}

#[test]
fn test_metrics_empty() {
    let m = BlockMetrics::compute(&[]);
    assert_eq!(m.entropy, 0.0);
    assert_eq!(m.unique_bytes, 0);
    assert_eq!(m.max_run_length, 0);
    assert_eq!(m.lz77_sample_ratio, 0.0);
}

// ---- Sanity checks on metric ranges ----

#[test]
fn test_metrics_sanity_ranges() {
    let patterns: Vec<Vec<u8>> = vec![
        vec![0u8; 256],
        (0..256).map(|i| i as u8).collect(),
        b"Hello, World! This is a test.".to_vec(),
        (0..1024).map(|i| ((i * 37) % 256) as u8).collect(),
    ];
    for (idx, data) in patterns.iter().enumerate() {
        let m = BlockMetrics::compute(data);
        assert!(m.entropy >= 0.0, "pattern {idx}: entropy must be >= 0");
        assert!(m.entropy <= 8.0, "pattern {idx}: entropy must be <= 8");
        assert!(m.ascii_ratio >= 0.0 && m.ascii_ratio <= 1.0, "pattern {idx}: ascii_ratio out of range");
        assert!(
            m.autocorrelation >= -1.0 && m.autocorrelation <= 1.0,
            "pattern {idx}: autocorrelation out of range: {}",
            m.autocorrelation
        );
        assert!(m.unique_bytes <= 256, "pattern {idx}: unique_bytes > 256");
        assert!(m.max_run_length >= 1, "pattern {idx}: max_run_length must be >= 1");
        assert!(
            m.lz77_sample_ratio >= 0.0 && m.lz77_sample_ratio <= 1.0,
            "pattern {idx}: lz77_sample_ratio out of range"
        );
        assert!(m.byte_variance >= 0.0, "pattern {idx}: variance must be >= 0");
    }
}

// ---- Classification tests ----

#[test]
fn test_classify_english_text_lzma() {
    let text = b"The quick brown fox jumps over the lazy dog. \
                 This is a sample of English text for classification testing. \
                 Natural language compresses well with LZMA-style range coders.";
    let (choice, _) = classify_block_v2(text);
    assert_eq!(choice, CodecChoice::LzmaStyle, "English text should map to LzmaStyle");
}

#[test]
fn test_classify_random_passthrough() {
    let data: Vec<u8> = (0..65536u64)
        .scan(42u64, |state, _| {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            Some((*state >> 33) as u8)
        })
        .collect();
    let (choice, m) = classify_block_v2(&data);
    assert!(
        m.entropy > 7.5,
        "pseudo-random entropy must be > 7.5 for Passthrough, got {}",
        m.entropy
    );
    assert_eq!(choice, CodecChoice::Passthrough, "High-entropy data should be Passthrough");
}

#[test]
fn test_classify_rle_data() {
    let mut data = Vec::with_capacity(4096);
    for _ in 0..64 {
        data.extend(std::iter::repeat_n(0x00, 30));
        data.extend(std::iter::repeat_n(0xFF, 34));
    }
    let (choice, m) = classify_block_v2(&data);
    assert!(m.unique_bytes <= 8, "RLE test data should have few unique bytes");
    assert!(m.max_run_length > 20, "RLE test data should have long runs");
    assert_eq!(choice, CodecChoice::RleHuffman, "Few-value run data should map to RleHuffman");
}

#[test]
fn test_classify_correlated_numeric() {
    // Slowly-varying ramp that stays in a non-ASCII range (128..192)
    let mut data = Vec::with_capacity(4096);
    for i in 0..4096u32 {
        let val = 160.0 + 30.0 * (i as f64 * 0.005).sin();
        data.push(val.clamp(0.0, 255.0) as u8);
    }
    let (choice, m) = classify_block_v2(&data);
    assert!(
        m.autocorrelation > 0.60,
        "correlated numeric data should have autocorrelation > 0.60, got {}",
        m.autocorrelation
    );
    assert!(m.ascii_ratio < 0.30, "numeric bytes should not be mostly ASCII printable, got {}", m.ascii_ratio);
    assert_eq!(choice, CodecChoice::DeltaAns, "Correlated non-text data should map to DeltaAns");
}

#[test]
fn test_classify_baseline_binary() {
    // Moderate entropy binary: 64 distinct byte values, no long runs, low ASCII ratio.
    // Per v1.2 classifier contract, ambiguous cases fall through to LzmaStyle and
    // the selector compares that candidate against the baseline.
    let data: Vec<u8> = (0u32..4096).map(|i| {
        (0x80 + ((i.wrapping_mul(37).wrapping_add(13)) % 64)) as u8
    }).collect();
    let (choice, _) = classify_block_v2(&data);
    assert_eq!(choice, CodecChoice::LzmaStyle, "Ambiguous binary should fall through to LzmaStyle");
}

// ---- Calgary corpus integration test ----

#[test]
fn test_classify_calgary_corpus() {
    let corpus_dir = std::path::Path::new("/tmp/nexcomp_corpora/calgary");
    if !corpus_dir.exists() {
        eprintln!("Calgary corpus not found at {}, skipping", corpus_dir.display());
        return;
    }

    let mut entries: Vec<_> = std::fs::read_dir(corpus_dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    assert!(!entries.is_empty(), "Calgary corpus directory is empty");

    println!(
        "\n{:<12} {:>7} {:>6} {:>6} {:>5} {:>5} {:>8} {:>6}  CODEC",
        "FILE", "ENTROPY", "ASCII", "ACORR", "UNIQ", "RUN", "VAR", "LZ77"
    );
    println!("{}", "-".repeat(80));

    for entry in &entries {
        let data = std::fs::read(entry.path()).expect("read file");
        let block = &data[..data.len().min(65536)];
        let (choice, m) = classify_block_v2(block);
        let name = entry.file_name();
        let name = name.to_string_lossy();
        println!(
            "{:<12} {:>7.3} {:>6.3} {:>6.3} {:>5} {:>5} {:>8.1} {:>6.3}  {}",
            name, m.entropy, m.ascii_ratio, m.autocorrelation, m.unique_bytes, m.max_run_length,
            m.byte_variance, m.lz77_sample_ratio, choice
        );

        // Basic sanity on every file
        assert!(m.entropy >= 0.0 && m.entropy <= 8.0);
        assert!(m.ascii_ratio >= 0.0 && m.ascii_ratio <= 1.0);
        assert!(m.lz77_sample_ratio >= 0.0 && m.lz77_sample_ratio <= 1.0);
        assert!(m.autocorrelation >= -1.0 && m.autocorrelation <= 1.0);
    }
}
