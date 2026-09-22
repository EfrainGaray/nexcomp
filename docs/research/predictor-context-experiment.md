# Predictor as context: a stride-aware context-mixing codec

Follow-up to `minmask-syndrome-experiment.md`, whose evidence pointed here: MinMask won only
where a cheap predictor was exact, and lost everywhere else because XOR throws away how
reliable the prediction is per bit. This experiment keeps the predictors and uses them as
*contexts* of a binary context mixer instead of subtracting them.

Branch `research/predictor-context` (from `research/minmask`), codec `src/codecs/stride_cm.rs`,
experiments `tests/predictor_context_experiment.rs`.

## Executive Summary

**It works, on the data the MinMask evidence pointed to.** As one more candidate in NEXCOMP's
per-block selector, the stride-aware context mixer makes no block larger and cuts the totals by
4.38% on Silesia (47 801 850 → 45 708 127 bytes), 4.97% on Canterbury and 2.62% on Calgary;
enwik8 is unchanged. It wins every block of mr (−33%), pic/ptt5 (−28%), kennedy.xls (−19%),
geo (−15%), x-ray (−13%), osdb (−12%) and sao (−4%), and never a text block. On the numeric
sets where MinMask lost (noisy sine, random walk) it now wins by 24–27%, and on the counter it
matches MinMask's near-free cost (1 039 bytes against NEXCOMP's 18 023).

**Decision: INTEGRATED** as `CodecId::StrideCm = 7`, tried on blocks with at most 85% printable
ASCII. The price is speed on the files it wins: it codes at about 1 MB/s in both directions, so
Silesia takes 120 s instead of about 74 s to compress and 38 s instead of about 7 s to
decompress. With it, NEXCOMP's Silesia total (45.7 MB) passes bsc (46.7 MB) and 7-Zip -mx=9
(48.8 MB) on Mahoney's Silesia table; the top entries (paq8px, cmix, about 28 MB) remain far
ahead.

## Design

Each byte is coded MSB-first as 8 binary decisions with the carry-less arithmetic coder from
`bwt_cm`. The probability of every bit comes from an integer logistic mixer (lpaq-style
`stretch`/`squash`, weights selected by the byte's position inside the element and the partial
byte) over these inputs:

| input | context |
|---|---|
| order 0, order 1 | partial byte; previous byte + partial byte |
| orders 2, 3, 4, 6 | hash of the previous 2/3/4/6 bytes + partial byte |
| column | byte one element back B[i−s], position inside the element |
| linear | 2·B[i−s] − B[i−2s], position inside the element |
| delta | B[i−s] − B[i−2s], position, previous byte |
| plane | B[i−s] + B[i−r] − B[i−r−s] (left + up − up-left), position, high nibble of B[i−r] |
| expected-bit models (column, linear, plane) | the predicted byte's next bit, whether the partial byte still agrees with the prediction, bit position, element position, and how wrong the predictor was one element back (5 classes) |

The value contexts learn transitions for each predicted value; the expected-bit models are the
"predictor as context" proper: a few thousand direct slots that learn, per byte position and
recent error, how often the prediction is right, and generalize across all predicted values
immediately (the same idea as LZMA's matched literal). Slots hold a 22-bit probability and a hit
count with adaptive rate 1/(n + 1.5), capped at 127 hits. One order-0 APM refines the mixer
output (final = (3·mixer + APM) / 4; an order-1 APM measured worse).

The element stride s ∈ {1, 2, 3, 4, 6, 8, 12, 16} is the one whose linear extrapolation has the
smallest mean error on the block; the record length r ≤ 4096 (row width for images, record
size for tables) is the distance with the smallest mean byte difference on a sample. Both go in
the 8-byte header, so the decoder never searches.

**Tuning** (on 1 MiB samples of mr, x-ray, sao, osdb, mozilla, geo, pic, obj2, book1,
kennedy.xls and four numeric sets): mixer learning rate 2^-11 (−0.2% vs 2^-10), hit cap 127
(−0.1% vs 255), and weighting the mixer over the APMs (−7.4%, the largest single change).

## Numeric synthetic sets

Sizes in bytes including container framing; `params` are the detected stride and record length.

| dataset | orig | NEXCOMP | stride CM | params | hybrid | hybrid vs NEXCOMP | lossless |
|---|---|---|---|---|---|---|---|
| u32 counter, 256 KiB | 262144 | 18023 | 1039 | s=4 r=1024 | 1039 | -94.2% | true |
| u16 sine + noise, 256 KiB | 262144 | 85876 | 65624 | s=2 r=2010 | 65624 | -23.6% | true |
| i32 random walk, 256 KiB | 262144 | 35197 | 25756 | s=4 r=12 | 25756 | -26.8% | true |
| u64 timestamps +1000±8, 256 KiB | 262144 | 62469 | 22116 | s=8 r=520 | 22116 | -64.6% | true |
| f32 damped oscillation, 256 KiB | 262144 | 182575 | 122324 | s=4 r=1676 | 122324 | -33.0% | true |
| u8 image 512x512, smooth + noise | 262144 | 153522 | 114700 | s=1 r=512 | 114700 | -25.3% | true |

## Corpus Results

`stride CM` is the codec alone on every 4 MiB block; `hybrid` is NEXCOMP with the stride CM
as one more candidate per block, which is exactly what the integrated selector does. `blocks
won` counts container blocks where the stride CM was smaller.

#### Calgary

| file | orig | NEXCOMP | bpb | stride CM | bpb | params | hybrid | bpb | blocks won | hybrid vs NEXCOMP | CM enc / dec MB/s | lossless |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| bib | 111261 | 26485 | 1.904 | 27303 | 1.963 | s=1 r=3 | 26485 | 1.904 | 0/1 | +0.00% | 1.12 / 1.20 | true |
| book1 | 768771 | 211405 | 2.200 | 220371 | 2.293 | s=4 r=433 | 211405 | 2.200 | 0/1 | +0.00% | 0.54 / 0.55 | true |
| book2 | 610856 | 145677 | 1.908 | 152292 | 1.994 | s=3 r=14 | 145677 | 1.908 | 0/1 | +0.00% | 0.56 / 0.58 | true |
| geo | 102400 | 56690 | 4.429 | 47947 | 3.746 | s=4 r=12 | 47947 | 3.746 | 1/1 | -15.42% | 1.10 / 1.09 | true |
| news | 377109 | 112031 | 2.377 | 116222 | 2.466 | s=1 r=4 | 112031 | 2.377 | 0/1 | +0.00% | 0.65 / 0.70 | true |
| obj1 | 21504 | 9554 | 3.554 | 11064 | 4.116 | s=4 r=92 | 9554 | 3.554 | 0/1 | +0.00% | 0.88 / 1.21 | true |
| obj2 | 246814 | 63934 | 2.072 | 75500 | 2.447 | s=4 r=96 | 63934 | 2.072 | 0/1 | +0.00% | 0.84 / 0.88 | true |
| paper1 | 53161 | 15831 | 2.382 | 16701 | 2.513 | s=1 r=1000 | 15831 | 2.382 | 0/1 | +0.00% | 1.14 / 1.26 | true |
| paper2 | 82199 | 24047 | 2.340 | 25941 | 2.525 | s=12 r=824 | 24047 | 2.340 | 0/1 | +0.00% | 1.04 / 1.11 | true |
| paper3 | 46526 | 15250 | 2.622 | 16278 | 2.799 | s=3 r=423 | 15250 | 2.622 | 0/1 | +0.00% | 1.00 / 1.08 | true |
| paper4 | 13286 | 4885 | 2.941 | 5151 | 3.102 | s=1 r=919 | 4885 | 2.941 | 0/1 | +0.00% | 1.19 / 1.24 | true |
| paper5 | 11954 | 4555 | 3.048 | 4846 | 3.243 | s=1 r=2915 | 4555 | 3.048 | 0/1 | +0.00% | 1.17 / 1.25 | true |
| paper6 | 38105 | 11745 | 2.466 | 12185 | 2.558 | s=1 r=1144 | 11745 | 2.466 | 0/1 | +0.00% | 1.16 / 1.21 | true |
| pic | 513216 | 44220 | 0.689 | 31947 | 0.498 | s=1 r=216 | 31947 | 0.498 | 1/1 | -27.75% | 1.06 / 1.19 | true |
| progc | 39611 | 12048 | 2.433 | 12503 | 2.525 | s=1 r=27 | 12048 | 2.433 | 0/1 | +0.00% | 0.97 / 0.81 | true |
| progl | 71646 | 15141 | 1.691 | 15499 | 1.731 | s=1 r=3 | 15141 | 1.691 | 0/1 | +0.00% | 1.13 / 1.26 | true |
| progp | 49379 | 10465 | 1.695 | 11255 | 1.823 | s=1 r=394 | 10465 | 1.695 | 0/1 | +0.00% | 1.16 / 1.17 | true |
| trans | 93695 | 17009 | 1.452 | 18072 | 1.543 | s=1 r=3 | 17009 | 1.452 | 0/1 | +0.00% | 1.14 / 1.19 | true |
| **total** | 3251493 | 800972 | 1.971 | 821077 | 2.020 | | 779956 | 1.919 | 2/18 | -2.62% | | |

#### Canterbury

| file | orig | NEXCOMP | bpb | stride CM | bpb | params | hybrid | bpb | blocks won | hybrid vs NEXCOMP | CM enc / dec MB/s | lossless |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| alice29.txt | 152089 | 40508 | 2.131 | 41223 | 2.168 | s=1 r=5 | 40508 | 2.131 | 0/1 | +0.00% | 0.90 / 1.07 | true |
| asyoulik.txt | 125179 | 37450 | 2.393 | 38554 | 2.464 | s=1 r=3870 | 37450 | 2.393 | 0/1 | +0.00% | 1.08 / 1.13 | true |
| cp.html | 24603 | 7124 | 2.316 | 7607 | 2.474 | s=1 r=5 | 7124 | 2.316 | 0/1 | +0.00% | 1.13 / 1.22 | true |
| fields.c | 11150 | 2986 | 2.142 | 3143 | 2.255 | s=1 r=2234 | 2986 | 2.142 | 0/1 | +0.00% | 1.21 / 1.27 | true |
| grammar.lsp | 3721 | 1153 | 2.479 | 1227 | 2.638 | s=1 r=0 | 1153 | 2.479 | 0/1 | +0.00% | 1.28 / 1.30 | true |
| kennedy.xls | 1029744 | 48130 | 0.374 | 39146 | 0.304 | s=2 r=13 | 39146 | 0.304 | 1/1 | -18.67% | 0.77 / 0.80 | true |
| lcet10.txt | 426754 | 99546 | 1.866 | 103098 | 1.933 | s=1 r=3 | 99546 | 1.866 | 0/1 | +0.00% | 0.69 / 0.70 | true |
| plrabn12.txt | 481861 | 134663 | 2.236 | 137722 | 2.287 | s=1 r=44 | 134663 | 2.236 | 0/1 | +0.00% | 0.68 / 0.69 | true |
| ptt5 | 513216 | 44220 | 0.689 | 31947 | 0.498 | s=1 r=216 | 31947 | 0.498 | 1/1 | -27.75% | 1.11 / 1.21 | true |
| sum | 38240 | 9915 | 2.074 | 13368 | 2.797 | s=12 r=60 | 9915 | 2.074 | 0/1 | +0.00% | 1.14 / 1.19 | true |
| xargs.1 | 4227 | 1610 | 3.047 | 1899 | 3.594 | s=4 r=0 | 1610 | 3.047 | 0/1 | +0.00% | 1.25 / 1.31 | true |
| **total** | 2810784 | 427305 | 1.216 | 418934 | 1.192 | | 406048 | 1.156 | 2/11 | -4.97% | | |

#### Silesia

| file | orig | NEXCOMP | bpb | stride CM | bpb | params | hybrid | bpb | blocks won | hybrid vs NEXCOMP | CM enc / dec MB/s | lossless |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| dickens | 10192446 | 2344168 | 1.840 | 2504336 | 1.966 | s=1 r=67 | 2344168 | 1.840 | 0/3 | +0.00% | 1.36 / 1.38 | true |
| mozilla | 51220480 | 14301108 | 2.234 | 15393064 | 2.404 | s=1 r=4 | 14301108 | 2.234 | 0/13 | +0.00% | 2.57 / 2.58 | true |
| mr | 9970564 | 2983837 | 2.394 | 1991813 | 1.598 | s=2 r=1024 | 1991813 | 1.598 | 3/3 | -33.25% | 1.54 / 1.57 | true |
| nci | 33553445 | 1441619 | 0.344 | 2030986 | 0.484 | s=3 r=70 | 1441619 | 0.344 | 0/8 | +0.00% | 4.21 / 4.16 | true |
| ooffice | 6152192 | 2231861 | 2.902 | 2698641 | 3.509 | s=1 r=4 | 2231861 | 2.902 | 0/2 | +0.00% | 0.81 / 0.82 | true |
| osdb | 10085684 | 3034161 | 2.407 | 2680374 | 2.126 | s=2 r=8 | 2680374 | 2.126 | 3/3 | -11.66% | 1.16 / 1.16 | true |
| reymont | 6627202 | 1024838 | 1.237 | 1188993 | 1.435 | s=1 r=5 | 1024838 | 1.237 | 0/2 | +0.00% | 1.04 / 1.02 | true |
| samba | 21606400 | 4024917 | 1.490 | 4542628 | 1.682 | s=1 r=3 | 4024917 | 1.490 | 0/6 | +0.00% | 2.63 / 2.65 | true |
| sao | 7251944 | 4563484 | 5.034 | 4366357 | 4.817 | s=2 r=28 | 4366357 | 4.817 | 2/2 | -4.32% | 0.84 / 0.85 | true |
| webster | 41458703 | 7233545 | 1.396 | 7848206 | 1.514 | s=1 r=4 | 7233545 | 1.396 | 0/10 | +0.00% | 2.74 / 2.83 | true |
| x-ray | 8474240 | 4231653 | 3.995 | 3680868 | 3.475 | s=2 r=3800 | 3680868 | 3.475 | 3/3 | -13.02% | 1.01 / 1.03 | true |
| xml | 5345280 | 386659 | 0.579 | 503483 | 0.754 | s=1 r=14 | 386659 | 0.579 | 0/2 | +0.00% | 1.08 / 1.07 | true |
| **total** | 211938580 | 47801850 | 1.804 | 49429749 | 1.866 | | 45708127 | 1.725 | 11/57 | -4.38% | | |

#### enwik8

| file | orig | NEXCOMP | bpb | stride CM | bpb | params | hybrid | bpb | blocks won | hybrid vs NEXCOMP | CM enc / dec MB/s | lossless |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| enwik8 | 100000000 | 24485326 | 1.959 | 27312968 | 2.185 | s=1 r=3 | 24485326 | 1.959 | 0/24 | +0.00% | 2.78 / 2.79 | true |
| **total** | 100000000 | 24485326 | 1.959 | 27312968 | 2.185 | | 24485326 | 1.959 | 0/24 | +0.00% | | |

#### End to end through the CLI (integrated selector)

`nexcomp compress` / `decompress` on every Silesia file with codec 7 in the selector: total
45 708 127 bytes, identical to the hybrid column, every file restored byte for byte.

| file | bytes | codec chosen | compress (s) | decompress (s) |
|---|---|---|---|---|
| dickens | 2344168 | bwt | 6.44 | 0.88 |
| mozilla | 14301108 | lzma | 29.37 | 0.29 |
| mr | 1991813 | stride-cm | 7.72 | 6.16 |
| nci | 1441619 | bwt | 6.43 | 1.49 |
| ooffice | 2231861 | lzma | 10.50 | 0.11 |
| osdb | 2680374 | stride-cm | 10.67 | 8.76 |
| reymont | 1024838 | bwt | 4.41 | 0.74 |
| samba | 4024917 | lzma + bwt | 9.53 | 0.66 |
| sao | 4366357 | stride-cm | 9.66 | 8.54 |
| webster | 7233545 | bwt | 14.23 | 1.72 |
| x-ray | 3680868 | stride-cm | 9.24 | 8.25 |
| xml | 386659 | bwt | 2.02 | 0.51 |
| **total** | **45708127** | | **120.2** | **38.1** |

Before integration the same files took about 74 s to compress and 7 s to decompress (library
timings from the MinMask corpus run). Binary files that the CM does not win (mozilla, ooffice)
still pay its encode time, since the ASCII gate only skips text.

## Ablation

Totals over 14 sets (six numeric sets and the first MiB of eight corpus files); every row adds
one group of models to the previous one, except the last two, which remove some.

| variant | total | u32 counter, 256 KiB | u16 sine + noise, 256 KiB | i32 random walk, 256 KiB | u64 timestamps +1000±8, 256 KiB | f32 damped oscillation, 256 KiB | u8 image 512x512, smooth + noise | mr (first 1 MiB) | x-ray (first 1 MiB) | sao (first 1 MiB) | osdb (first 1 MiB) | geo (first 1 MiB) | pic (first 1 MiB) | kennedy.xls (first 1 MiB) | book1 (first 1 MiB) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| order-0..6 only | 2695222 | 51644 | 140884 | 25869 | 81156 | 209058 | 116063 | 213447 | 454987 | 675735 | 294216 | 52117 | 48836 | 113742 | 217468 |
| + column value context | 2505998 | 5843 | 107235 | 25255 | 25878 | 177102 | 115980 | 213142 | 449829 | 671848 | 291567 | 48692 | 48900 | 107095 | 217632 |
| + linear, delta, plane value contexts | 2284147 | 3313 | 67762 | 25646 | 24263 | 124958 | 114644 | 193469 | 444847 | 652243 | 291315 | 48494 | 32389 | 40870 | 219934 |
| + expected-bit models (all) | 2270654 | 1039 | 65624 | 25756 | 22116 | 122324 | 114700 | 192632 | 443648 | 652301 | 291103 | 47947 | 31947 | 39146 | 220371 |
| expected-bit models without plane | 2394428 | 1649 | 74211 | 25645 | 22607 | 128529 | 115733 | 206217 | 448116 | 671429 | 293049 | 47715 | 49243 | 90769 | 219516 |
| expected-bit linear only | 2419335 | 2448 | 77346 | 25682 | 26924 | 129229 | 115650 | 205964 | 451290 | 674497 | 294013 | 48502 | 48925 | 100889 | 217976 |
| NEXCOMP (reference) | 2680843 | 18023 | 85876 | 35197 | 62469 | 182575 | 153522 | 283881 | 504135 | 666594 | 328126 | 56690 | 44220 | 48130 | 211405 |

- **Order-0..6 alone** lands at NEXCOMP's level on this mix (2 695 222 vs 2 680 843 bytes).
- **Value contexts carry most of the gain**: the column context alone takes 7.0% off, and the
  linear, delta and planar contexts another 8.9%.
- **Expected-bit models** (the prediction's next bit as context, with prefix agreement and the
  predictor's recent error) add 0.6% overall, but they are what makes exact data nearly free:
  the u32 counter drops from 3 313 to 1 039 bytes and timestamps from 24 263 to 22 116.
- **The planar predictor** matters most on images and tables: without it the total grows 5.5%,
  kennedy.xls goes from 39 146 to 90 769 bytes and pic from 31 947 to 49 243.
- **Text** (book1) gets slightly worse with prediction contexts, which is why the selector keeps
  BWT/PPM there.

## Integration Decision

**INTEGRATED** (Tier A of the MinMask criteria: a reduction on standard corpora with zero
regressions after adaptive selection).

- Selection stays size-only: the codec is chosen only where it is smaller, so no block grows.
- The ASCII gate (≤ 85% printable) is data-driven: every winning block measured ≤ 0.79 (osdb),
  every text file ≥ 0.90, and the codec never won a text block.
- Costs: about 1 MB/s encode and decode on the blocks it wins; peak RSS of the CLI is 1.96 GB
  compressing mozilla (51 MB, 13 blocks in parallel) and 0.52 GB on mr; and slower compression
  of binaries it does not win.
- The wire format gains codec id 7 inside the existing NX13 container; older NX13 files still
  decode.

**Next steps with evidence behind them:** richer 2-D contexts (up-left and up-right neighbours,
two record lengths) for images and tables; a long-range match model so executables such as
mozilla and ooffice can benefit; and skipping the CM early on binary blocks where a quick sample
shows no gain, to recover compression speed.

## Reproduction

```
export NEXCOMP_CORPORA_DIR=/path/to/corpora   # calgary/ canterbury/ silesia/ enwik8/
cargo test --release --lib stride_cm
cargo test --release --test predictor_context_experiment -- --ignored --nocapture --test-threads=1 --exact numeric_synthetic
cargo test --release --test predictor_context_experiment -- --ignored --nocapture --test-threads=1 --exact corpora
cargo test --release --test predictor_context_experiment -- --ignored --nocapture --test-threads=1 --exact ablation
```
