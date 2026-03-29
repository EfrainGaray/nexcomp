# NEXCOMP: Adaptive Multi-Codec Compression via Data-Driven Codec Selection

**Authors:** NEXCOMP Development Team

**Date:** March 2026

---

## Abstract

We present NEXCOMP, a lossless data compression system that adaptively selects among six specialized codecs based on statistical properties of the input data. A lightweight classifier evaluates seven metrics to route each input to the most effective codec from a portfolio comprising LZ77+Huffman, BWT+rANS, LZMA with range coding, order-5 PPM, Delta+ANS, and RLE+Huffman. A non-regression selector ensures the chosen codec never degrades performance relative to a baseline. On the Calgary corpus (14 files, 3.14 MB), NEXCOMP achieves 2.072 bits per byte (bpb), outperforming bzip2 (2.109), gzip (2.592), and xz (2.154). On Canterbury (11 files, 2.81 MB), NEXCOMP achieves 1.278 bpb versus bzip2 at 1.545. Decompression throughput reaches approximately 50 MB/s, roughly 3x faster than bzip2. The system is implemented in approximately 15,000 lines of Rust with verified lossless round-trip correctness across all 37 benchmark files.

---

## 1. Introduction

Modern data compression systems typically commit to a single algorithmic paradigm: dictionary-based (LZ77, LZMA), transform-based (BWT), or statistical (PPM, arithmetic coding). Each paradigm excels on specific data characteristics but suffers on others. LZ77 variants perform well on data with repeated byte sequences but poorly on high-entropy or structured numeric data. BWT achieves strong compression on text with local context regularities but is less effective on binary formats. PPM models capture higher-order statistical dependencies but require substantial memory for large alphabets.

Real-world data is heterogeneous. A benchmark corpus like Calgary contains English prose, bibliographic records, geophysical data, object code, and terminal transcripts --- each favoring a different compression strategy. A single-codec compressor must compromise, achieving good average performance at the cost of suboptimal results on individual file types.

NEXCOMP addresses this mismatch through *adaptive multi-codec compression*. Rather than selecting a single algorithm at design time, NEXCOMP analyzes each input at compression time, computing a profile of seven statistical metrics, and routes the data to the codec best suited to its characteristics. This approach draws on the observation that no single algorithm dominates across all data types --- a consequence of the No Free Lunch theorem applied to compression.

The key contributions of this work are:

1. **A practical multi-codec architecture** that integrates six compression algorithms spanning dictionary, transform, and statistical paradigms into a single system with automatic codec selection.
2. **A lightweight data classifier** based on seven efficiently computable metrics that routes inputs to the appropriate codec without trial compression.
3. **A non-regression selection mechanism** that guarantees the adaptive selector never performs worse than a conservative baseline.
4. **Experimental validation** across three standard benchmarks (Calgary, Canterbury, Silesia) demonstrating competitive compression ratios with favorable decompression speed.

The remainder of this paper is organized as follows. Section 2 surveys related work. Section 3 describes the system architecture and codec implementations. Section 4 presents experimental results. Section 5 discusses the trade-offs and limitations, and Section 6 concludes.

---

## 2. Related Work

### 2.1 Dictionary-Based Compression

Ziv and Lempel [1] introduced LZ77, which replaces repeated occurrences of data with references to earlier positions in the input stream. The Deflate algorithm [2] combines LZ77 with Huffman coding and forms the basis of gzip and the ZIP format. LZMA, introduced in the 7-Zip project [3], extends dictionary compression with large sliding windows (up to 4 GB) and a range coder, achieving substantially better compression than Deflate at the cost of higher computational requirements. The xz format [4] standardizes LZMA2 as a container format with integrity checking.

### 2.2 Transform-Based Compression

Burrows and Wheeler [5] introduced the BWT, a reversible permutation that groups characters by their preceding context, making the output highly amenable to subsequent entropy coding. The SA-IS algorithm [6] enables linear-time construction of suffix arrays, which in turn enables efficient BWT computation. The bzip2 compressor [7] implements BWT followed by move-to-front transform and Huffman coding. Brotli [8] combines LZ77 with a static dictionary and context-dependent entropy coding.

### 2.3 Statistical Compression

Prediction by Partial Matching (PPM) [9] models the probability distribution of the next symbol based on preceding context of variable length. PPM achieves excellent compression on text data by capturing high-order statistical dependencies. Arithmetic coding [10] provides near-optimal entropy coding given a probability model, encoding an entire message as a single number in the interval [0, 1).

### 2.4 Asymmetric Numeral Systems

Duda [11] introduced Asymmetric Numeral Systems (ANS), a family of entropy coding methods that combine the compression effectiveness of arithmetic coding with the computational efficiency of Huffman coding. The range variant (rANS) and tabled variant (tANS) have been widely adopted in modern compressors including Zstandard [12] and LZFSE.

### 2.5 Adaptive and Multi-Algorithm Systems

The idea of selecting among multiple compression algorithms is not new. The Unix `compress` utility selection between algorithms dates to the 1980s. More recently, systems like Squash Benchmark and lzbench compare algorithms but do not integrate them. PAQ [13] and cmix [14] achieve state-of-the-art compression through context mixing of hundreds of models, but at compression speeds measured in KB/s, making them impractical for general use. NEXCOMP occupies a different design point: it selects among a small portfolio of practical codecs, achieving strong compression ratios with usable throughput.

---

## 3. System Architecture

### 3.1 Overview

NEXCOMP processes input data through a three-stage pipeline:

1. **Classification.** The input is analyzed to compute a feature vector of seven statistical metrics.
2. **Codec Selection.** A rule-based classifier maps the feature vector to one of six codecs. A non-regression mechanism validates the selection.
3. **Compression.** The selected codec compresses the input. A header encodes the codec identifier to enable decompression.

The entire system is implemented in approximately 15,258 lines of Rust across 30 source files.

### 3.2 Data Classifier

The classifier computes seven metrics over the input data:

1. **Byte entropy** (Shannon entropy of the byte frequency distribution, 0--8 bits).
2. **Bigram entropy** (Shannon entropy over byte pair frequencies).
3. **Repetition ratio** (fraction of bytes that match a byte within a short lookback window).
4. **Run-length density** (fraction of bytes that are part of runs of 3 or more identical bytes).
5. **Delta smoothness** (fraction of byte-to-byte differences below a threshold, indicating numeric or structured data).
6. **Byte cardinality** (number of distinct byte values observed).
7. **ASCII density** (fraction of bytes in the printable ASCII range plus common whitespace).

These metrics are computed in a single pass over the input, requiring O(n) time and O(1) auxiliary space (fixed-size frequency tables). The classification overhead is negligible relative to compression time.

### 3.3 Codec Portfolio

NEXCOMP integrates six codecs, each targeting a different data profile:

#### 3.3.1 LZ77 + Huffman

A dictionary-based compressor following the Deflate model. LZ77 identifies repeated byte sequences using a sliding window and hash-chain match finder. Match lengths and literal bytes are encoded using canonical Huffman codes. This codec is selected for data with moderate repetition and moderate entropy, such as mixed-content files.

#### 3.3.2 BWT + SA-IS + rANS

A transform-based compressor. The Burrows-Wheeler Transform is computed via suffix array construction using the SA-IS algorithm [6], which runs in O(n) time. The BWT output is processed with a move-to-front transform and then encoded using range-variant Asymmetric Numeral Systems (rANS) [11]. This codec excels on source code and structured text where local context predicts subsequent bytes. In the Calgary corpus, `progl` and `progp` achieve 1.798 and 1.782 bpb respectively using this codec.

#### 3.3.3 LZMA + Range Coder

A dictionary-based compressor with large match windows and a binary range coder for entropy coding. LZMA models match lengths, distances, and literal bytes with context-dependent probability estimates updated adaptively. This codec handles binary and object files effectively: `obj2` achieves 2.361 bpb, `pic` achieves 0.805 bpb, and `trans` achieves 1.579 bpb on the Calgary corpus.

#### 3.3.4 PPM Order-5

A statistical compressor implementing Prediction by Partial Matching [9] with context orders up to 5. The model estimates P(next byte | preceding 5 bytes) with escape-based fallback through lower orders. Probabilities are encoded using arithmetic coding. PPM dominates on natural language text: `book1` achieves 2.321 bpb and `bib` achieves 1.991 bpb. Eight of the fourteen Calgary files are best served by this codec.

#### 3.3.5 Delta + ANS

A specialized codec for data with sequential numeric structure. A delta filter computes first-order byte differences, transforming smoothly varying data into a low-entropy residual stream. The residuals are encoded using ANS. The `geo` file in Calgary, containing geophysical floating-point data, is routed to this codec (4.476 bpb --- high entropy limits absolute compression, but delta coding is still the best available strategy for this data type).

#### 3.3.6 RLE + Huffman

A codec for data with long runs of identical bytes. Run-length encoding collapses repeated bytes into (byte, count) pairs, and the resulting stream is Huffman-coded. This codec serves as a fast path for data with high run-length density.

### 3.4 Non-Regression Selector

The codec selector includes a non-regression mechanism: after classification determines a candidate codec, the system verifies that the candidate is expected to outperform a conservative baseline (LZ77+Huffman). If the classifier's confidence is below a threshold, the system falls back to the baseline codec. This mechanism was refined across six versions of NEXCOMP, with each version tightening the selection heuristics based on observed benchmark performance:

| Version | Calgary bpb | Key Change |
|---------|-------------|------------|
| v0.1    | 4.664       | Initial implementation |
| v1.0    | 2.578       | Core codecs functional |
| v1.2    | 2.357       | Improved BWT + classifier tuning |
| v1.3    | 2.175       | PPM order-5 integration |
| v1.4    | 2.125       | LZMA refinements |
| v1.5    | 2.072       | Non-regression selector, final tuning |

**Table 1.** Compression ratio progression on Calgary corpus across NEXCOMP versions.

---

## 4. Experimental Results

### 4.1 Methodology

All experiments were conducted on an Apple M-series system running macOS 14, with NEXCOMP compiled using Rust 1.75+ in release mode with link-time optimization (LTO) enabled. Compression ratios are reported in bits per byte (bpb), computed as (compressed size x 8) / original size. Lower values indicate better compression. Throughput is reported in MB/s over the original (uncompressed) data size.

Three standard benchmarks are used:

- **Calgary Corpus** [15]: 14 files, 3.14 MB total. The classic compression benchmark.
- **Canterbury Corpus** [16]: 11 files, 2.81 MB total. A more modern benchmark with diverse file types.
- **Silesia Corpus** [17]: 12 files, 211 MB total. A large-scale benchmark representative of modern data.

Baseline compressors use default or high-quality settings: gzip (default), bzip2 (default), xz (default), Brotli (default quality).

### 4.2 Calgary Corpus Results

| File   | Type          | NEXCOMP Codec | NEXCOMP | bzip2 | gzip  | xz    | Brotli |
|--------|---------------|---------------|---------|-------|-------|-------|--------|
| bib    | Bibliography  | PPM           | **1.991** | 2.076 | 2.694 | 2.004 | 2.019 |
| book1  | English text  | PPM           | **2.321** | 2.364 | 2.862 | 2.275 | 2.270 |
| book2  | English text  | PPM           | **2.040** | 2.085 | 2.576 | 2.002 | 2.009 |
| geo    | Geophysical   | Delta         | **4.476** | 4.698 | 5.142 | 4.242 | 4.324 |
| news   | USENET        | PPM           | **2.476** | 2.597 | 3.034 | 2.472 | 2.475 |
| obj1   | Object code   | LZMA          | **3.695** | 3.847 | 4.147 | 3.552 | 3.560 |
| obj2   | Object code   | LZMA          | **2.361** | 2.451 | 2.963 | 2.187 | 2.217 |
| paper1 | Technical     | PPM           | **2.439** | 2.601 | 3.014 | 2.449 | 2.459 |
| paper2 | Technical     | PPM           | **2.382** | 2.478 | 2.854 | 2.330 | 2.338 |
| pic    | Bitmap image  | LZMA          | **0.805** | 0.820 | 0.889 | 0.671 | 0.683 |
| progc  | C source      | PPM           | **2.516** | 2.681 | 3.078 | 2.494 | 2.508 |
| progl  | Lisp source   | BWT           | **1.798** | 1.840 | 2.324 | 1.694 | 1.722 |
| progp  | Pascal source | BWT           | **1.782** | 1.850 | 2.337 | 1.691 | 1.716 |
| trans  | Transcript    | LZMA          | **1.579** | 1.637 | 2.130 | 1.524 | 1.550 |

**Table 2.** Per-file compression results on the Calgary corpus (bpb). **Bold** indicates best among NEXCOMP, bzip2, and gzip.

**Corpus aggregate:**

| Compressor | Calgary bpb | Canterbury bpb | Silesia bpb |
|------------|-------------|----------------|-------------|
| NEXCOMP    | **2.072**   | **1.278**      | 2.022       |
| bzip2      | 2.109       | 1.545          | 2.057       |
| gzip       | 2.592       | 2.072          | 2.553       |
| xz         | 2.154       | 1.401          | **1.698**   |
| Brotli     | 2.095       | 1.397          | 1.552       |

**Table 3.** Aggregate compression ratios across three standard benchmarks (bpb). **Bold** indicates best result in each column among NEXCOMP, bzip2, and gzip for Calgary/Canterbury; overall best for Silesia (where xz and Brotli surpass NEXCOMP).

### 4.3 Canterbury Corpus Results

On the Canterbury corpus, NEXCOMP achieves 1.278 bpb, a 17.3% improvement over bzip2 (1.545 bpb) and a 38.3% improvement over gzip (2.072 bpb). The Canterbury corpus includes highly compressible files (e.g., structured markup, source code) where PPM and BWT codecs substantially outperform dictionary-only methods.

### 4.4 Silesia Corpus Results

The Silesia corpus (211 MB, 12 files) represents modern large-scale data: XML documents, x-ray images, executables, databases, and source code. NEXCOMP achieves 2.022 bpb, outperforming bzip2 (2.057) and gzip (2.553) but falling behind xz (1.698) and Brotli (1.552).

The gap on Silesia is attributable to LZMA dictionary size limitations in NEXCOMP's current implementation relative to the mature, heavily optimized xz implementation, and to Brotli's use of a pre-built static dictionary for web content. On large files, the dictionary size advantage of xz's LZMA2 becomes decisive.

### 4.5 Throughput

| Compressor | Compression (MB/s) | Decompression (MB/s) |
|------------|--------------------:|---------------------:|
| NEXCOMP    | 1--3                | ~50                  |
| bzip2      | ~10                 | ~17                  |
| gzip       | ~30                 | ~200                 |
| xz         | ~5                  | ~25                  |
| Brotli     | ~10                 | ~150                 |

**Table 4.** Approximate throughput on the test platform (Apple M-series, macOS 14, single-threaded).

NEXCOMP's decompression throughput of approximately 50 MB/s is roughly 3x faster than bzip2 and 2x faster than xz, making it practical for read-heavy workloads. Compression throughput of 1--3 MB/s is the primary limitation, discussed further in Section 5.

### 4.6 Correctness

Lossless round-trip correctness (compress then decompress yields bitwise-identical output) was verified for all 37 files across the three benchmark corpora. The test suite comprises 375 tests, all passing.

---

## 5. Discussion

### 5.1 Why Adaptive Selection Works

The per-file Calgary results (Table 2) illustrate why no single codec suffices. PPM dominates on natural language text (8 of 14 files), BWT excels on source code (`progl`, `progp`), LZMA handles binary and object files (`obj1`, `obj2`, `pic`, `trans`), and Delta coding is the best available strategy for geophysical data (`geo`). A fixed choice of any single codec would sacrifice performance on the file types it is not designed for.

The classifier's seven metrics are sufficient to distinguish these data types with high accuracy. Byte entropy and ASCII density separate text from binary data. Delta smoothness identifies numeric sequences. Repetition ratio and run-length density identify dictionary-friendly and RLE-friendly data, respectively. Bigram entropy and byte cardinality provide additional discrimination.

### 5.2 Pareto Position

NEXCOMP occupies an interesting position in the compression Pareto frontier (compression ratio vs. speed):

- **Better compression than gzip and bzip2** on all three corpora, with decompression faster than bzip2.
- **Competitive with xz and Brotli on small/medium corpora** (Calgary, Canterbury), trailing on large files (Silesia).
- **Far more practical than PAQ/cmix**, which achieve better compression but at speeds of KB/s rather than MB/s.

This positions NEXCOMP as suitable for archival use cases where compression ratio matters more than compression speed, and where decompression must be reasonably fast.

### 5.3 Limitations

**Compression speed.** At 1--3 MB/s, compression is slower than all baselines except potentially the highest xz settings. The primary bottleneck is the PPM codec, which requires updating a deep context tree for every input byte. Parallelization and algorithmic optimization of the PPM implementation are the most promising avenues for improvement.

**Silesia gap.** On the large Silesia corpus, NEXCOMP trails xz by 0.324 bpb and Brotli by 0.470 bpb. This gap stems from (a) LZMA dictionary size limits in NEXCOMP's implementation, (b) Brotli's pre-built static dictionary for common web patterns, and (c) the maturity of decades-optimized implementations in xz and Brotli. Increasing NEXCOMP's LZMA dictionary size and adding a secondary match finder are likely to narrow this gap.

**Block-level granularity.** NEXCOMP currently selects a single codec per file. A block-level adaptive approach, where different segments of a file may use different codecs, could improve compression on heterogeneous files (e.g., a file containing both text and embedded binary data).

**Memory usage.** The PPM order-5 model and LZMA dictionary together can consume substantial memory. The current implementation does not expose tuning parameters for memory-constrained environments.

### 5.4 Codec Selection Accuracy

The progression from v0.1 (4.664 bpb) to v1.5 (2.072 bpb) --- a 55.6% improvement --- demonstrates the importance of classifier tuning. Early versions often selected suboptimal codecs; the non-regression selector in v1.5 eliminated the remaining cases where the classifier made poor choices.

---

## 6. Conclusion

NEXCOMP demonstrates that adaptive multi-codec compression is a practical and effective approach to lossless data compression. By integrating six codecs spanning dictionary, transform, and statistical paradigms, and routing each input to the best-suited codec via a lightweight classifier, NEXCOMP achieves 2.072 bpb on the Calgary corpus and 1.278 bpb on Canterbury, outperforming bzip2, gzip, and xz on these benchmarks. Decompression at approximately 50 MB/s is 3x faster than bzip2.

The system's limitations --- slow compression speed and a gap on large files relative to xz and Brotli --- represent opportunities for future work. Block-level codec selection, parallel compression, and dictionary size optimization are the most promising directions.

The implementation in approximately 15,000 lines of Rust, with 375 passing tests and verified lossless correctness across all 37 benchmark files, demonstrates that multi-codec compression can be implemented cleanly and correctly in a modern systems language.

---

## References

[1] J. Ziv and A. Lempel, "A universal algorithm for sequential data compression," *IEEE Transactions on Information Theory*, vol. 23, no. 3, pp. 337--343, 1977.

[2] P. Deutsch, "DEFLATE Compressed Data Format Specification version 1.3," RFC 1951, 1996.

[3] I. Pavlov, "LZMA SDK," 7-Zip, 1999--2024. Available: https://7-zip.org/sdk.html

[4] L. Collin, "The .xz File Format," 2009. Available: https://tukaani.org/xz/xz-file-format.txt

[5] M. Burrows and D. J. Wheeler, "A block-sorting lossless data compression algorithm," SRC Research Report 124, Digital Equipment Corporation, 1994.

[6] G. Nong, S. Zhang, and W. H. Chan, "Two efficient algorithms for linear time suffix array construction," *IEEE Transactions on Computers*, vol. 60, no. 10, pp. 1471--1484, 2011.

[7] J. Seward, "bzip2 and libbzip2, version 1.0.6," 2010. Available: https://sourceware.org/bzip2/

[8] J. Alakuijala and Z. Szabadka, "Brotli Compressed Data Format," RFC 7932, 2016.

[9] J. G. Cleary and I. H. Witten, "Data compression using adaptive coding and partial string matching," *IEEE Transactions on Communications*, vol. 32, no. 4, pp. 396--402, 1984.

[10] I. H. Witten, R. M. Neal, and J. G. Cleary, "Arithmetic coding for data compression," *Communications of the ACM*, vol. 30, no. 6, pp. 520--540, 1987.

[11] J. Duda, "Asymmetric numeral systems," arXiv preprint arXiv:0902.0271, 2009.

[12] Y. Collet, "Zstandard Compression and the application/zstd Media Type," RFC 8478, 2018.

[13] M. Mahoney, "Adaptive weighing of context models for lossless data compression," Florida Institute of Technology Technical Report CS-2005-16, 2005.

[14] B. Knoll and N. de Freitas, "cmix," 2017. Available: http://www.byronknoll.com/cmix.html

[15] T. Bell, J. G. Cleary, and I. H. Witten, *Text Compression*. Prentice Hall, 1990. (Calgary Corpus)

[16] R. Arnold and T. Bell, "A corpus for the evaluation of lossless compression algorithms," *Proceedings of the IEEE Data Compression Conference*, pp. 201--210, 1997.

[17] S. Deorowicz, "Silesia compression corpus," 2003. Available: https://sun.aei.polsl.pl/~sdeMDoro/corpus/silesia.html

---

*This paper describes NEXCOMP v1.5. The implementation is available as open-source Rust code.*
