# Changelog

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
