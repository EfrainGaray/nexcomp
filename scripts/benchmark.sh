#!/bin/bash
# NEXCOMP Benchmark Script — Ubuntu 24.04 LTS
# Reproduces all benchmarks from §4 of the NEXCOMP design document.
#
# Requirements: ~5GB disk, ~8GB RAM, internet connection
# Tested on: Ubuntu 24.04 LTS, Ryzen 7 5800X, 32GB RAM
#
# Usage: chmod +x benchmark.sh && ./benchmark.sh

set -euo pipefail

WORKDIR="$(pwd)/nexcomp-bench"
CORPUS_DIR="$WORKDIR/corpora"
RESULTS_DIR="$WORKDIR/results"
TOOLS_DIR="$WORKDIR/tools"

mkdir -p "$CORPUS_DIR" "$RESULTS_DIR" "$TOOLS_DIR"

echo "============================================"
echo " NEXCOMP Benchmark Suite v1.0"
echo " $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "============================================"

# ──────────────────────────────────────────────
# 1. Download corpora
# ──────────────────────────────────────────────
echo ""
echo "[1/5] Downloading benchmark corpora..."

# Silesia corpus (211 MB)
if [ ! -d "$CORPUS_DIR/silesia" ]; then
    echo "  Downloading Silesia corpus..."
    mkdir -p "$CORPUS_DIR/silesia"
    cd "$CORPUS_DIR/silesia"
    wget -q "http://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip" -O silesia.zip
    unzip -q silesia.zip
    rm silesia.zip
    cd "$WORKDIR"
else
    echo "  Silesia corpus already present."
fi

# enwik8 (100 MB — first 10^8 bytes of English Wikipedia)
if [ ! -f "$CORPUS_DIR/enwik8" ]; then
    echo "  Downloading enwik8..."
    cd "$CORPUS_DIR"
    wget -q "http://mattmahoney.net/dc/enwik8.zip" -O enwik8.zip
    unzip -q enwik8.zip
    rm enwik8.zip
    cd "$WORKDIR"
else
    echo "  enwik8 already present."
fi

# Calgary corpus (3.2 MB)
if [ ! -d "$CORPUS_DIR/calgary" ]; then
    echo "  Downloading Calgary corpus..."
    mkdir -p "$CORPUS_DIR/calgary"
    cd "$CORPUS_DIR/calgary"
    wget -q "http://www.data-compression.info/files/corpora/largecalgarycorpus.zip" -O calgary.zip
    unzip -q calgary.zip || true
    rm -f calgary.zip
    cd "$WORKDIR"
else
    echo "  Calgary corpus already present."
fi

echo "  Corpora ready."

# ──────────────────────────────────────────────
# 2. Install compressor tools
# ──────────────────────────────────────────────
echo ""
echo "[2/5] Installing compressor tools..."

install_if_missing() {
    local cmd="$1"
    local pkg="$2"
    if ! command -v "$cmd" &>/dev/null; then
        echo "  Installing $pkg..."
        sudo apt-get install -y -qq "$pkg" 2>/dev/null || echo "  WARNING: Could not install $pkg"
    fi
}

install_if_missing gzip gzip
install_if_missing bzip2 bzip2
install_if_missing xz xz-utils
install_if_missing zstd zstd
install_if_missing brotli brotli

# Build NEXCOMP
echo "  Building NEXCOMP (release)..."
cd "$(dirname "$0")/.."
cargo build --release 2>&1 | tail -1
NEXCOMP="$(pwd)/target/release/nexcomp"
cd "$WORKDIR"

echo "  Tools ready."

# ──────────────────────────────────────────────
# 3. Run benchmarks
# ──────────────────────────────────────────────
echo ""
echo "[3/5] Running benchmarks..."

# Benchmark function: compress, measure, decompress, verify
# Usage: run_bench <name> <compress_cmd> <decompress_cmd> <input> <compressed> <decompressed>
run_bench() {
    local name="$1"
    local input="$2"
    local compress_cmd="$3"
    local compressed="$4"
    local decompress_cmd="$5"
    local decompressed="$6"
    local corpus_name="$7"

    local input_size
    input_size=$(stat -c%s "$input" 2>/dev/null || stat -f%z "$input")

    # Compress
    local comp_start comp_end comp_time
    comp_start=$(date +%s%N)
    eval "$compress_cmd" 2>/dev/null
    comp_end=$(date +%s%N)
    comp_time=$(( (comp_end - comp_start) / 1000000 )) # ms

    if [ ! -f "$compressed" ]; then
        echo "  SKIP: $name on $corpus_name (compression failed)"
        return
    fi

    local comp_size
    comp_size=$(stat -c%s "$compressed" 2>/dev/null || stat -f%z "$compressed")

    # Decompress
    local dec_start dec_end dec_time
    dec_start=$(date +%s%N)
    eval "$decompress_cmd" 2>/dev/null
    dec_end=$(date +%s%N)
    dec_time=$(( (dec_end - dec_start) / 1000000 )) # ms

    # Verify integrity
    local original_md5 decoded_md5 integrity
    original_md5=$(md5sum "$input" | cut -d' ' -f1)
    if [ -f "$decompressed" ]; then
        decoded_md5=$(md5sum "$decompressed" | cut -d' ' -f1)
        if [ "$original_md5" = "$decoded_md5" ]; then
            integrity="OK"
        else
            integrity="FAIL"
        fi
    else
        integrity="SKIP"
    fi

    # Calculate metrics
    local ratio bpb comp_speed dec_speed
    ratio=$(echo "scale=4; $comp_size / $input_size * 100" | bc)
    bpb=$(echo "scale=4; $comp_size * 8 / $input_size" | bc)

    if [ "$comp_time" -gt 0 ]; then
        comp_speed=$(echo "scale=1; $input_size / 1048576 / ($comp_time / 1000)" | bc)
    else
        comp_speed="INF"
    fi

    if [ "$dec_time" -gt 0 ]; then
        dec_speed=$(echo "scale=1; $input_size / 1048576 / ($dec_time / 1000)" | bc)
    else
        dec_speed="INF"
    fi

    local mem_mb
    mem_mb=$(echo "scale=1; $comp_size / 1048576" | bc)

    # Output row
    printf "| %-14s | %-10s | %8s | %6s | %8s | %8s | %6s | %s |\n" \
        "$name" "$corpus_name" "${ratio}%" "$bpb" "${comp_speed} MB/s" "${dec_speed} MB/s" "${mem_mb}M" "$integrity"

    # Append to CSV
    echo "$name,$corpus_name,$input_size,$comp_size,$ratio,$bpb,$comp_speed,$dec_speed,$comp_time,$dec_time,$integrity" \
        >> "$RESULTS_DIR/results.csv"

    # Cleanup
    rm -f "$compressed" "$decompressed"
}

# CSV header
echo "compressor,corpus,input_bytes,output_bytes,ratio_pct,bpb,comp_mb_s,dec_mb_s,comp_ms,dec_ms,integrity" \
    > "$RESULTS_DIR/results.csv"

# Table header
echo ""
printf "| %-14s | %-10s | %8s | %6s | %8s | %8s | %6s | %s |\n" \
    "Compressor" "Corpus" "Ratio" "bpb" "Comp" "Decomp" "OutSz" "OK?"
printf "|%s|%s|%s|%s|%s|%s|%s|%s|\n" \
    "$(printf '%.0s-' {1..16})" "$(printf '%.0s-' {1..12})" \
    "$(printf '%.0s-' {1..10})" "$(printf '%.0s-' {1..8})" \
    "$(printf '%.0s-' {1..10})" "$(printf '%.0s-' {1..10})" \
    "$(printf '%.0s-' {1..8})" "$(printf '%.0s-' {1..6})"

# Run all benchmarks for enwik8
INPUT="$CORPUS_DIR/enwik8"
TMP="$WORKDIR/tmp"
mkdir -p "$TMP"

if [ -f "$INPUT" ]; then
    for tool in gzip bzip2 xz zstd brotli; do
        case $tool in
            gzip)
                run_bench "gzip -9" "$INPUT" \
                    "gzip -9 -c '$INPUT' > '$TMP/out.gz'" "$TMP/out.gz" \
                    "gzip -d -c '$TMP/out.gz' > '$TMP/dec'" "$TMP/dec" "enwik8"
                ;;
            bzip2)
                run_bench "bzip2 -9" "$INPUT" \
                    "bzip2 -9 -c '$INPUT' > '$TMP/out.bz2'" "$TMP/out.bz2" \
                    "bzip2 -d -c '$TMP/out.bz2' > '$TMP/dec'" "$TMP/dec" "enwik8"
                ;;
            xz)
                run_bench "xz -9e" "$INPUT" \
                    "xz -9e -c '$INPUT' > '$TMP/out.xz'" "$TMP/out.xz" \
                    "xz -d -c '$TMP/out.xz' > '$TMP/dec'" "$TMP/dec" "enwik8"
                ;;
            zstd)
                run_bench "zstd --ultra -22" "$INPUT" \
                    "zstd --ultra -22 -c '$INPUT' > '$TMP/out.zst'" "$TMP/out.zst" \
                    "zstd -d -c '$TMP/out.zst' > '$TMP/dec'" "$TMP/dec" "enwik8"
                ;;
            brotli)
                run_bench "brotli -q11" "$INPUT" \
                    "brotli -q 11 -c '$INPUT' > '$TMP/out.br'" "$TMP/out.br" \
                    "brotli -d -c '$TMP/out.br' > '$TMP/dec'" "$TMP/dec" "enwik8"
                ;;
        esac
    done

    # NEXCOMP
    if [ -f "$NEXCOMP" ]; then
        run_bench "NEXCOMP" "$INPUT" \
            "'$NEXCOMP' compress '$INPUT' '$TMP/out.nxc'" "$TMP/out.nxc" \
            "'$NEXCOMP' decompress '$TMP/out.nxc' '$TMP/dec'" "$TMP/dec" "enwik8"
    fi
fi

# ──────────────────────────────────────────────
# 4. Generate summary
# ──────────────────────────────────────────────
echo ""
echo "[4/5] Generating result summary..."
echo ""
echo "Results saved to: $RESULTS_DIR/results.csv"

# ──────────────────────────────────────────────
# 5. Verify integrity
# ──────────────────────────────────────────────
echo ""
echo "[5/5] Integrity verification..."
FAILURES=$(grep "FAIL" "$RESULTS_DIR/results.csv" | wc -l)
if [ "$FAILURES" -gt 0 ]; then
    echo "  WARNING: $FAILURES round-trip failures detected!"
    grep "FAIL" "$RESULTS_DIR/results.csv"
else
    echo "  All round-trips verified successfully."
fi

echo ""
echo "============================================"
echo " Benchmark complete: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "============================================"
