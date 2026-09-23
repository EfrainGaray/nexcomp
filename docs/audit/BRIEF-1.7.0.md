# NEXCOMP 1.7.0 — brief for the second external audit

The first audit ([EXTERNAL_AUDIT.md](EXTERNAL_AUDIT.md)) was asked for while the
formats were still moving; its twelve findings and our answer to each are in
[RESPONSE.md](RESPONSE.md). This brief asks for the audit of a release we intend
to call stable: the formats are specified and pinned by fixtures, the decoder is
bounded and fuzzed, and every number below comes from a script that records how
it ran.

Audit this tag: **v1.7.0**. What we want to hear is (a) whether a `.nxc` file
written by this release can be trusted for long-term storage, (b) whether the
numbers hold up on your hardware, and (c) what is still missing before this can
be called stable without a warning in the README.

## 1. Results

Sizes in bytes, whole files, container framing and the per-file hash included.
Every file was compressed and decompressed on its own, and a result was only
recorded once the restored bytes hashed to the original.

| corpus | original | NEXCOMP | bpb | xz -9e | bzip2 -9 |
|---|---|---|---|---|---|
| Calgary (standard 14) | 3,141,622 | 743,525 | 1.8934 | 843,828 | 828,347 |
| Canterbury (11) | 2,810,784 | 403,833 | 1.1494 | 493,080 | 542,710 |
| Silesia (12) | 211,938,580 | 45,456,856 | 1.7159 | 48,456,004 | 54,506,769 |
| enwik8 | 100,000,000 | 24,485,454 | 1.9588 | 24,831,648 | 29,008,758 |

Measured by `scripts/benchmark.sh`; the artifact with the environment, the
per-file table and the tool versions is [`bench/results/20260923T020706Z-darwin-x86_64.md`](../../bench/results/20260923T020706Z-darwin-x86_64.md).

Against the published leaderboards (Matt Mahoney's tables, September 2026, not
measured here): on Silesia NEXCOMP sits below bee -m3 (45,622,742) and
freearc -m9 (45,542,009), above TNSSRC (45,267,065) and Tangelo 2.3
(44,037,765), with paq8px_v215 (27,825,511) and cmix (28,261,094) another 17 MB
below. The gap that matters is against the context-mixing leaders, not against
the general-purpose tools.

## 2. What changed since the audited baseline

The first audit read `6d55700`. Since then, besides the twelve findings:

- **Formats.** `NX14` (container + whole-file BLAKE3) and `NXE3` (encrypted
  wrapper with its KDF costs in an authenticated header) are what this release
  writes; `NX13` and `NXE2` still decode. The pre-release magics `NXC\x01`,
  `NX12` and `NXE1` are refused, on purpose — the reasoning is under F-06 in
  RESPONSE.md, and it is the one recommendation we did not follow.
- **Fixtures.** `tests/formats/` pins the bytes of every readable format, one
  file per codec plus BCJ, multi-block and the encrypted wrappers;
  `tests/formats/MANIFEST` carries the length and BLAKE3 of each original. The
  suite decodes all of them on every build and every architecture, and the
  writer test fails if this build would produce different bytes.
- **Ratio.** `stride_cm` gained a match model and the encoder keeps the smaller
  of the two model masks; the LZMA parse keeps re-parsing while it pays.
- **Decoder.** One overflow found by `cargo fuzz` after the first audit: the top
  distance slot overflowed the 1-based conversion (a panic under overflow
  checks, a wrapped zero in release). Fixed and pinned by
  `tests/fuzz_regressions.rs`.
- **Build.** `cargo clippy --all-targets -- -D warnings` is clean and enforced
  in CI, on x86_64 and aarch64.

## 3. Scope, in priority order

### P0 — losslessness and decoder safety
1. Is each decoder the exact mirror of its encoder on all inputs — empty, one
   byte, 4 MiB − 1, exact block multiples, all-equal, random, adversarial?
2. Determinism across machines: a file written on x86_64 must decode bit-exact
   on aarch64. Decoders are meant to be integer-only, with no dependence on
   thread count, rayon scheduling or hash iteration order.
3. Hostile input: no panic, no hang, no unbounded allocation for any byte string
   given to `try_adaptive_decompress`, `decompress_to` or the CLI. Our gate
   asks for 24 h of fuzzing per target with no crash; we have about 40 minutes per target so far
   (`fuzz/`, four targets), which is the one gate criterion still open.
4. The newest code: the `stride_cm` match model and the mask choice in
   `encode()`, the re-parse loop in `lzma_style`.

### P0 — encryption
5. `src/crypto/`: Argon2id costs in the NXE3 header and the bounds that refuse a
   hostile one, salt and nonce from `OsRng`, AEAD associated data, key
   zeroization, and the CLI password interface (`--password-file`,
   `NEXCOMP_PASSWORD`, no-echo prompt; the argv form warns).

### P1 — the numbers
6. Reproduce section 1 on your hardware and tell us if anything in the
   measurement is unfair: framing, block splitting, corpus versions, tool flags.
7. `select_best_codec` gates each candidate by ASCII ratio, entropy and block
   size. `tests/selector_oracle.rs` says the gates cost nothing on Calgary,
   Canterbury and canterbury-large; tell us where that experiment is too narrow.

### P1 — ratio roadmap
8. Rank what would buy the most, with evidence. What we suspect, in order: a
   general context mixer with a match model for text and executables; text
   preprocessing before modelling; binary models beyond BCJ; richer 2-D contexts
   in `stride_cm`; long-range matching across the 4 MiB block boundary.

### P2 — resources
9. Peak RSS is 2.1 GB compressing Silesia (the largest single file is 51 MB) and
   `RAYON_NUM_THREADS` is what bounds it. Is that bound the right one, and is
   the per-block worst case what we think it is?

### P3 — code health
10. Research-only code still compiled into the library (`src/codecs/minmask/`,
    `src/neural/`, `src/repair_fast.rs`, `src/grammar/`, `src/ajedrez.rs`), the
    two codec enums (`adaptive::CodecId` and `classifier_v2::CodecChoice`), and
    the `#[allow]`s this release added to get clippy clean.

## 4. Known issues — confirm, prioritise, or tell us we are wrong

- **24 h fuzzing is not done.** See P0 item 3.
- **Whole-file input.** Compression reads the whole input into memory; only
  decompression streams block by block. The NXE3 header stores the input length
  as u32, so it is wrong above 4 GiB (authenticated metadata only, the decoder
  does not rely on it).
- **Compression is slow** — Silesia takes about 9 minutes here, decompression
  about 2. The context-mixing codecs are tried on blocks they rarely win.
- **Young formats.** NX14 and NXE3 were introduced in 1.6.0 and nothing but this
  project writes them.
- **Git history** still carries the `target/` directory that was committed
  before the first audit (~406 MB `.git`).

## 5. How to build, test and reproduce

```
cargo build --release                        # target/release/nexcomp
cargo test --release -- --skip benchmark     # the suite CI runs
cargo clippy --release --all-targets -- -D warnings

NEXCOMP_MUTATIONS=5000 cargo test --release --test mutation_decode
cargo +nightly fuzz run decompress           # also: block, codec-roundtrip, roundtrip

bash scripts/corpora.sh                      # fetches and verifies against bench/manifest.tsv
bash scripts/benchmark.sh                    # writes bench/results/<UTC>-<os>-<arch>.{md,tsv}
```

## 6. What we need back

- **Findings**, each with severity, `file:line`, a minimal input or command that
  reproduces it, and a suggested fix.
- **Your own numbers** for section 1, with hardware and commit.
- **The ratio roadmap** from item 8, each item with an estimated gain on Silesia
  and enwik8, the evidence, and a rough effort.
- Anything that would stop someone trusting a `.nxc` file for long-term storage.
