# Changelog

## 1.8.0 — 2026-09-23

Everything an independent audit of 1.7.0 asked for before the format could be
called stable. Container magic `NX15`; `NX14` and `NX13` still decode.

### Correctness

- **The delta codec no longer depends on the compiler.** `normalize_freqs`
  handed the rounding remainder out in the order the standard library's
  unstable sort left tied symbols in, so a block written here failed to decode
  when the same source was built with rustc 1.75 or 1.80. Ties now break by
  symbol index; payloads carry bit `0x80` on their delta type to say so, and
  older ones keep decoding through the old derivation.
- **The compressor no longer panics on long runs.** A run of more than
  2,250,592 equal bytes fits in a block but in no RLE length class, and
  compressing a sparse file died with exit 101. Runs are split; a candidate
  codec that panics is now dropped instead of taking the process with it.
- **A zero-byte file is not an archive.** `compress` used to write nothing for
  an empty input and `decompress` accepted any zero-byte file as an empty
  original, so a truncated archive reported a successful restore.

### Resources

- `try_adaptive_decompress` grew the output as the container declared it: a
  2.8 MB file with 200,000 empty block headers asked for 781 GiB. Both
  in-memory entry points now grow with what actually decodes.
- Blocks are grouped so their estimated working memory stays under 2 GiB. Four
  4 MiB PPM blocks peaked at 5.3 GB on twelve threads and now peak at 1.5 GB,
  which is what one PPM block costs.

### Fixtures, fuzzing and docs

- The MANIFEST is complete by construction and the suite asserts it matches the
  files on disk: the 17 NX13 fixtures had fallen out of it and had not been
  decoded since `dc07175`.
- The fuzz targets reach the sizes the format allows — the container target
  capped output at 64 KiB and the block target took the block length as a u16,
  so neither had ever seen a multi-block file or a full 4 MiB block.
- Corrected claims: the Silesia leaderboard figures (TNSSRC 0.1.0 is
  43,724,575 and 28,261,094 is precomp + cmix v21), the MSRV (1.85, not 1.75,
  with the committed lock), the NXE3 length field (u64), the fixture count, and
  what "streams" means on the decoding side.

## 1.7.0 — 2026-09-22

The first release meant to be audited as a stable one: the formats are
specified, the decoder is bounded and fuzzed, and every published number is
measured by a script that records how it ran.

### Formats

- `NX14` (container with a whole-file BLAKE3) and `NXE3` (encrypted wrapper with
  its KDF costs in the header) are what this release writes. `NX13` and `NXE2`
  still decode. The pre-release magics `NXC\x01`, `NX12` and `NXE1` are refused
  with an error that points at [docs/FORMAT.md](docs/FORMAT.md).
- Committed fixtures in `tests/formats/` pin the bytes each format produces, and
  CI decodes them on both x86_64 and aarch64.

### Compression

- `stride_cm` gained a match model, and the encoder now keeps the smaller of the
  two model masks, so repetitive binaries win without costing pure numeric data.
- The LZMA parse keeps re-parsing while it pays instead of stopping at a fixed
  number of rounds.

### Decoder safety

- Every length a codec payload declares is checked against the container before
  the codec runs; a hostile block can no longer drive an allocation.
- The top distance slot no longer overflows the 1-based conversion: the distance
  is rejected instead. Found by `cargo fuzz`, pinned by
  `tests/fuzz_regressions.rs`.
- `cargo fuzz` targets for the container, the blocks and every codec, with a
  smoke run in CI.

### Build and CI

- `cargo clippy --all-targets -- -D warnings` is clean and enforced in CI.
- CI runs the suite on x86_64 and aarch64, has each architecture decode what the
  other wrote, runs the mutation harness and `cargo audit`.
