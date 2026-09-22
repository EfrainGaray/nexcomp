# NEXCOMP

Adaptive lossless compressor that selects the best codec per block.

**WARNING: This is experimental software.** NEXCOMP is a research compressor, not production-hardened. APIs, file formats, and compression behavior may change between versions. Use at your own risk.

## Results

| Corpus     | NEXCOMP (bpb) | bzip2 (bpb) | gzip (bpb) |
|------------|---------------|-------------|------------|
| Calgary    | 2.072         | 2.109       | 2.592      |
| Canterbury | 1.278         | 1.545       | 2.072      |
| Silesia    | 2.022         | 2.057       | --         |

Decompression throughput: ~50 MB/s (bzip2: ~17 MB/s).
Compression throughput: ~1--3 MB/s.

## Installation

```
cargo build --release
```

The binary is placed at `target/release/nexcomp`. Requires Rust 1.75+.

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

## Benchmarks

To reproduce the benchmark numbers:

```
scripts/download_corpora.sh
scripts/benchmark.sh
```

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
| `crypto/`       | ChaCha20-Poly1305 encryption         |
| `neural/`       | Experimental neural context model    |

## Limitations

- **Compression speed is slow** (~1--3 MB/s). The adaptive selector trial-compresses each block with every codec.
- **Silesia gap.** On the Silesia corpus NEXCOMP only narrowly beats bzip2 and does not yet match brotli or xz.
- **Experimental format.** The file format is not stabilized. Files written by one version may not be readable by future versions.

## Tests

```
cargo test --release
```

375 tests passed, 0 failed.

## License

MIT
