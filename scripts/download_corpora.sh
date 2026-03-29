#!/bin/bash
set -euo pipefail

CORPORA_DIR="${NEXCOMP_CORPORA_DIR:-/tmp/nexcomp_corpora}"

download_targz_if_missing() {
    local name="$1" url="$2" dir="$3"
    if [ -d "$dir" ] && [ "$(ls -A "$dir" 2>/dev/null)" ]; then
        echo "  $name: already present at $dir"
        return 0
    fi
    echo "  $name: downloading from $url ..."
    mkdir -p "$dir"
    local tmpfile="/tmp/nexcomp_dl_${name}.tar.gz"
    curl -L -o "$tmpfile" "$url"
    tar xzf "$tmpfile" -C "$dir"
    rm -f "$tmpfile"
    echo "  $name: done ($(ls "$dir" | wc -l | tr -d ' ') files)"
}

download_zip_if_missing() {
    local name="$1" url="$2" dir="$3"
    if [ -d "$dir" ] && [ "$(ls -A "$dir" 2>/dev/null)" ]; then
        echo "  $name: already present at $dir"
        return 0
    fi
    echo "  $name: downloading from $url ..."
    mkdir -p "$dir"
    local tmpfile="/tmp/nexcomp_dl_${name}.zip"
    curl -L -o "$tmpfile" "$url"
    unzip -o "$tmpfile" -d "$dir"
    rm -f "$tmpfile"
    echo "  $name: done ($(ls "$dir" | wc -l | tr -d ' ') files)"
}

echo "=== NEXCOMP Corpus Downloader ==="

# Calgary (tar.gz)
download_targz_if_missing "calgary" \
    "https://corpus.canterbury.ac.nz/resources/calgary.tar.gz" \
    "$CORPORA_DIR/calgary"

# Canterbury (tar.gz)
download_targz_if_missing "canterbury" \
    "https://corpus.canterbury.ac.nz/resources/cantrbry.tar.gz" \
    "$CORPORA_DIR/canterbury"

# Silesia (211MB, zip)
download_zip_if_missing "silesia" \
    "https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip" \
    "$CORPORA_DIR/silesia"

# enwik8 (100MB, zip)
if [ -f "$CORPORA_DIR/enwik8/enwik8" ]; then
    echo "  enwik8: already present at $CORPORA_DIR/enwik8"
else
    echo "  enwik8: downloading..."
    mkdir -p "$CORPORA_DIR/enwik8"
    curl -L -o "/tmp/nexcomp_dl_enwik8.zip" "https://mattmahoney.net/dc/enwik8.zip"
    unzip -o "/tmp/nexcomp_dl_enwik8.zip" -d "$CORPORA_DIR/enwik8"
    rm -f "/tmp/nexcomp_dl_enwik8.zip"
    echo "  enwik8: done"
fi

echo ""
echo "=== Available corpora ==="
for d in "$CORPORA_DIR"/*/; do
    name=$(basename "$d")
    count=$(ls "$d" | wc -l | tr -d ' ')
    size=$(du -sh "$d" | cut -f1)
    echo "  $name: $count files, $size"
done
