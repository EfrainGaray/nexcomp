# NEXCOMP

Adaptive lossless compressor that selects the best codec per block.

1.7.0 is the first release meant to be used as a stable one: the formats are specified and pinned
by fixtures, the decoder is bounded and fuzzed, and every number below comes from a script that
records how it ran. A release may stop writing a file format but never stops reading one an
earlier release wrote ([docs/FORMAT.md](docs/FORMAT.md)); the CLI and the library API can still
change between versions. It remains a research compressor: it is slow to compress, it has not
been through an independent audit of this release, and nothing but this project writes NX15.

## Results

Sizes in bytes; whole files, framing and the per-file hash included. Every file was compressed
and decompressed on its own, and only recorded once the restored bytes hashed to the original.

| corpus | original | NEXCOMP | bpb | xz | bzip2 |
|---|---|---|---|---|---|
| calgary | 3141622 | 743525 | 1.8934 | 843828 | 828347 |
| canterbury | 2810784 | 403833 | 1.1494 | 493080 | 542710 |
| silesia | 211938580 | 45456856 | 1.7159 | 48456004 | 54506769 |
| enwik8 | 100000000 | 24485454 | 1.9588 | 24831648 | 29008758 |

Measured by `scripts/benchmark.sh` on the 1.8.0 build; environment, tool versions and per-file
numbers in [`bench/results/20260923T181910Z-darwin-x86_64.md`](bench/results/20260923T181910Z-darwin-x86_64.md).

NEXCOMP is ahead of the general-purpose tools and behind the context-mixing leaders. On Silesia its
45,456,856 bytes sit below bee -m3 -d8 (45,622,742) and freearc -m9 (45,542,009), above tangelo 2.3
(44,037,765) and TNSSRC 0.1.0 (43,724,575), with paq8px_v215 -12L at 27,825,511 and
precomp v0.4.7 -cn | cmix v21 at 28,261,094 another 17.6 MB below. Those figures are from Matt
Mahoney's published Silesia table, not measured here.

## Installation

```
cargo build --release
```

The binary is placed at `target/release/nexcomp`. Requires Rust 1.85+ (the locked
dependencies are edition 2024).

## Usage

**Compress a file:**

```
nexcomp compress input.txt output.nxc
```

**Decompress:**

```
nexcomp decompress output.nxc restored.txt
```

**Verbose mode** (shows per-block codec and ratio):

```
nexcomp compress input.txt output.nxc --verbose
```

**Encrypt/decrypt** (ChaCha20-Poly1305, key from Argon2id with the costs stored in the file):

```
nexcomp compress input.txt output.nxc --encrypt                 # prompts twice
nexcomp compress input.txt output.nxc --encrypt --password-file pw.txt
NEXCOMP_PASSWORD=... nexcomp decompress output.nxc restored.txt
```

Encrypted input is detected automatically; the password comes from `--password-file`,
`NEXCOMP_PASSWORD` or a prompt. Passing it as `--encrypt <pw>` / `--decrypt <pw>` still works but
prints a warning, since it shows up in `ps` and shell history.

**Inspect** a compressed file's metadata:

```
nexcomp inspect output.nxc
nexcomp inspect output.nxc --show-codec
```

## Architecture

NEXCOMP splits input into blocks and runs an adaptive selector that trial-compresses each block with multiple codecs, keeping the smallest result. A no-regression guarantee ensures every block is at least as good as the baseline LZ77+Huffman path.

```
Input -> [Block Splitter] -> [Adaptive Selector] -> Best codec per block -> Output
                                  |
                  Trial-compress with all codecs,
                  pick smallest, verify vs baseline
```

### Codecs

| ID | Codec       | Description                        |
|----|-------------|------------------------------------|
| 0  | lz77huf     | LZ77 + blocked Huffman (baseline)  |
| 1  | lzma        | LZMA-style range coding            |
| 2  | delta       | Delta filter + ANS                 |
| 3  | rlehuf      | RLE + Huffman                      |
| 4  | store       | Passthrough (incompressible data)  |
| 5  | bwt         | Burrows-Wheeler + rANS             |
| 6  | ppm         | Prediction by partial matching     |
| 7  | stride-cm   | Context mixing with stride, record and plane contexts |

Each block also records a CRC-32, and the file a BLAKE3 of the whole original.
The container and the encrypted wrapper are specified in [docs/FORMAT.md](docs/FORMAT.md).

## Benchmarks

```
scripts/corpora.sh                 # download and verify against bench/manifest.tsv
scripts/benchmark.sh               # writes bench/results/<timestamp>-<os>-<arch>.{tsv,md}
```

Every result artifact records the commit, the full `rustc -Vv`, the machine, the thread count and
the version of every tool it ran, and each file is only recorded once the restored bytes hash to
the original. The corpora are pinned by SHA-256 in `bench/manifest.tsv`; Calgary is the standard
14 files.

## Components

| Module          | Role                                 |
|-----------------|--------------------------------------|
| `adaptive`      | Block-level codec selector           |
| `classifier_v2` | Heuristic block classifier           |
| `codecs/`       | Codec implementations                |
| `entropy/`      | rANS entropy coder                   |
| `grammar/`      | Re-Pair grammar compression          |
| `lz77/`         | LZ77 encoder + Huffman backend       |
| `transform/`    | Domain-specific reversible transforms|
| `crypto/`       | ChaCha20-Poly1305 with Argon2id      |
| `neural/`       | Experimental neural context model    |

## Limitations

- **Compression is slow.** The selector trial-compresses each block with several codecs, and the
  context-mixing codecs are the slowest of them. Decompression of a block costs about what its
  codec cost to encode, so the context-mixing blocks are the slow ones there too.
- **The ratio gap that matters** is against the context-mixing leaders (paq8px, cmix), not against
  the general-purpose tools. On Silesia it is concentrated in `mozilla`, `webster` and `samba`.
- **Memory.** Compression holds several blocks and their candidates at once; `RAYON_NUM_THREADS`
  bounds it. Decompression writes block by block, and groups them so their working memory stays
  under 2 GiB — but a single PPM block costs about 1.5 GB on its own, and the archive itself is
  read into memory, so a `.nxc` larger than RAM cannot be decompressed.
- **Young format.** NX15 is new in 1.8.0, NXE3 in 1.6.0. From here on a release may stop writing a
  format but never stops reading one an earlier release wrote; the pre-release `NXC\x01`, `NX12`
  and `NXE1` files are refused, see [docs/FORMAT.md](docs/FORMAT.md).

## Tests

```
cargo test --release                        # the suite, including the format fixtures
NEXCOMP_MUTATIONS=5000 cargo test --release --test mutation_decode
cargo +nightly fuzz run decompress          # also: block, codec-roundtrip, roundtrip
```

## License

MIT
