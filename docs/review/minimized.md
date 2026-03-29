# Minimized Versions

## Abstract (≤100 words)

NEXCOMP is a lossless compressor (~15K lines Rust, zero compression library dependencies) that runs 6 codecs per block—PPM-5, BWT+rANS, LZMA, Delta+ANS, RLE+Huffman, Passthrough—and emits the smallest output. Compression speed is 1–3 MB/s; decompression reaches ~50 MB/s (3x bzip2's ~17 MB/s). On standard benchmarks: Calgary 2.072 bpb (bzip2: 2.109), Canterbury 1.278 bpb (bzip2: 1.545), Silesia 2.022 bpb (bzip2: 2.057). Compression is slow. The adaptive selector adds overhead proportional to codec count. Lossless correctness verified across 375 tests covering 37 files.

Word count: 88

## README intro (≤200 words)

NEXCOMP compresses each block with 6 independent codecs and keeps the smallest result.

| Codec | Method |
|---|---|
| PPM-5 | Prediction by partial matching, order 5 |
| BWT+rANS | Burrows-Wheeler + asymmetric numeral systems |
| LZMA | Lempel-Ziv-Markov chain |
| Delta+ANS | Delta filter + entropy coding |
| RLE+Huffman | Run-length encoding + Huffman |
| Passthrough | Emit raw bytes when nothing helps |

**Benchmarks (bits per byte, lower is better):**

| Corpus | NEXCOMP | bzip2 |
|---|---|---|
| Calgary | 2.072 | 2.109 |
| Canterbury | 1.278 | 1.545 |
| Silesia | 2.022 | 2.057 |

**Speeds:** Compression 1–3 MB/s. Decompression ~50 MB/s (bzip2: ~17 MB/s).

**Limitations:** Compression is slow—each block runs all 6 codecs. Not suitable for real-time or streaming use cases. Decompression is fast because only the winning codec runs.

**Implementation:** ~15K lines Rust, no compression library dependencies. 375 tests across 37 files verify lossless round-trip correctness.

```
cargo build --release
./target/release/nexcomp compress input.bin -o output.nx
./target/release/nexcomp decompress output.nx -o restored.bin
```

Word count: 148

## Cuts made
- "We present a novel" → DELETE — filler, adds no information
- "It is worth noting that" → DELETE — throat-clearing
- "state-of-the-art" → specific bpb numbers — adjective replaced with data
- "significantly outperforms" → exact bpb comparisons — let numbers speak
- "In this paper" → DELETE — obvious from context
- "our approach" → "NEXCOMP" — be specific
- "achieves competitive results" → exact bpb per corpus — vague claim replaced with measurements
- "fast decompression" → "~50 MB/s (bzip2: ~17 MB/s)" — quantify the claim
- Limitations placed before benchmark results in abstract — reader knows trade-offs upfront
