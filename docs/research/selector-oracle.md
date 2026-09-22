# What the selector's gates cost

The block selector does not try every codec on every block: gates on the ASCII
ratio, the entropy and the block size decide which candidates are worth
encoding. The audit's question (F-11) is what those gates deny.

`tests/selector_oracle.rs` answers it by brute force. For every block it runs
**all eight codecs, with and without the BCJ filter, with no gates at all**, and
compares the smallest of those sixteen with what the selector actually produced.
The selector's own choice is one of the sixteen, so the oracle can only be
smaller or equal, and the gap is exactly what the gates cost.

```
NEXCOMP_CORPORA_DIR=~/corpora cargo test --release --test selector_oracle \
  -- --ignored --nocapture --test-threads=1 oracle
```

It reads every file in each corpus directory (so Calgary appears here with the
18 files the archive ships, not the standard 14 the benchmark uses).

## Result, 2026-09-22

Run on commit `db38422`, 33 blocks, 8.6 MB of input:

| corpus | files | blocks | selector | ungated oracle | missed |
|---|---|---|---|---|---|
| canterbury-large | 3 | 4 | 2,253,427 | 2,253,427 | 0 |
| calgary | 18 | 18 | 779,488 | 779,488 | 0 |
| canterbury | 11 | 11 | 405,762 | 405,762 | 0 |
| **total** | 32 | 33 | **3,438,677** | **3,438,677** | **0** |

Not one block would have been smaller with a codec the gates denied.

**Why `canterbury-large` is the corpus that matters here.** The gates were tuned
on Calgary (`tests/threshold_tuning.rs`) and the stride-cm gate was set from
Silesia and Canterbury winners, so a good result there proves little. E. coli
(DNA, 4.6 MB), the bible and world192 were never used for any threshold, and the
gates cost nothing on them either.

## What this does not say

- The corpora here are text, source, a spreadsheet, a bitmap and DNA. It says
  nothing about the block types the oracle could not afford to cover: the
  Silesia binaries (`mozilla`, `ooffice`, `samba`) are 4 MiB blocks where a full
  ungated run costs about 16 encodes each, minutes per block.
- It measures the gates, not the codec set. A codec that does not exist cannot
  be denied, and the gap to the context-mixing leaders is exactly that.
- The gates still pay for themselves in time: the ungated oracle took 273 s for
  what the selector compresses in a few seconds.
