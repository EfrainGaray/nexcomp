# Reader Report

**Date**: 2026-03-29
**Reviewer**: Automated critical reader (Claude)
**Scope**: docs/paper/nexcomp_paper.md, README.md, cross-checked against src/main.rs and test/benchmark data

---

## Critical (blocks publication)

[P1] **Paper does not exist.** The file `docs/paper/nexcomp_paper.md` was not found. No paper has been written yet. The paper must be created before any publication or distribution can proceed.

[P2] **README does not exist.** The file `README.md` at the project root was not found (only `NEXCOMP_DESIGN.md` exists). A user-facing README is required for any public release.

[P3] **No verified benchmark results on disk.** There are no saved benchmark result files (CSV, JSON, or text) from actual runs. The design document (`NEXCOMP_DESIGN.md`) contains tables where most NEXCOMP rows are marked `[EST]` (estimated), not measured. The benchmark scripts (`scripts/benchmark.sh`, `scripts/benchmark_v12.sh`) exist but no output artifacts were found. Any paper must use measured numbers, not estimates.

---

## Major (should fix)

[P4] **Design doc NEXCOMP numbers are all estimates.** In NEXCOMP_DESIGN.md section 4, every NEXCOMP row is tagged `[EST]`. Specific examples:
- "NEXCOMP-e3: 2.400 bpb [EST]" and "NEXCOMP-full: 1.200 bpb [EST]" on enwik8
- "NEXCOMP-e3: 2.080 bpb [EST]" on Silesia
- "NEXCOMP-e3: 2.240 bpb [EST]" on Calgary
- The design doc admits "NEXCOMP-e3 en MVP solo tiene Re-Pair + rANS, resultando en ~2.4 bpb, que es *peor* que xz (1.80 bpb)"

**Fix**: Run `scripts/benchmark_v12.sh` with all three corpora present, capture output, and use only measured values in any paper or README.

[P5] **Version mismatch between Cargo.toml and CLI.** `Cargo.toml` declares `version = "0.1.0"` but `src/main.rs` line 59 declares `version = "1.2.0"`. The design doc references both "v0.1" and "v1.2". When the paper and README are written, one canonical version must be used.

**Fix**: Align Cargo.toml version with the CLI version string, or vice versa. The paper and README must use the same version.

[P6] **Neural model (Stage 4) is not implemented.** The design doc describes a "Transformer autoregresivo (decoder-only)" with 8M parameters as Stage 4, but `src/neural/` exists as a module stub. The actual v1.2 pipeline uses an `adaptive` selector choosing among classical codecs (LZ77+Huffman variants, rANS, passthrough). Any paper must NOT claim neural compression capability unless it is implemented and benchmarked. The "NEXCOMP-full" rows in the design doc are entirely hypothetical.

**Fix**: The paper must describe only the implemented pipeline (classifier -> adaptive codec selector -> LZ77/Huffman/rANS). Neural claims belong in a "Future Work" section only.

[P7] **Target benchmark numbers for cross-document consistency do not yet have source data.** The task specifies these numbers must match everywhere:
- Calgary bpb = 2.072
- Canterbury bpb = 1.278
- Silesia bpb = 2.022
- Test count = 375
- Lossless count = 37

None of these numbers appear in any existing file in the repository. The v1.1 baseline numbers in `tests/v12_benchmark.rs` show different per-file bpb values (e.g., Calgary/bib = 2.515, Canterbury/kennedy.xls = 0.863). The aggregate corpus bpb values (2.072, 1.278, 2.022) cannot be verified without running the benchmarks. When the paper is written, these numbers MUST come from an actual benchmark run, not be invented.

**Fix**: Run the full benchmark suite, record results, then derive all document numbers from that single authoritative source.

---

## Minor (nice to fix)

[P8] **CLI inspect subcommand has --show-codec and --decrypt flags not documented in design doc.** The design doc section 3.4 only shows `compress` and `decompress` subcommands. The `inspect` subcommand (with `--show-codec` and `--decrypt` flags) is implemented in `src/main.rs` lines 82-88 but not documented. The README should document all three subcommands.

**Fix**: When writing the README, document:
```
nexcomp compress   <input> <output> [--encrypt <pw>] [--verbose]
nexcomp decompress <input> <output> [--decrypt <pw>]
nexcomp inspect    <input> [--decrypt <pw>] [--show-codec]
```

[P9] **Benchmark script `scripts/benchmark.sh` uses Ubuntu-specific commands.** It calls `sudo apt-get install`, `stat -c%s` (GNU stat), and `md5sum` (GNU coreutils). On macOS (the current platform), these will fail. The README should note platform requirements or the script should be made portable.

**Fix**: Note in README that `benchmark.sh` requires Linux. The `benchmark_v12.sh` script is more portable (uses `wc -c` instead of `stat`).

[P10] **No Canterbury corpus floor value in benchmark_v12.sh.** The script sets a floor of `1.807` for Canterbury TAR but individual Canterbury files have no floor checks (unlike Calgary files which have per-file floors). This asymmetry should be noted.

[P11] **Design doc references (to verify when paper is written).** The following references are cited in NEXCOMP_DESIGN.md and must be verified in any paper:
- Shannon 1948 -- real
- Kolmogorov 1965 -- real
- Deletang et al. ICLR 2024 -- must verify: "Language Modeling Is Compression" (actually published 2024, check exact venue)
- Charikar et al. 2005 IEEE Trans. IT -- real (Smallest Grammar Problem)
- Larsson & Moffat 2000 -- real (Re-Pair, published in JACM or Proc. IEEE DCC)
- Nong et al. 2009 -- real (SA-IS algorithm)
- Kim et al. ESA 2024 -- must verify: Re2Pair paper, check if actually ESA 2024
- Bellard 2021 NNCP v2 -- must verify exact year and publication
- Li et al. 2025 -- must verify: LLM compression paper, check Table 2 claim
- Pelton et al. 2015 Gorilla -- actually Pelton et al. at VLDB, verify author name (likely "Tuomas Pelkonen" not "Pelton")

---

## Verification checklist

These items should be re-checked once the paper and README are written:

### Paper checks
- [ ] Paper file exists at docs/paper/nexcomp_paper.md
- [ ] Abstract is 150 words or fewer
- [ ] Every number is from a measured benchmark run (no [EST] tags)
- [ ] Hardware spec is present (CPU, RAM, OS, Rust version)
- [ ] "Bold = best" is correct in all comparison tables
- [ ] Limitations section exists and is honest (no neural model, classical codecs only)
- [ ] No claim of beating xz/brotli/zstd unless measured data supports it
- [ ] All references are real with correct author names, years, and venues
- [ ] Calgary aggregate bpb matches verified benchmark output
- [ ] Canterbury aggregate bpb matches verified benchmark output
- [ ] Silesia aggregate bpb matches verified benchmark output

### README checks
- [ ] README.md exists at project root
- [ ] "Experimental" or "Research" warning appears before any benchmark results
- [ ] CLI commands match src/main.rs (compress, decompress, inspect with correct flags)
- [ ] Installation instructions include: Rust toolchain version, cargo build --release
- [ ] Limitations are visible (not buried at bottom), covering: no neural model, single-threaded, no streaming
- [ ] All benchmark numbers match the paper exactly

### Cross-document consistency
- [ ] Calgary bpb is identical in paper and README (target: 2.072, pending verification)
- [ ] Canterbury bpb is identical in paper and README (target: 1.278, pending verification)
- [ ] Silesia bpb is identical in paper and README (target: 2.022, pending verification)
- [ ] Test count is identical everywhere (target: 375, pending verification)
- [ ] Lossless verification count is identical everywhere (target: 37, pending verification)
- [ ] Version number is consistent (resolve 0.1.0 vs 1.2.0 conflict first)

### Source code verification
- [x] CLI subcommands: compress, decompress, inspect (verified in src/main.rs)
- [x] compress flags: --encrypt, --verbose (verified lines 68-75)
- [x] decompress flags: --decrypt (verified lines 77-81)
- [x] inspect flags: --decrypt, --show-codec (verified lines 82-88)
- [x] Lossless round-trip logic present (verified: decompress_data + size check)
- [x] Encryption uses ChaCha20-Poly1305 via crypto module (verified line 10, line 510)
- [ ] Benchmark numbers match actual run output (cannot verify -- no saved results)
