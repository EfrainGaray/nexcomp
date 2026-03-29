// Reproduction test for tar block 0 corruption
use nexcomp::classifier::classify_block;
use nexcomp::transform::{apply_transform, inverse_transform};
use nexcomp::grammar::{repair_encode, repair_decode, repair_serialize, repair_deserialize};
use nexcomp::entropy::{normalize_freqs, build_table, build_decode_table, rans_encode, rans_decode};
use std::path::Path;

fn load_block0_fixture() -> Option<Vec<u8>> {
    let path = Path::new("/tmp/nxc_block0.bin");
    if !path.exists() {
        eprintln!("block0 fixture not found at {}, skipping", path.display());
        return None;
    }
    Some(std::fs::read(path).expect("read block0 fixture"))
}

#[test]
fn test_block0_transform_roundtrip() {
    let data = match load_block0_fixture() {
        Some(data) => data,
        None => return,
    };
    let domain = classify_block(&data).unwrap();
    let transformed = apply_transform(&data, domain);
    let recovered = inverse_transform(&transformed, domain);
    assert_eq!(data, recovered, "Transform alone corrupts data");
}

#[test]
fn test_block0_repair_roundtrip() {
    let data = match load_block0_fixture() {
        Some(data) => data,
        None => return,
    };
    let domain = classify_block(&data).unwrap();
    let transformed = apply_transform(&data, domain);
    
    let grammar = repair_encode(&transformed).unwrap();
    let decoded = repair_decode(&grammar);
    assert_eq!(transformed, decoded, "Re-Pair corrupts data");
}

#[test]
fn test_block0_full_pipeline() {
    let data = match load_block0_fixture() {
        Some(data) => data,
        None => return,
    };
    let domain = classify_block(&data).unwrap();
    let transformed = apply_transform(&data, domain);
    
    // Re-Pair
    let grammar = repair_encode(&transformed).unwrap();
    let grammar_bytes = repair_serialize(&grammar);
    
    // rANS
    let mut counts = vec![0u64; 256];
    for &b in &grammar_bytes { counts[b as usize] += 1; }
    let freqs = normalize_freqs(&counts, 256);
    let table = build_table(&freqs).unwrap();
    let encoded = rans_encode(&grammar_bytes, &table).unwrap();
    
    // Decode
    let dtable = build_decode_table(&table);
    let dec_grammar_bytes = rans_decode(&encoded, &dtable, grammar_bytes.len()).unwrap();
    assert_eq!(grammar_bytes, dec_grammar_bytes, "rANS corrupts grammar bytes");
    
    let dec_grammar = repair_deserialize(&dec_grammar_bytes).unwrap();
    let dec_transformed = repair_decode(&dec_grammar);
    assert_eq!(transformed, dec_transformed, "Re-Pair serialize/deserialize corrupts data");
    
    let recovered = inverse_transform(&dec_transformed, domain);
    assert_eq!(data, recovered, "Full pipeline corrupts data");
}
