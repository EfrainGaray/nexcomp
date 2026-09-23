//! Golden fixtures (external audit F-06, F-09): files written by earlier
//! versions must keep decoding to the exact same bytes, on any machine.
//!
//! `tests/formats/MANIFEST` lists every fixture with the length and BLAKE3 of
//! its original. Directories the current writer no longer produces (a codec
//! that got better makes smaller files) keep their bytes: `nx14-pre-reparse`
//! is what 1.6.0 wrote before the LZMA re-parse iterated, and
//! `nx14-pre-pinned-table` is a delta block whose rANS table predates the
//! pinned tie order. The fixtures are frozen: regenerating them needs
//! `NEXCOMP_REGENERATE_FIXTURES=1`, and a fixture whose bytes change is a
//! compatibility break, not a test to update.
//!
//!   cargo test --release --test format_fixtures -- --ignored write_format_fixtures

use nexcomp::adaptive::{self, encode_with, try_adaptive_decompress, CodecId, BLOCK_SIZE};
use nexcomp::codecs::bcj_filter;
use nexcomp::crypto;
use std::path::{Path, PathBuf};

/// The password of the encrypted fixtures; they hold public test data.
const FIXTURE_PASSWORD: &[u8] = b"nexcomp-fixture";

fn formats_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/formats")
}

fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *state >> 33
}

/// Inputs that between them reach every codec and both block layouts.
fn fixture_inputs() -> Vec<(&'static str, Vec<u8>)> {
    let mut seed = 3;
    let text: Vec<u8> = {
        let words = ["the ", "compressor ", "block ", "of ", "and ", "model ", "context ", "mixing\n"];
        let mut out = Vec::new();
        while out.len() < 32768 {
            out.extend_from_slice(words[(lcg(&mut seed) % words.len() as u64) as usize].as_bytes());
        }
        out.truncate(32768);
        out
    };
    let numeric: Vec<u8> = (0..8192u32).flat_map(|i| (1000 + 3 * i).to_le_bytes()).collect();
    let runs: Vec<u8> = (0..1024).flat_map(|i| vec![(i % 7) as u8; 32]).collect();
    let random: Vec<u8> = (0..4096).map(|_| lcg(&mut seed) as u8).collect();
    let exe: Vec<u8> = (0..4096u32)
        .flat_map(|i| [[0xE8].as_slice(), &(i * 16).to_le_bytes(), &[0x55, 0x48, 0x89, 0xE5]].concat())
        .collect();
    let multiblock: Vec<u8> = b"nexcomp multi block fixture ".iter().copied().cycle().take(BLOCK_SIZE + 100).collect();
    // Two blocks where several codecs come out nearly the same size: if the
    // writer ever depends on the machine, this is where it shows.
    let zeros = vec![0u8; 2 * BLOCK_SIZE];
    vec![
        ("empty", Vec::new()),
        ("one-byte", vec![0x42]),
        ("text", text),
        ("numeric", numeric),
        ("runs", runs),
        ("random", random),
        ("exe", exe),
        ("multiblock", multiblock),
        ("zeros", zeros),
    ]
}

/// The input each forced-codec fixture uses.
fn codec_input(codec: CodecId, inputs: &[(&'static str, Vec<u8>)]) -> Vec<u8> {
    let named = |name: &str| inputs.iter().find(|(n, _)| *n == name).unwrap().1.clone();
    match codec {
        CodecId::DeltaAns | CodecId::StrideCm => named("numeric"),
        CodecId::RleHuffman => named("runs"),
        CodecId::Passthrough => named("random"),
        _ => named("text"),
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, e) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    !data.iter().fold(!0u32, |c, &b| table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8))
}

/// A container holding `data` in one block forced to `codec`, optionally
/// through the BCJ filter. Written by hand so every codec has a fixture, not
/// only the ones the selector picks.
fn forced_container(magic: &[u8; 4], data: &[u8], codec: CodecId, bcj: bool) -> Vec<u8> {
    assert!(data.len() <= BLOCK_SIZE);
    let filtered = if bcj { bcj_filter::bcj_encode(data) } else { data.to_vec() };
    let payload = encode_with(codec, &filtered).expect("codec encodes the fixture input");
    let mut out = magic.to_vec();
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.push(codec as u8);
    out.push(u8::from(bcj));
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&payload);
    if magic != b"NX13" {
        out.extend_from_slice(blake3::hash(data).as_bytes());
    }
    out
}

/// Every fixture of the current build: (relative path, original bytes).
fn fixtures_to_write() -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let inputs = fixture_inputs();
    let mut out = Vec::new();
    let format = adaptive::CONTAINER_MAGIC;
    let dir = std::str::from_utf8(format).unwrap().to_ascii_lowercase();
    for (name, data) in &inputs {
        out.push((format!("{dir}/{name}.nxc"), data.clone(), adaptive::adaptive_compress(data)));
    }
    for id in 0..8u8 {
        let codec = CodecId::from_u8(id).unwrap();
        let data = codec_input(codec, &inputs);
        let file = forced_container(format, &data, codec, false);
        out.push((format!("{dir}/codec-{}.nxc", codec.name()), data, file));
    }
    let exe = inputs.iter().find(|(n, _)| *n == "exe").unwrap().1.clone();
    let bcj = forced_container(format, &exe, CodecId::LzmaStyle, true);
    out.push((format!("{dir}/bcj-lzma.nxc"), exe, bcj));

    // Encrypted wrappers around the text fixture.
    let text = inputs.iter().find(|(n, _)| *n == "text").unwrap().1.clone();
    let payload = adaptive::adaptive_compress(&text);
    let nxe2_header = [&crypto::NXE2_MAGIC[..], &(text.len() as u32).to_le_bytes()].concat();
    let nxe2 = [
        nxe2_header.clone(),
        crypto::encrypt(&payload, FIXTURE_PASSWORD, &nxe2_header).expect("nxe2 fixture"),
    ]
    .concat();
    out.push(("nxe2/text.nxc".to_string(), text.clone(), nxe2));
    // Cheap costs: the fixture pins the format, not the cost of the default.
    let kdf = crypto::KdfParams { m_cost_kib: 8192, t_cost: 1, p_cost: 1 };
    let nxe3 = crypto::seal(&payload, FIXTURE_PASSWORD, text.len() as u64, kdf).expect("nxe3 fixture");
    out.push(("nxe3/text.nxc".to_string(), text, nxe3));
    out
}

/// Decode a fixture whatever its format.
fn decode_fixture(file: &[u8]) -> Vec<u8> {
    let payload = if crypto::is_sealed(file) {
        crypto::open(file, FIXTURE_PASSWORD).expect("fixture decrypts")
    } else {
        file.to_vec()
    };
    try_adaptive_decompress(&payload).expect("fixture decodes")
}

#[test]
fn fixtures_decode_to_their_recorded_bytes() {
    let manifest = std::fs::read_to_string(formats_dir().join("MANIFEST")).expect("MANIFEST exists");
    let mut checked = 0;
    let mut listed: Vec<String> = Vec::new();
    for line in manifest.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let (path, len, hash) = (
            fields.next().expect("path"),
            fields.next().expect("length").parse::<usize>().expect("length"),
            fields.next().expect("hash"),
        );
        let file = std::fs::read(formats_dir().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let decoded = decode_fixture(&file);
        assert_eq!(decoded.len(), len, "{path}: length");
        assert_eq!(blake3::hash(&decoded).to_hex().as_str(), hash, "{path}: contents");
        checked += 1;
        listed.push(path.to_string());
    }
    // A fixture that falls out of the MANIFEST stops being checked without
    // anything failing, which is how the NX13 set went unread for a while.
    let mut on_disk = fixture_files();
    on_disk.sort();
    listed.sort();
    assert_eq!(listed, on_disk, "the MANIFEST and tests/formats have drifted apart");
    assert_eq!(checked, on_disk.len());
}

/// Every `.nxc` under `tests/formats`, as a path relative to that directory.
fn fixture_files() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "nxc") {
                out.push(path.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    let root = formats_dir();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

/// The writer still produces the committed bytes; on another architecture this
/// also shows its output does not depend on the machine.
#[test]
fn the_writer_still_produces_the_committed_fixtures() {
    for (path, original, file) in fixtures_to_write() {
        let full = formats_dir().join(&path);
        // Encrypted fixtures carry a random salt and nonce; only their plaintext is stable.
        if crypto::is_sealed(&file) {
            assert_eq!(decode_fixture(&std::fs::read(&full).unwrap()), original, "{path}");
            continue;
        }
        let committed = std::fs::read(&full).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(file, committed, "{path}: the writer's output changed");
    }
}

#[test]
#[ignore]
fn write_format_fixtures() {
    let regenerate = std::env::var("NEXCOMP_REGENERATE_FIXTURES").is_ok_and(|v| v == "1");
    let mut manifest = String::from("# format fixtures: path, original length, BLAKE3 of the original\n");
    for (path, original, file) in fixtures_to_write() {
        let full = formats_dir().join(&path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        if full.exists() && !regenerate {
            println!("kept    {path}");
        } else {
            std::fs::write(&full, &file).unwrap();
            println!("written {path} ({} bytes)", file.len());
        }
        manifest.push_str(&format!("{path} {} {}\n", original.len(), blake3::hash(&original).to_hex()));
    }
    // Every other fixture on disk — the frozen sets — gets its line from the
    // file itself, so the MANIFEST cannot quietly lose one.
    let written: Vec<String> = manifest
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    let mut frozen: Vec<String> = fixture_files().into_iter().filter(|p| !written.contains(p)).collect();
    frozen.sort();
    for path in frozen {
        let file = std::fs::read(formats_dir().join(&path)).unwrap();
        let original = decode_fixture(&file);
        manifest.push_str(&format!("{path} {} {}\n", original.len(), blake3::hash(&original).to_hex()));
        println!("frozen  {path}");
    }
    std::fs::write(formats_dir().join("MANIFEST"), manifest).unwrap();
}
