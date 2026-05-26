# SPEC-01: nexcomp-gemma

## One-liner
Lossless compressor that uses Gemma 4 E2B as context model in arithmetic coding — first practical neural compressor for small infrastructure.

## Smoke test results (2025-04-29, RTX 4070Ti Super)
- Determinism: BIT-EXACT (max diff = 0.00e+00) ✓
- Speed GPU: 72.9 tok/sec
- BPB: Python code 0.574 bpb (+73% vs bzip2), English 0.959 bpb (+54%), Spanish 1.612 bpb (+23%), JSON 2.108 bpb (=bzip2)
- nexcomp baseline: 2.061 bpb Calgary (beats bzip2 2.096, beats zstd-19 2.233 on text)

## Architecture decisions (post-Opus review)

### MVP: Python + PyO3 (NO subprocess JSON)
- llama-cpp-python exposes float32 logits directly
- nexcomp range_coder.rs bound via maturin/PyO3
- Single process, zero-copy, zero IPC overhead
- Determinism guaranteed: no float32→u16 quantization in transport

### Student model (distillation)
- Tokenizer: Gemma 256k with factored embeddings (256k×128→128×512, ALBERT-style)
- Architecture: 12L, hidden=512, heads=8 — ~150M params total
- Size: ~75MB Q4
- Training: online distillation (KL on bitstream cross-entropy, not raw logits)
- Data: The Stack v2 + FineWeb-Edu, ~5B tokens (not 10B)
- Hardware: RTX 4070Ti 16GB, ~8-10h
- Target: bpb < 1.3 code, < 1.5 English (NOT 2.5 — that loses to bzip2)
- Speed: 300-400 tok/sec CPU

### File format .nxg4
```
Magic: "NXG4" (4 bytes)
Version: u8
Flags: u8 (bit0=neural, bit1=hybrid, bit2=student)
Original size: u64
GGUF SHA256 (first 8 bytes): u64
Block size: u32
Resync interval: u32 (blocks between resync points — REQUIRED for corruption resilience)
Reserved: 32 bytes
[blocks...]
```

### Competitive baseline: ts_zip (Fabrice Bellard, RWKV-169M → 0.87 bpb text8)
- Must benchmark against ts_zip before any public claim
- If we beat ts_zip on code corpus → that's the headline

### Hackathon: gemma-4-good-hackathon ($200K, May 2026, 153 teams)
- Pitch: "Gemma compresses code 73% better than bzip2. We made it run on a Raspberry Pi."
- Demo: side-by-side live (linux kernel tarball, zstd vs nexcomp vs Gemma-direct)
- Use case with dollar number: OMS medical manual 800MB → 180MB, 12h→5h on 2G
- Gemma multiplier story: Gemma ≠ chatbot, Gemma = universal compression primitive

## Risks (Tier 1 — existential)
1. Cross-platform determinism: tested only on RTX 4070Ti. Must verify CPU AVX2, ARM, llama.cpp version changes.
2. Decompression speed is the KPI: 300 tok/sec = 5KB/sec → 100MB = 5h. Need parallel decode or async AC.
3. Gemma TOS: verify embedded use licensing before week 4.
4. No resync points → single block corruption = entire file unreadable.

## Roadmap (4 weeks)
- W1: bit-exact roundtrip Python+PyO3, public benchmark vs bzip2/zstd/ts_zip
- W2: Gemma GPU optimization (KV cache reuse, speculative AC → 200+ tok/sec)
- W3: student model, online distillation, bpb < 1.3 code gate
- W4: standalone Rust binary, web demo, hackathon pitch

## Phase: xp → solid after W1 gate passes
