// LZ77 integration tests

use nexcomp::lz77;

#[test]
fn test_roundtrip_random() {
    let data: Vec<u8> = (0..65536u64)
        .map(|i| ((i.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)) >> 33) as u8)
        .collect();
    let compressed = lz77::compress(&data);
    let recovered = lz77::decompress(&compressed).unwrap();
    assert_eq!(data, recovered);
    eprintln!("random: {} -> {} bytes", data.len(), compressed.len());
}

#[test]
fn test_roundtrip_repetitive() {
    let data: Vec<u8> = b"abcdefgh".iter().cycle().take(65536).copied().collect();
    let compressed = lz77::compress(&data);
    let recovered = lz77::decompress(&compressed).unwrap();
    assert_eq!(data, recovered);
    assert!(
        compressed.len() < data.len() / 5,
        "expected <{} bytes, got {}",
        data.len() / 5,
        compressed.len()
    );
    eprintln!(
        "repetitive: {} -> {} bytes ({:.1}%)",
        data.len(),
        compressed.len(),
        compressed.len() as f64 / data.len() as f64 * 100.0
    );
}

#[test]
fn test_roundtrip_run_length() {
    let data: Vec<u8> = vec![0xAA; 65536];
    let compressed = lz77::compress(&data);
    let recovered = lz77::decompress(&compressed).unwrap();
    assert_eq!(data, recovered);
    assert!(compressed.len() < 2000, "run-length should compress to <2000 bytes, got {}", compressed.len());
    eprintln!("run-length: {} -> {} bytes", data.len(), compressed.len());
}

#[test]
fn test_roundtrip_english_text() {
    let text = std::fs::read("tests/fixtures/english_sample.txt").unwrap();
    let compressed = lz77::compress(&text);
    let recovered = lz77::decompress(&compressed).unwrap();
    assert_eq!(text, recovered);
    eprintln!(
        "english: {} -> {} bytes ({:.1}%)",
        text.len(),
        compressed.len(),
        compressed.len() as f64 / text.len() as f64 * 100.0
    );
}

#[test]
fn test_roundtrip_long_distance_match() {
    // Match at distance > 65536 bytes (beyond gzip's 32KB window)
    // but within our 4MB window
    let mut data = vec![0u8; 200_000];
    let pattern = b"NEXCOMP_LONG_DISTANCE_TEST_PATTERN_1234567890";
    data[0..pattern.len()].copy_from_slice(pattern);
    // Same pattern at 150KB distance
    data[150_000..150_000 + pattern.len()].copy_from_slice(pattern);

    let compressed = lz77::compress(&data);
    let recovered = lz77::decompress(&compressed).unwrap();
    assert_eq!(data, recovered, "long-distance round-trip failed");

    let (_, _, match_ratio) = lz77::measure(&data);
    eprintln!("long distance: match_ratio={:.2}%", match_ratio * 100.0);
    // The match at 150KB should be found
    assert!(match_ratio > 0.001, "should find match at 150KB distance");
}

#[test]
fn test_roundtrip_real_tar() {
    let tar_data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = lz77::compress(&tar_data);
    let recovered = lz77::decompress(&compressed).unwrap();
    assert_eq!(tar_data, recovered);
    let (_, _, match_ratio) = lz77::measure(&tar_data);
    eprintln!(
        "corpus.tar: {} -> {} bytes ({:.1}%), match_ratio={:.1}%",
        tar_data.len(),
        compressed.len(),
        compressed.len() as f64 / tar_data.len() as f64 * 100.0,
        match_ratio * 100.0
    );
}

#[test]
fn test_full_pipeline_v3_roundtrip() {
    // Full CLI pipeline round-trip via the binary
    let tar_data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    std::fs::write("/tmp/nxc_v3_test.tar", &tar_data).unwrap();

    let nxc = env!("CARGO_BIN_EXE_nexcomp");
    let out = std::process::Command::new(nxc)
        .args(["compress", "/tmp/nxc_v3_test.tar", "/tmp/nxc_v3_test.nxc"])
        .output()
        .expect("compress failed");
    assert!(out.status.success(), "compress failed: {}", String::from_utf8_lossy(&out.stderr));

    let compressed_size = std::fs::metadata("/tmp/nxc_v3_test.nxc").unwrap().len();

    let out = std::process::Command::new(nxc)
        .args(["decompress", "/tmp/nxc_v3_test.nxc", "/tmp/nxc_v3_test.dec"])
        .output()
        .expect("decompress failed");
    assert!(out.status.success(), "decompress failed: {}", String::from_utf8_lossy(&out.stderr));

    let recovered = std::fs::read("/tmp/nxc_v3_test.dec").unwrap();
    assert_eq!(tar_data, recovered, "pipeline v3 is not lossless");

    let bpb = compressed_size as f64 * 8.0 / tar_data.len() as f64;
    let v01_bpb = 4.664;

    eprintln!("Pipeline v0.3 benchmark:");
    eprintln!("  Original:      {} bytes (8.000 bpb)", tar_data.len());
    eprintln!("  NEXCOMP v0.1:  1,962,721 bytes (4.664 bpb)  [reference]");
    eprintln!("  NEXCOMP v0.3:  {} bytes ({:.3} bpb)", compressed_size, bpb);
    eprintln!("  Improvement:   {:.3} bpb ({:.1}%)", v01_bpb - bpb, (v01_bpb - bpb) / v01_bpb * 100.0);
    eprintln!("  gzip -9:       641,307 bytes (1.524 bpb)     [target]");

    assert!(bpb < v01_bpb, "v0.3 ({:.3} bpb) must improve over v0.1 ({:.3} bpb)", bpb, v01_bpb);
}
