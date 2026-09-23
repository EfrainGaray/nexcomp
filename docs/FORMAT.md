# NEXCOMP file formats

Every integer is little-endian. Offsets are in bytes.

## Compatibility policy

1. A release may stop **writing** a format, but never stops **reading** a format
   an earlier release wrote.
2. Every readable format has golden fixtures in `tests/formats/`, with the length
   and BLAKE3 of the original in `tests/formats/MANIFEST`. The test suite decodes
   all of them on every build and every architecture; regenerating them requires
   `NEXCOMP_REGENERATE_FIXTURES=1`, because a fixture whose bytes change is a
   compatibility break, not a test to update.
3. A format change gets a new magic. There is no version negotiation beyond it.
4. This policy starts with the formats below. The pre-release formats at the end
   of this file are exempt and are refused with a clear error.

| magic | what it is | written by | read by |
|---|---|---|---|
| `NX15` | container with a whole-file hash | 1.8.0 and later | 1.8.0 and later |
| `NX14` | the same layout, codec payloads of 1.6.0-1.7.0 | 1.6.0-1.7.0 | 1.6.0 and later |
| `NX13` | container without the hash | 1.5.x | 1.5.x and later |
| `NXE3` | encrypted wrapper, KDF costs in the header | 1.6.0 and later | 1.6.0 and later |
| `NXE2` | encrypted wrapper, fixed KDF costs | 1.5.x | 1.5.x and later |

`NX15` and `NX14` share a layout; what changed is inside the codec payloads, so
they are told apart by the magic rather than left to fail on a checksum. An
`NX14` file written by 1.6.0 and one written by 1.7.0 can differ in the
stride-cm model mask, which is why a 1.6.0 build rejects some `NX14` files: the
magic below is where that line is drawn properly.

## Container: NX15 (and NX14, NX13)

```
[0..4]   magic "NX15"        (NX13 is the same layout without the footer)
[4..12]  original length     u64
[12..16] block count         u32   == ceil(original length / 4 MiB)
then, for each block:
  [0]      codec id          u8
  [1]      BCJ filter flag   u8    (0 or 1)
  [2..6]   block length      u32   == min(4 MiB, remaining original bytes)
  [6..10]  payload length    u32
  [10..14] CRC-32 of block   u32   (IEEE 802.3, of the block's original bytes)
  [14..]   payload
footer (NX15 and NX14 only):
  [0..32]  BLAKE3 of the whole original
```

Blocks are independent: 4 MiB of input each, the last one shorter, every block
compressed with the codec that produced the smallest payload for it. The layout
is checked before decoding: the block count must match the declared length, and
each block's length must be the one the writer would have used, so a file cannot
declare an arbitrary block geometry.

| id | codec | notes |
|---|---|---|
| 0 | lz77huf | LZ77 + per-block Huffman, the baseline every block is measured against |
| 1 | lzma | range coder, price-based optimal parse, BT3 match finder |
| 2 | delta | delta filter + rANS, per lane |
| 3 | rlehuf | run-length + Huffman |
| 4 | store | raw bytes |
| 5 | bwt | Burrows-Wheeler (SA-IS) + binary context mixing |
| 6 | ppm | order-5 PPM with update exclusion |
| 7 | stride-cm | context mixing with stride, record and plane contexts |

The BCJ flag means the block was passed through the x86 call/jump filter before
the codec, and must be passed through its inverse after decoding.

Two codec payloads carry a field that says how they were modelled, and a
decoder that does not know a value must refuse the block rather than guess:

- **stride-cm** (id 7) stores the set of active models as a byte. 1.6.0 wrote
  masks within `0x1f`; 1.7.0 added the match model, `0x20`. A build that does
  not know a bit refuses the block.
- **delta** (id 2) has bit `0x80` of its delta type set when the rANS tables
  are derived with the pinned tie order. Payloads without it come from an
  earlier release and are decoded through the old derivation, which depends on
  the standard library's sort and is why the order was pinned.

### What the decoder guarantees

- Any byte string is either decoded or reported as an error: no panic, no
  silent wrong output. Checked by `tests/mutation_decode.rs` and the targets
  in `fuzz/`.
- Each block's decoded bytes are checked against the block length and CRC-32;
  the whole output is checked against the BLAKE3 footer in NX15 and NX14.
- Every declared length inside a codec payload is checked against the block
  length before the codec runs, and the output buffer grows with what decodes
  rather than with what the container declares, so neither a hostile header nor
  a corrupt payload can force an allocation the file does not back.
- What a block's own codec needs while decoding it is a different bound: PPM
  (id 6) keeps a context table per byte of its block and costs about 1.5 GB for
  a full 4 MiB block. Blocks are grouped so that their estimated working memory
  stays under 2 GiB, but a single block can still exceed it.
- `try_adaptive_decompress_limited` refuses a file that declares more output
  than the caller allows, before allocating for it.
- What the decoder does not bound is how far a *valid* file expands: a few
  kilobytes of run-length blocks restore to gigabytes. Memory stays bounded
  because the output is written block by block, but disk is not, so a caller
  taking untrusted files should pass a limit or watch the destination.

## Encrypted wrapper: NXE3 (and NXE2)

```
NXE3:
  [0..4]   magic "NXE3"
  [4]      KDF id            u8    (1 = Argon2id, version 0x13)
  [5..9]   memory cost KiB   u32
  [9..13]  time cost         u32
  [13..17] parallelism       u32
  [17..25] original length   u64
NXE2:
  [0..4]   magic "NXE2"
  [4..8]   original length   u32   (Argon2id with m = 19456 KiB, t = 2, p = 1)
then, for both:
  [32B salt][12B nonce][ChaCha20-Poly1305 ciphertext of the container][16B tag]
```

The header is the AEAD associated data, so the declared costs and length are
authenticated. Files are compressed first and encrypted afterwards. New files
use m = 64 MiB, t = 3, p = 1; costs above 1 GiB, 16 passes or 16 lanes are
refused before any key derivation, so a hostile header cannot demand unbounded
memory.

## Pre-release formats

`NXC\x01`, `NX12` and `NXE1` were written by development builds that were never
released or tagged. They carry no checksum, and their codecs (LZMA literal
coding, the BWT stage, the encryption wrapper) changed in ways a current decoder
cannot reproduce, so decoding them here could return wrong data silently.
They are refused with an error naming the format.

Files in those formats can still be recovered by building the last commit that
reads them and decompressing there:

| format | last commit that reads it |
|---|---|
| `NXC\x01` | `d181659` |
| `NX12` | `0076e42` |
| `NXE1` | `6f3c658` |
