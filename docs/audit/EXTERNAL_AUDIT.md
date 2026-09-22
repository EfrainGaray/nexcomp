# NEXCOMP — External Audit Brief

*The audit this brief asked for came back with twelve findings; the answer to each
one is in [RESPONSE.md](RESPONSE.md). This brief is kept as it was sent.*

## 1. What NEXCOMP is trying to be

NEXCOMP is a lossless compressor whose single goal is **the best compression ratio on the
market**. Compression speed is secondary. Decompression must stay exact, deterministic across
machines, and practical (seconds per MB at worst, never hours). Every design decision should be
judged against that goal: a finding that costs ratio needs a strong correctness or security
reason, and an opportunity that buys ratio is in scope even if it costs speed.

We want the auditor to tell us (a) whether the output is always lossless and safe to decode,
(b) whether our benchmark numbers hold up, and (c) where the largest remaining ratio gains are.

## 2. Current results

Sizes in bytes, measured on this commit's code (Apple x86_64, 12 threads). NEXCOMP totals include
all container framing. xz and bzip2 were run on the same machine; the leaderboard figures come
from Matt Mahoney's published tables (LTCB and Silesia pages, September 2026).

| corpus | original | NEXCOMP | bpb | xz -9e | bzip2 -9 | leaderboard reference |
|---|---|---|---|---|---|---|
| Silesia (12 files) | 211,938,580 | 45,708,355 | 1.725 | 48,456,100 | 54,506,769 | paq8px_v215 27,825,511 · bsc 46,723,436 · 7-Zip -mx=9 48,792,760 |
| enwik8 | 100,000,000 | 24,485,422 | 1.959 | 24,831,656 | 29,008,758 | cmix v21 14,623,723 |
| Calgary (18 files) | 3,251,493 | 780,028 | 1.919 | 883,896 | 866,501 | – |
| Canterbury (11 files) | 2,810,784 | 406,092 | 1.156 | 493,168 | 542,710 | – |

Silesia through the CLI: 120 s to compress, 38 s to decompress; peak RSS 1.96 GB (mozilla).
Per-file tables: `docs/research/predictor-context-experiment.md`.

**The gap that matters:** NEXCOMP is ahead of general-purpose tools (xz, bzip2, 7-Zip, bsc) but
about 1.6× larger than the context-mixing leaders (paq8px, cmix) on Silesia and enwik8.

## 3. Architecture

`src/adaptive.rs` splits the input into independent 4 MiB blocks, compresses each block with
several candidate codecs in parallel (rayon) and keeps the smallest, so no codec can make a block
larger than the best alternative. Codecs (id stored per block):

| id | name | module | notes |
|---|---|---|---|
| 0 | lz77huf | `src/lz77/` | baseline LZ77 + Huffman, always computed |
| 1 | lzma | `src/codecs/lzma_style.rs` | range coder; price-based optimal parse; binary-tree (BT3) match finder |
| 2 | delta | `src/codecs/delta_ans.rs` | delta filter + rANS |
| 3 | rlehuf | `src/codecs/rle_huffman.rs` | RLE + Huffman |
| 4 | store | – | raw |
| 5 | bwt | `src/codecs/bwt_codec.rs`, `bwt_cm.rs` | BWT (u32 SA-IS) + binary context mixing over the BWT output |
| 6 | ppm | `src/codecs/ppm.rs` | order-5 PPM with exclusion and update exclusion |
| 7 | stride-cm | `src/codecs/stride_cm.rs` | logistic context mixing with stride-aware prediction contexts; skipped on blocks > 85% printable ASCII |

A per-block BCJ (x86) filter flag is tried for executable-looking blocks.

**Container NX13:** `[4B "NX13"][8B total length][4B block count]`, then per block
`[1B codec][1B bcj][4B original length][4B payload length][4B CRC-32 of the original block][payload]`.
Decoding returns `Result`, verifies every length and the CRC, and wraps each block decoder in
`catch_unwind` so a codec assertion becomes an error.

**Encrypted wrapper NXE2** (`src/main.rs`, `src/crypto/mod.rs`): `[4B "NXE2"][4B input length]`
used as AEAD associated data, then `[32B salt][12B nonce][ChaCha20-Poly1305 ciphertext + tag]`;
key = Argon2id(password, salt) with the `argon2` crate defaults (19 MiB, 2 passes, 1 lane).

**Legacy formats:** `src/main.rs` still decodes the old `NXC\x01` files (v1 grammar/rANS path and
v2 selector path through `src/selector.rs`, `src/classifier/`, `src/transform/`, `src/grammar/`,
`src/ajedrez.rs`).

**Research-only code, not on the production path:** `src/codecs/minmask/` (mask search + sparse
XOR residuals, rejected; see `docs/research/minmask-syndrome-experiment.md`), `src/neural/`,
`src/context_model.rs`, `src/repair_fast.rs`, `src/lz77/optimal.rs`.

## 4. What changed since the audited baseline (`0076e42`)

`git log --oneline 0076e42..main` lists 27 commits. By area:

- **LZMA:** removed an MTF on literals that broke the matched-literal context; price-based optimal
  parsing with rep / short-rep / length-2 edges; BT3 match finder (4.4× faster, smaller output).
- **Container:** 4 MiB blocks with parallel selection; `Result`-based parsing with bounds and
  overflow checks; unknown codec ids rejected; CRC-32 per block; decoder panics contained.
- **BWT:** sentinel BWT; u32 SA-IS with buffers released before recursion; MTF/Huffman stage
  replaced by a binary context mixer; decoded output bounded by the declared length.
- **PPM:** packed u64 context keys, sparse tables, update exclusion (≈10× faster, up to 4.5% smaller).
- **New codec 7 (stride-cm):** −4.4% on Silesia, −5.0% on Canterbury, −2.6% on Calgary.
- **Crypto:** SHA-256 + HKDF replaced by Argon2id; wrapper magic NXE1 → NXE2.
- **RLE:** corrupt Huffman code tables return errors instead of panicking.

Internal reviews of every commit have been done; the only real defect they found (a panic on
hostile RLE Huffman tables) is fixed. We want an independent pass, not a confirmation.

## 5. Audit scope, in priority order

### P0 — Losslessness and decoder safety
1. For every codec, is the decoder the exact mirror of the encoder on **all** inputs (empty,
   1 byte, 4 MiB − 1, exact multiples of block sizes, all-equal, random, adversarial)?
   Property-based or differential fuzzing is welcome.
2. **Determinism across machines:** a file compressed on x86_64 must decode bit-exact on aarch64
   (and vice versa). All probability models are meant to be integer-only; confirm there is no
   floating point, `HashMap` iteration order, thread-count or rayon scheduling dependence in any
   *decoder* path. (The LZMA optimal parser uses `f32` prices, but only in the encoder.)
3. **Hostile input:** no panic, no hang, no unbounded allocation for any byte string passed to
   `try_adaptive_decompress` or the CLI. Include `cargo fuzz` targets if you can.
4. `stride_cm` (the newest code): encoder/decoder symmetry of `start_byte`/`predict`/`update`,
   index bounds, header validation.

### P0 — Encryption
5. Argon2id parameters, salt and nonce generation (`rand::thread_rng`), AEAD usage and associated
   data, error handling on wrong passwords and truncated files, and the password interface of
   the CLI (see Known issues).

### P1 — Compression ratio (the product goal)
6. Reproduce the numbers in section 2 and tell us if anything in the measurement is unfair
   (framing, block splitting, corpora versions, tool flags).
7. Review the selector heuristics in `select_best_codec` (the ASCII / entropy gates for BWT, PPM
   and stride-cm): which blocks are denied a candidate that would have won?
8. Rank the largest ratio opportunities with evidence. The ones we suspect:
   - a cmix/paq-class context mixer for text and executables (orders 1–N, word model, match
     model, mixer hierarchy, SSE chains), since text is where BWT/PPM plateau at ~1.84 bpb on
     dickens;
   - text preprocessing (dictionary / capital-letter transforms) before modelling;
   - x86 and structured-binary models beyond BCJ (mozilla, ooffice, samba are LZMA-bound);
   - richer 2-D contexts in stride-cm (mr, x-ray, sao);
   - removing the 4 MiB block boundary for long-range matches;
   - a learned model inside the mixer (the current LTCB leader combines a context mixer with a
     small quantized transformer). Tell us whether you think it is worth it for us, and in what order.

### P2 — Performance and resources
9. Peak memory (1.96 GB on a 51 MB file) and whether it is bounded per block; worst-case encode
   time per block; decode speed of the context-mixing codecs (~1 MB/s).

### P3 — Code health
10. Dead or research-only code compiled into the library, duplicated codec enums
    (`adaptive::CodecId` vs `classifier_v2::CodecChoice`), pre-existing clippy warnings, and the
    legacy `NXC\x01` decoding path.

## 6. Known issues (confirm, prioritise, or tell us we are wrong)

- **Password on the command line:** `--encrypt <password>` / `--decrypt <password>` expose the
  password in `ps` output and shell history.
- **Codec-internal lengths are trusted:** the container checks each block's final length and CRC,
  but a codec's own header (`ppm.rs:318`, `lzma_style.rs:762`, `rle_huffman.rs:314`, `:433`) can
  declare up to 4 GiB and the decoder reserves that before the container can reject it. Block
  lengths are also not capped at 4 MiB in `parse_blocks`.
- **Whole-file processing:** input and output are fully in memory; no streaming. The NXE2 header
  stores the input length as u32 (truncated above 4 GiB; it is only authenticated metadata).
- **Format stability:** NX13 and NXE2 are new in this release; files from earlier builds (NX12,
  NXE1) are rejected with an error. There is no format version negotiation beyond the magic.
- **Version strings:** `Cargo.toml` says 1.5.0, the CLI still says 1.2.0.
- **Speed cost of stride-cm:** it is tried on every non-text block, including binaries it never
  wins (mozilla, ooffice), which costs encode time.
- **History size:** build artifacts (`target/`) were committed with the baseline; they are no
  longer tracked, but they remain in git history (~406 MB `.git`).

## 7. How to build, test and reproduce

```
cargo build --release                 # binary: target/release/nexcomp
cargo test --release                  # 407 tests pass; 13 long benchmarks are #[ignore]

target/release/nexcomp compress   in.bin out.nxc --verbose
target/release/nexcomp decompress out.nxc restored.bin
target/release/nexcomp inspect    out.nxc --show-codec

# corpora: same sources as scripts/download_corpora.sh
#   calgary/ canterbury/ silesia/ enwik8/ under one directory
export NEXCOMP_CORPORA_DIR=/path/to/corpora
cargo test --release --test predictor_context_experiment -- --ignored --nocapture --test-threads=1 --exact corpora
cargo test --release --test predictor_context_experiment -- --ignored --nocapture --test-threads=1 --exact numeric_synthetic
cargo test --release --test predictor_context_experiment -- --ignored --nocapture --test-threads=1 --exact ablation
```

## 8. What we need back

- **Findings**, each with severity (critical / high / medium / low), file:line, a minimal input or
  command that reproduces it, and a suggested fix.
- **Benchmark verification:** your own numbers for section 2 (same corpora, same tool flags), with
  hardware and commit.
- **Ratio roadmap:** the top opportunities from item 8 (or others you find), each with an estimated
  gain on Silesia and enwik8, the evidence behind the estimate, and a rough effort.
- Anything that would stop a user from trusting a `.nxc` file for long-term storage.
