# Response to the external audit

Audited commit: `6d55700`. Every change below is on top of it; each finding
names the commits that address it and how it is checked from now on.

Summary: F-01 to F-06 and F-09 to F-12 are fixed. F-07 is fixed by measuring the
standard Calgary 14 and renaming what we published. The one recommendation we
did not follow is reading the pre-release formats (`NXC\x01`, `NX12`, `NXE1`),
which we refuse instead; the reasoning is under F-06.

## P0 — decoder safety

### F-01 — allocation bomb in NX13, baseline codec included — fixed

`d181659` checks every length a codec payload declares against the block length
from the container *before* the codec runs (`check_declared_lengths`), caps each
block at 4 MiB through a strict layout check, and bounds the LZ77 decoder by the
expected output. `80b30ce` fixes the three decoder panics a new mutation harness
found: PPM dividing by zero after escaping past all 256 symbols, an uncovered
Huffman code, and the bit reader underflowing past the end of the data.

Checked by `tests/hostile_decode.rs` (a tracking allocator asserts no hostile
1-byte block causes an allocation above 64 MiB), by `tests/mutation_decode.rs`
(300 mutations per codec by default, 20,000 per codec on demand, with the same
allocation bound) and by the `fuzz/` targets.

### F-02 — the legacy decoder had the same problem — fixed by removal

`c54451d` refuses `NXC\x01`, `NX12` and `NXE1` with a clear error instead of
decoding them, so the unbounded legacy path is gone. See F-06 for why we refuse
them rather than bound them.

### F-03 — panic on a truncated NXE2 wrapper — fixed

`a29dec5` checks the wrapper header length before splitting it; `6c806b5` moved
the wrapper into `src/crypto` where every prefix of a file is tested.
`tests/cli_hostile.rs` runs the CLI on a 4-byte `NXE2` file.

### F-04 — password exposed through argv — fixed

`c3970bd`: the password comes from `--password-file`, `NEXCOMP_PASSWORD` or a
no-echo prompt (asked twice when encrypting, and before compressing so the
prompt does not wait for the codecs). Without a terminal and without a source
the CLI fails instead of blocking. The argv form still works and prints a
warning, so existing scripts keep running while they migrate.

### F-05 — NXE2 depended on implicit Argon2 parameters — fixed

`6c806b5`: the NXE2 costs are pinned in code (19456 KiB, t=2, p=1) and checked
by a known-answer test, so upgrading the `argon2` crate can no longer change the
key of an existing file. New files use the NXE3 wrapper, which stores the KDF id
and the m/t/p costs in its authenticated header and uses 64 MiB, 3 passes.
Costs above 1 GiB, 16 passes or 16 lanes are refused before any derivation, so a
hostile header cannot demand unbounded memory. Salt and nonce come from `OsRng`
(now the fallible API of rand 0.9, `14d395c`), and derived keys are zeroized.

## Before calling the format stable

### F-06 — no compatibility policy — fixed, with one deliberate exception

`docs/FORMAT.md` specifies every format and states the policy: a release may
stop writing a format but never stops reading one an earlier release wrote.
`7d8f67f` freezes golden fixtures for NX13, NXE2 and NXE3 and `99d4ee9` adds
them for NX14: 36 files under `tests/formats`, one per codec (hand-framed, so
every decoder is pinned and not only the codecs the selector picks), plus a BCJ
block, a two-block file, and the encrypted wrappers. `tests/formats/MANIFEST`
records the length and BLAKE3 of each original; the suite decodes all of them on
every build and architecture, and regenerating them needs
`NEXCOMP_REGENERATE_FIXTURES=1`.

**Where we disagree:** the audit asks for fixtures for `NX12` and `NXE1` too, so
that they keep decoding. We refuse those files instead. They were written by
development builds that were never released or tagged, they carry no checksum,
and their codecs (the LZMA literal coder, the BWT stage, the wrapper) changed in
ways the current decoders cannot reproduce; decoding them with today's code
could return wrong bytes silently, which is worse than refusing. `docs/FORMAT.md`
names the last commit that reads each pre-release format, so those files can
still be recovered by building that commit. The policy starts here.

### Whole-file hash — done (audit item 10)

`99d4ee9` adds NX14: NX13 plus a 32-byte BLAKE3 of the original in a footer,
checked after decoding. 32 bytes per file, whatever its size. NX13 files keep
decoding.

## Benchmarks

### F-07 — "Calgary 18 files" was not the standard corpus — fixed

`bench/manifest.tsv` pins the standard 14 files (paper3 to paper6 are gone) with
their sizes and SHA-256, and `scripts/corpora.sh` verifies every file against it
before a benchmark runs; the extended set is no longer reported.

Re-measured, the standard Calgary 14 is **744,025 bytes**
(1.8946 bpb) against xz -9e at 843,828 and
bzip2 -9 at 828,347. The audit derived 743,577 for this corpus from our old
per-file table; the 448-byte difference is exactly the 32-byte whole-file hash NX14 now adds to
each of the 14 files. The full table, with the environment it was measured in, is in
`bench/results/20260922T190350Z-darwin-x86_64.md`.

Silesia and enwik8 are a separate run of the same script (`bench/results/20260922T190707Z-darwin-x86_64.md`),
because the machine was saturated by an unrelated build while this was written and one Silesia file
alone took over half an hour there. Their numbers: Silesia 45,630,063 bytes (1.7224 bpb) against
xz -9e at 48,456,004 and bzip2 -9 at 54,506,769; enwik8 24,485,454 (1.9588 bpb) against 24,831,648
and 29,008,758. Both are the previous measurements plus 32 bytes per file, which is the whole-file
hash NX14 adds. Sizes do not depend on the load; the times and peak RSS in an artifact are only as
good as the `load` line it records.

### F-08 — the benchmark harness was not reproducible — fixed

`scripts/benchmark.sh` was rewritten. It verifies the corpus against the
manifest, then for every file and every tool records the compressed size, wall
time and peak RSS (from `/usr/bin/time`), decompresses, and only records the
result when the restored bytes hash to the original. Each run writes
`bench/results/<UTC timestamp>-<os>-<arch>.tsv` with the per-file numbers and a
`.md` with the environment: commit (and whether the tree was dirty), full
`rustc -Vv`, CPU, cores, memory, kernel, thread count, and the version of every
tool it ran. The old script's "memory" column, which was the compressed size,
is gone, and so is its second corpus definition.

### F-11 — the selector was never measured against an oracle — done

`tests/selector_oracle.rs` compresses every block with all eight codecs, with
and without BCJ and with no gates at all, and compares the smallest of those
sixteen with what the selector produced.

**Result: the gates cost nothing measurable.** On 33 blocks of
`canterbury-large` (E. coli, the bible, world192 — never used to tune a
threshold), Calgary and Canterbury, the selector and the ungated oracle both
produce 3,438,677 bytes; not one block would have been smaller with a codec the
gates denied. The per-file table and what the experiment does *not* cover (the
Silesia binaries, where an ungated run costs minutes per block) are in
`docs/research/selector-oracle.md`.

## Engineering

### F-09 — no CI, fuzzing or cross-architecture validation — fixed

The first green run is 35774376267. `.github/workflows/ci.yml` runs on x86_64 and aarch64: build, clippy, the full
suite (which decodes the committed fixtures and checks the writer still produces
them), a longer mutation run, and a CLI round trip including encryption. Each
architecture then decodes the files the other one wrote. Two more jobs run the
four fuzz targets for two minutes each and `cargo audit --deny warnings`.

`fuzz/` has cargo-fuzz targets for the container, each block decoder, every
codec's own round trip and compress/decompress of arbitrary input. Locally each
target ran for 30 minutes, 212,926 executions in total, with no crash, after
fixing the one crash the first five-minute run had found.

`14d395c` clears what `cargo audit` reported: RUSTSEC-2026-0204 in
crossbeam-epoch (through rayon) and the unsoundness warning for rand 0.8.

### F-10 — parallelism multiplied memory — fixed on both sides

Compression keeps only the best candidate so far instead of holding every
candidate until the end (`reduce_with`), which frees each loser as soon as it
loses; the chosen bytes are identical, so no ratio changes.

Decoding no longer assembles the whole output: `adaptive::decompress_to` decodes
a bounded group of blocks at a time and writes each one out, hashing as it goes,
and the CLI decompresses straight to the file. A file that expands to 16 MiB
from under 8 KB now streams through in blocks instead of being assembled first,
and a decompression bomb cannot force an allocation the process aborts on.
`RAYON_NUM_THREADS` bounds how many blocks are in flight, on both sides.

### F-12 — public documentation was out of date — fixed

`fe6fa1a`: the CLI version comes from `CARGO_PKG_VERSION` (it said 1.2.0 while
the crate said 1.5.0) and `inspect` prints the real container format instead of
`version=1.3`. The README results now come from a benchmark artifact, and name
the artifact they were generated from.

## Audit gate criteria

| criterion | status |
|---|---|
| A. malformed NX13 never allocates above a configured maximum | met: `tests/hostile_decode.rs`, `tests/mutation_decode.rs`, fuzz targets |
| B. malformed legacy NXC never allocates above a configured maximum | met by refusing the legacy formats (F-02) |
| C. 24 h of fuzzing with zero panic, abort or hang | partly: 30 minutes per target with no crash; the 24 h run is not done |
| D. files written on x86 decode on ARM and the other way round | met: CI run 35774376267 decoded each architecture's files on the other, and the fixtures the writer produced on aarch64 were byte-identical to the committed x86_64 ones |
| E. standard benchmark manifests reproduce byte for byte | met: `bench/manifest.tsv` and the result artifacts |

## Not done, on purpose

- **Reorganising the tree** into `core/ codecs/ container/ crypto/ legacy/
  research/` with features for the research code. It is code health, not a data
  risk, and it would move every file in the middle of a security fix series.
- **The ratio roadmap** (general context mixer with a match model, 16 to 64 MiB
  model segments, binary models). That is the next block of work, not part of
  answering the audit.
