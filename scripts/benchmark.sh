#!/usr/bin/env bash
# Benchmark NEXCOMP against the reference compressors on the standard corpora
# and write a result artifact that records how it was measured.
#
#   scripts/benchmark.sh [corpus ...]      default: calgary canterbury silesia enwik8
#
# Environment:
#   NEXCOMP_CORPORA_DIR   where the corpora live (default ~/corpora)
#   NEXCOMP_TOOLS         which tools to run (default: nexcomp xz bzip2, plus
#                         zstd and brotli when installed)
#   NEXCOMP_BIN           the binary under test (default target/release/nexcomp)
#
# Every file is compressed and decompressed on its own; the result is only
# recorded when the restored bytes hash to the original. Sizes are whole files,
# framing included.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORPORA_DIR="${NEXCOMP_CORPORA_DIR:-$HOME/corpora}"
NEXCOMP_BIN="${NEXCOMP_BIN:-$ROOT/target/release/nexcomp}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

CORPORA=("$@")
[ ${#CORPORA[@]} -eq 0 ] && CORPORA=(calgary canterbury silesia enwik8)

TOOLS="${NEXCOMP_TOOLS:-}"
if [ -z "$TOOLS" ]; then
    TOOLS="nexcomp xz bzip2"
    command -v zstd >/dev/null && TOOLS="$TOOLS zstd"
    command -v brotli >/dev/null && TOOLS="$TOOLS brotli"
fi

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }

# Run a command, setting SECS and RSS (bytes). stdout goes to $1.
measure() {
    local out="$1"; shift
    local log start end
    log="$(mktemp)"
    start="$(now)"
    /usr/bin/time -l "$@" >"$out" 2>"$log" || /usr/bin/time -v "$@" >"$out" 2>"$log"
    end="$(now)"
    SECS="$(perl -e 'printf "%.3f\n", $ARGV[1] - $ARGV[0]' "$start" "$end")"
    # macOS: "<bytes> maximum resident set size"; GNU: "Maximum resident set size (kbytes): <n>"
    RSS="$(awk '/maximum resident set size/ {print $1; found=1}
                /Maximum resident set size/ {print $NF * 1024; found=1}
                END {if (!found) print 0}' "$log" | head -1)"
    rm -f "$log"
}

compress_cmd() {
    case "$1" in
        nexcomp) echo "$NEXCOMP_BIN compress %in% %out%" ;;
        xz) echo "xz -9e -T1 -c %in%" ;;
        bzip2) echo "bzip2 -9 -c %in%" ;;
        zstd) echo "zstd --ultra -22 --long=27 -T1 -q -c %in%" ;;
        brotli) echo "brotli -q 11 -c %in%" ;;
    esac
}

decompress_cmd() {
    case "$1" in
        nexcomp) echo "$NEXCOMP_BIN decompress %out% %restored%" ;;
        xz) echo "xz -dc %out%" ;;
        bzip2) echo "bzip2 -dc %out%" ;;
        zstd) echo "zstd -dc --long=27 %out%" ;;
        brotli) echo "brotli -dc %out%" ;;
    esac
}

tool_version() {
    case "$1" in
        nexcomp) "$NEXCOMP_BIN" --version ;;
        xz) xz --version | head -1 ;;
        bzip2) bzip2 --help 2>&1 | head -1 | sed 's/^ *//' ;;
        zstd) zstd --version ;;
        brotli) brotli --version ;;
    esac
}

files_of() {
    awk -F'\t' -v c="$1" '$1 == c {print $2}' "$ROOT/bench/manifest.tsv"
}

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
mkdir -p "$ROOT/bench/results"
tsv="$ROOT/bench/results/$stamp-$os-$arch.tsv"
md="$ROOT/bench/results/$stamp-$os-$arch.md"

if [ "$os" = darwin ]; then
    cpu="$(sysctl -n machdep.cpu.brand_string)"
    cores="$(sysctl -n hw.ncpu)"
    memory="$(( $(sysctl -n hw.memsize) / 1024 / 1024 )) MiB"
else
    cpu="$(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)"
    cores="$(nproc)"
    memory="$(awk '/MemTotal/ {print int($2 / 1024) " MiB"}' /proc/meminfo)"
fi

{
    echo "# NEXCOMP benchmark $stamp"
    echo
    echo '```'
    echo "commit        $(git -C "$ROOT" rev-parse HEAD)$(git -C "$ROOT" diff --quiet || echo ' (dirty working tree)')"
    echo "rustc         $(rustc -Vv | tr '\n' ' ' | sed 's/  */ /g')"
    echo "host          $os $arch, $cpu, $cores cores, $memory"
    echo "kernel        $(uname -srv)"
    echo "threads       ${RAYON_NUM_THREADS:-$cores} (RAYON_NUM_THREADS)"
    echo "corpora       $CORPORA_DIR, verified against bench/manifest.tsv"
    for tool in $TOOLS; do printf '%-13s %s\n' "$tool" "$(tool_version "$tool")"; done
    echo '```'
    echo
} > "$md"

printf 'corpus\tfile\ttool\toriginal\tcompressed\tbpb\tcompress_s\tcompress_rss\tdecompress_s\tdecompress_rss\n' > "$tsv"

echo "verifying corpora..."
bash "$ROOT/scripts/corpora.sh" verify

for corpus in "${CORPORA[@]}"; do
    echo "== $corpus"
    for tool in $TOOLS; do
        [ -n "$(compress_cmd "$tool")" ] || continue
        total_in=0; total_out=0; total_cs=0; total_ds=0; max_rss=0
        for file in $(files_of "$corpus"); do
            input="$CORPORA_DIR/$corpus/$file"
            packed="$WORK/$file.$tool"
            restored="$WORK/$file.restored"
            rm -f "$packed" "$restored"

            cmd="$(compress_cmd "$tool")"
            cmd="${cmd//%in%/$input}"; cmd="${cmd//%out%/$packed}"
            # shellcheck disable=SC2086
            if [ "$tool" = nexcomp ]; then measure /dev/null $cmd; else measure "$packed" $cmd; fi
            cs="$SECS"; c_rss="$RSS"

            cmd="$(decompress_cmd "$tool")"
            cmd="${cmd//%out%/$packed}"; cmd="${cmd//%restored%/$restored}"
            # shellcheck disable=SC2086
            if [ "$tool" = nexcomp ]; then measure /dev/null $cmd; else measure "$restored" $cmd; fi
            ds="$SECS"; d_rss="$RSS"

            if [ "$(sha256 "$input")" != "$(sha256 "$restored")" ]; then
                echo "  $tool $file: RESTORED BYTES DIFFER" >&2
                exit 1
            fi

            in_size="$(wc -c < "$input" | tr -d ' ')"
            out_size="$(wc -c < "$packed" | tr -d ' ')"
            bpb="$(perl -e 'printf "%.4f\n", $ARGV[0] * 8 / $ARGV[1]' "$out_size" "$in_size")"
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
                "$corpus" "$file" "$tool" "$in_size" "$out_size" "$bpb" "$cs" "$c_rss" "$ds" "$d_rss" >> "$tsv"
            total_in=$((total_in + in_size)); total_out=$((total_out + out_size))
            total_cs="$(perl -e 'printf "%.3f\n", $ARGV[0] + $ARGV[1]' "$total_cs" "$cs")"
            total_ds="$(perl -e 'printf "%.3f\n", $ARGV[0] + $ARGV[1]' "$total_ds" "$ds")"
            [ "$c_rss" -gt "$max_rss" ] && max_rss="$c_rss"
            rm -f "$packed" "$restored"
        done
        bpb="$(perl -e 'printf "%.4f\n", $ARGV[0] * 8 / $ARGV[1]' "$total_out" "$total_in")"
        printf '%s\ttotal\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$corpus" "$tool" "$total_in" "$total_out" "$bpb" "$total_cs" "$max_rss" "$total_ds" 0 >> "$tsv"
        printf '  %-8s %12s bytes  %6s bpb  %8ss compress  %8ss decompress  %s MiB peak\n' \
            "$tool" "$total_out" "$bpb" "$total_cs" "$total_ds" "$((max_rss / 1048576))"
    done
done

{
    for corpus in "${CORPORA[@]}"; do
        echo "## $corpus"
        echo
        echo "| tool | compressed | bpb | compress s | decompress s | peak RSS MiB |"
        echo "|---|---|---|---|---|---|"
        awk -F'\t' -v c="$corpus" '$1 == c && $2 == "total" {
            printf "| %s | %s | %s | %s | %s | %d |\n", $3, $5, $6, $7, $9, $8 / 1048576
        }' "$tsv"
        echo
    done
    echo "Per-file numbers: \`$(basename "$tsv")\`."
} >> "$md"

echo
echo "wrote $md"
echo "      $tsv"
