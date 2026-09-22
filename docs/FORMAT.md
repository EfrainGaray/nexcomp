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
| `NX14` | container with a whole-file hash | 1.6.0 and later | 1.6.0 and later |
| `NX13` | container without the hash | 1.5.x | 1.5.x and later |
| `NXE3` | encrypted wrapper, KDF costs in the header | 1.6.0 and later | 1.6.0 and later |
| `NXE2` | encrypted wrapper, fixed KDF costs | 1.5.x | 1.5.x and later |

## Container: NX14 (and NX13)

```
[0..4]   magic "NX14"        (NX13 is the same layout without the footer)
[4..12]  original length     u64
[12..16] block count         u32   == ceil(original length / 4 MiB)
then, for each block:
  [0]      codec id          u8
  [1]      BCJ filter flag   u8    (0 or 1)
  [2..6]   block length      u32   == min(4 MiB, remaining original bytes)
  [6..10]  payload length    u32
  [10..14] CRC-32 of block   u32   (IEEE 802.3, of the block's original bytes)
  [14..]   payload
footer (NX14 only):
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

### What the decoder guarantees

- Any byte string is either decoded or reported as an error: no panic, no
  unbounded allocation, no silent wrong output. Checked by
  `tests/mutation_decode.rs` and the targets in `fuzz/`.
- Each block's decoded bytes are checked against the block length and CRC-32;
  the whole output is checked against the BLAKE3 footer in NX14.
- Every declared length inside a codec payload is checked against the block
  length before the codec runs, so a corrupt file cannot make a decoder
  allocate or loop beyond what its block could hold.
- `try_adaptive_decompress_limited` refuses a file that declares more output
  than the caller allows, before allocating for it.

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
