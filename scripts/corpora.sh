#!/usr/bin/env bash
# Download the benchmark corpora and verify them against bench/manifest.tsv.
#
#   scripts/corpora.sh              download what is missing, then verify
#   scripts/corpora.sh verify       verify only
#   scripts/corpora.sh manifest     rewrite bench/manifest.tsv from what is on disk
#
# The corpora live in $NEXCOMP_CORPORA_DIR (default ~/corpora). Only the files
# named in the manifest are part of a benchmark: the Calgary corpus is the
# standard 14 files, not the extended set some mirrors ship.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/bench/manifest.tsv"
CORPORA_DIR="${NEXCOMP_CORPORA_DIR:-$HOME/corpora}"

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

fetch() {
    local url="$1" out="$2"
    echo "  fetching $url"
    curl -fsSL --retry 3 -o "$out" "$url"
}

download() {
    local tmp
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN

    if [ ! -d "$CORPORA_DIR/calgary" ]; then
        fetch "https://corpus.canterbury.ac.nz/resources/calgary.tar.gz" "$tmp/calgary.tar.gz"
        mkdir -p "$CORPORA_DIR/calgary"
        tar xzf "$tmp/calgary.tar.gz" -C "$CORPORA_DIR/calgary"
    fi
    if [ ! -d "$CORPORA_DIR/canterbury" ]; then
        fetch "https://corpus.canterbury.ac.nz/resources/cantrbry.tar.gz" "$tmp/cantrbry.tar.gz"
        mkdir -p "$CORPORA_DIR/canterbury"
        tar xzf "$tmp/cantrbry.tar.gz" -C "$CORPORA_DIR/canterbury"
    fi
    if [ ! -d "$CORPORA_DIR/silesia" ]; then
        fetch "https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip" "$tmp/silesia.zip"
        mkdir -p "$CORPORA_DIR/silesia"
        unzip -qo "$tmp/silesia.zip" -d "$CORPORA_DIR/silesia"
    fi
    # Not part of the published benchmark: the corpus no selector gate was tuned on.
    if [ ! -d "$CORPORA_DIR/canterbury-large" ]; then
        fetch "https://corpus.canterbury.ac.nz/resources/large.tar.gz" "$tmp/large.tar.gz"
        mkdir -p "$CORPORA_DIR/canterbury-large"
        tar xzf "$tmp/large.tar.gz" -C "$CORPORA_DIR/canterbury-large"
    fi
    if [ ! -f "$CORPORA_DIR/enwik8/enwik8" ]; then
        fetch "https://mattmahoney.net/dc/enwik8.zip" "$tmp/enwik8.zip"
        mkdir -p "$CORPORA_DIR/enwik8"
        unzip -qo "$tmp/enwik8.zip" -d "$CORPORA_DIR/enwik8"
    fi
}

verify() {
    local failures=0 checked=0
    while IFS=$'\t' read -r corpus file size hash; do
        case "$corpus" in \#*|"") continue ;; esac
        local path="$CORPORA_DIR/$corpus/$file"
        if [ ! -f "$path" ]; then
            echo "  MISSING  $corpus/$file"
            failures=$((failures + 1))
            continue
        fi
        local actual_size actual_hash
        actual_size="$(wc -c < "$path" | tr -d ' ')"
        actual_hash="$(sha256 "$path")"
        if [ "$actual_size" != "$size" ] || [ "$actual_hash" != "$hash" ]; then
            echo "  DIFFERS  $corpus/$file ($actual_size bytes, $actual_hash)"
            failures=$((failures + 1))
        fi
        checked=$((checked + 1))
    done < "$MANIFEST"
    echo "  $checked files verified, $failures problems"
    [ "$failures" -eq 0 ]
}

write_manifest() {
    {
        echo "# NEXCOMP benchmark corpora. corpus<TAB>file<TAB>size<TAB>sha256"
        echo "# calgary:    https://corpus.canterbury.ac.nz/resources/calgary.tar.gz (standard 14 files)"
        echo "# canterbury: https://corpus.canterbury.ac.nz/resources/cantrbry.tar.gz"
        echo "# canterbury-large: https://corpus.canterbury.ac.nz/resources/large.tar.gz (oracle only)"
        echo "# silesia:    https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip"
        echo "# enwik8:     https://mattmahoney.net/dc/enwik8.zip"
        for corpus in calgary canterbury canterbury-large silesia enwik8; do
            for file in $(files_of "$corpus"); do
                local path="$CORPORA_DIR/$corpus/$file"
                printf '%s\t%s\t%s\t%s\n' "$corpus" "$file" "$(wc -c < "$path" | tr -d ' ')" "$(sha256 "$path")"
            done
        done
    } > "$MANIFEST"
    echo "  wrote $MANIFEST"
}

# The files of a corpus, in the order the benchmark reports them.
files_of() {
    case "$1" in
        # The standard Calgary corpus; paper3..paper6 are no longer part of it.
        calgary) echo "bib book1 book2 geo news obj1 obj2 paper1 paper2 pic progc progl progp trans" ;;
        canterbury) echo "alice29.txt asyoulik.txt cp.html fields.c grammar.lsp kennedy.xls lcet10.txt plrabn12.txt ptt5 sum xargs.1" ;;
        canterbury-large) echo "E.coli bible.txt world192.txt" ;;
        silesia) echo "dickens mozilla mr nci ooffice osdb reymont samba sao webster x-ray xml" ;;
        enwik8) echo "enwik8" ;;
    esac
}

case "${1:-download}" in
    download) download; verify ;;
    verify) verify ;;
    manifest) write_manifest ;;
    *) echo "usage: $0 [download|verify|manifest]" >&2; exit 2 ;;
esac
