#!/bin/bash
# NEXCOMP Benchmark Suite v1.2
#
# Goals:
# - smoke test the current binary
# - measure corpus.tar, Calgary TAR and Canterbury TAR when present
# - skip explicitly when corpora are missing
# - optionally enforce v1.2 floor checks with NEXCOMP_ENFORCE_V12_FLOORS=1

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/nexcomp"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/nexcomp-v12.XXXXXX")"
ENFORCE_FLOORS="${NEXCOMP_ENFORCE_V12_FLOORS:-1}"

cleanup() {
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

have_cmd() {
    command -v "$1" >/dev/null 2>&1
}

filesize() {
    wc -c < "$1" | tr -d '[:space:]'
}

bpb_of() {
    awk -v comp="$1" -v orig="$2" 'BEGIN { printf "%.3f", (comp * 8.0) / orig }'
}

floor_check() {
    local label="$1"
    local measured="$2"
    local floor="$3"
    if [ "$ENFORCE_FLOORS" = "1" ]; then
        awk -v m="$measured" -v f="$floor" -v label="$label" '
            BEGIN {
                if (m > f + 0.001) {
                    printf "%s regressed: %.3f bpb > floor %.3f bpb\n", label, m, f > "/dev/stderr";
                    exit 1;
                }
            }'
    else
        printf "%s measured %.3f bpb vs floor %.3f bpb (floor enforcement disabled)\n" \
            "$label" "$measured" "$floor"
    fi
}

run_roundtrip() {
    local label="$1"
    local input="$2"
    local floor="$3"
    local compressed="$WORKDIR/$(basename "$input").nxc"
    local recovered="$WORKDIR/$(basename "$input").dec"

    if [ ! -f "$input" ]; then
        printf "%s missing at %s, skipping\n" "$label" "$input"
        return 0
    fi

    "$BIN" compress "$input" "$compressed" >/dev/null
    "$BIN" decompress "$compressed" "$recovered" >/dev/null
    cmp -s "$input" "$recovered"

    local orig size bpb codec
    orig="$(filesize "$input")"
    size="$(filesize "$compressed")"
    bpb="$(bpb_of "$size" "$orig")"

    if "$BIN" inspect "$compressed" --show-codec >/dev/null 2>&1; then
        codec="$("$BIN" inspect "$compressed" --show-codec 2>/dev/null | tail -n 1)"
    else
        codec="n/a"
    fi

    printf "%-18s %10s %10s %8s %10s\n" "$label" "$orig" "$size" "$bpb" "$codec"
    floor_check "$label" "$bpb" "$floor"
}

create_tar_from_dir() {
    local dir="$1"
    local archive="$2"
    if ! have_cmd tar; then
        printf "tar command not found, skipping archive for %s\n" "$dir"
        return 1
    fi
    tar -cf "$archive" -C "$dir" .
}

run_tar_dir() {
    local label="$1"
    local dir="$2"
    local floor="$3"
    local archive="$WORKDIR/${label}.tar"

    if [ ! -d "$dir" ]; then
        printf "%s directory not found at %s, skipping\n" "$label" "$dir"
        return 0
    fi

    if ! create_tar_from_dir "$dir" "$archive"; then
        printf "%s tar archive could not be created, skipping\n" "$label"
        return 0
    fi
    run_roundtrip "$label" "$archive" "$floor"
}

run_individual_catalog() {
    local label="$1"
    local dir="$2"
    shift 2
    local files=("$@")

    if [ ! -d "$dir" ]; then
        printf "%s directory not found at %s, skipping\n" "$label" "$dir"
        return 0
    fi

    printf "\n%s\n" "$label"
    printf "%-18s %10s %10s %8s %10s\n" "FILE" "ORIG" "OUT" "BPB" "CODEC"
    printf "%s\n" "------------------------------------------------------------------"

    local seen=0
    local file floor path
    for file in "${files[@]}"; do
        path="$dir/$file"
        if [ ! -f "$path" ]; then
            printf "%-18s %10s\n" "$file" "missing"
            continue
        fi
        floor=""
        if [ "$label" = "Calgary individual files" ]; then
            case "$file" in
                bib) floor=4.568 ;;
                book1) floor=5.672 ;;
                book2) floor=4.633 ;;
                geo) floor=5.392 ;;
                news) floor=2.861 ;;
                obj1) floor=7.138 ;;
                obj2) floor=2.570 ;;
                paper1) floor=5.401 ;;
                paper2) floor=5.478 ;;
                pic) floor=1.514 ;;
                progc) floor=5.322 ;;
                progl) floor=3.482 ;;
                progp) floor=3.544 ;;
                trans) floor=3.013 ;;
            esac
        fi

        local compressed="$WORKDIR/$file.nxc"
        local recovered="$WORKDIR/$file.dec"
        "$BIN" compress "$path" "$compressed" >/dev/null
        "$BIN" decompress "$compressed" "$recovered" >/dev/null
        cmp -s "$path" "$recovered"

        local orig size bpb codec
        orig="$(filesize "$path")"
        size="$(filesize "$compressed")"
        bpb="$(bpb_of "$size" "$orig")"
        if "$BIN" inspect "$compressed" --show-codec >/dev/null 2>&1; then
            codec="$("$BIN" inspect "$compressed" --show-codec 2>/dev/null | tail -n 1)"
        else
            codec="n/a"
        fi

        printf "%-18s %10s %10s %8s %10s\n" "$file" "$orig" "$size" "$bpb" "$codec"
        if [ -n "${floor:-}" ]; then
            floor_check "$label/$file" "$bpb" "$floor"
        fi
        seen=$((seen + 1))
    done

    if [ "$seen" -eq 0 ]; then
        printf "%s had no readable files, skipping\n" "$label"
    fi
}

run_competitor() {
    local tool="$1"
    local input="$2"
    local output="$3"
    shift 3
    if ! have_cmd "$tool"; then
        printf "%s not found, skipping competitor\n" "$tool"
        return 0
    fi
    "$tool" "$@" -c "$input" > "$output"
    filesize "$output"
}

echo "============================================"
echo " NEXCOMP Benchmark Suite v1.2"
echo " $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "============================================"

echo
echo "[1/4] Building release binary"
cd "$ROOT"
cargo build --release --quiet

echo
echo "[2/4] Smoke tests"
cargo test -q --test v12_no_regression -- --nocapture

echo
echo "[3/4] Corpus floors"
printf "%-18s %10s %10s %8s %10s\n" "LABEL" "ORIG" "OUT" "BPB" "CODEC"
printf "%s\n" "------------------------------------------------------------------"

run_roundtrip "corpus.tar" "$ROOT/tests/fixtures/corpus.tar" 1.210
run_tar_dir "Calgary TAR" "${NEXCOMP_CALGARY_DIR:-/tmp/nexcomp_corpora/calgary}" 2.557
run_tar_dir "Canterbury TAR" "${NEXCOMP_CANTERBURY_DIR:-/tmp/nexcomp_corpora/canterbury}" 1.807

echo
echo "[4/4] Individual catalogs"
run_individual_catalog "Calgary individual files" "${NEXCOMP_CALGARY_DIR:-/tmp/nexcomp_corpora/calgary}" \
    bib book1 book2 geo news obj1 obj2 paper1 paper2 pic progc progl progp trans
run_individual_catalog "Canterbury individual files" "${NEXCOMP_CANTERBURY_DIR:-/tmp/nexcomp_corpora/canterbury}" \
    alice29.txt asyoulik.txt cp.html fields.c grammar.lsp kennedy.xls lcet10.txt plrabn12.txt ptt5 sum xargs.1

echo
echo "Done."
