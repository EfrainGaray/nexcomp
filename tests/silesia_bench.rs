use nexcomp::adaptive::{adaptive_compress, adaptive_decompress, CodecId};
use std::path::Path;

const SILESIA_DIR: &str = "/tmp/nexcomp_corpora/silesia";
const SILESIA_FILES: &[&str] = &[
    "dickens", "mozilla", "mr", "nci", "ooffice", "osdb",
    "reymont", "samba", "sao", "webster", "xml", "x-ray"
];

#[test]
fn silesia_full_benchmark() {
    if !Path::new(SILESIA_DIR).exists() {
        eprintln!("Silesia not found, skipping");
        return;
    }

    eprintln!("\n{}", "=".repeat(110));
    eprintln!("  SILESIA CORPUS — NEXCOMP v1.5 vs bzip2-9 vs brotli-11");
    eprintln!("{}", "=".repeat(110));
    eprintln!("{:<12} {:>10} {:>10} {:>6} {:>10} {:>6} {:>10} {:>6} {:>7} {:>4} {:>7} {:>7}",
        "File", "Orig", "NXC", "bpb", "bzip2", "bpb", "brotli", "bpb", "codec", "BCJ", "Dbz2", "Dbrotli");

    let mut tot_orig = 0usize;
    let mut tot_nxc = 0usize;
    let mut tot_bz = 0usize;
    let mut tot_br = 0usize;

    for &f in SILESIA_FILES {
        let path = format!("{}/{}", SILESIA_DIR, f);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let orig = data.len();

        // NEXCOMP
        let compressed = adaptive_compress(&data);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(data, decompressed, "LOSSLESS FAIL: {}", f);

        // Header: [4B "NX13"][4B orig_len LE][1B codec_id][1B bcj_flag][compressed_data]
        let codec = CodecId::from_u8(compressed[8]);
        let bcj_flag = compressed[9] != 0;
        let nxc_size = compressed.len();

        // bzip2
        let bz_path = format!("/tmp/silesia_{}.bz2", f);
        std::process::Command::new("bzip2").args(["-9", "-c", &path])
            .stdout(std::fs::File::create(&bz_path).unwrap())
            .status().unwrap();
        let bz_size = std::fs::metadata(&bz_path).unwrap().len() as usize;

        // brotli
        let br_path = format!("/tmp/silesia_{}.br", f);
        std::process::Command::new("brotli").args(["-q", "11", "-c", &path])
            .stdout(std::fs::File::create(&br_path).unwrap())
            .status().unwrap();
        let br_size = std::fs::metadata(&br_path).unwrap().len() as usize;

        let nxc_bpb = nxc_size as f64 * 8.0 / orig as f64;
        let bz_bpb = bz_size as f64 * 8.0 / orig as f64;
        let br_bpb = br_size as f64 * 8.0 / orig as f64;

        let bcj_str = if bcj_flag { "YES" } else { "no" };

        eprintln!("{:<12} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>10} {:>6.3} {:>7} {:>4} {:>+7.3} {:>+7.3}",
            f, orig, nxc_size, nxc_bpb, bz_size, bz_bpb, br_size, br_bpb,
            codec.name(), bcj_str, nxc_bpb - bz_bpb, nxc_bpb - br_bpb);

        tot_orig += orig;
        tot_nxc += nxc_size;
        tot_bz += bz_size;
        tot_br += br_size;
    }

    let nxc_bpb = tot_nxc as f64 * 8.0 / tot_orig as f64;
    let bz_bpb = tot_bz as f64 * 8.0 / tot_orig as f64;
    let br_bpb = tot_br as f64 * 8.0 / tot_orig as f64;

    eprintln!("{}", "-".repeat(110));
    eprintln!("{:<12} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>10} {:>6.3} {:>7} {:>4} {:>+7.3} {:>+7.3}",
        "TOTAL", tot_orig, tot_nxc, nxc_bpb, tot_bz, bz_bpb, tot_br, br_bpb,
        "", "", nxc_bpb - bz_bpb, nxc_bpb - br_bpb);
}
