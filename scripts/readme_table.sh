#!/usr/bin/env bash
# Print the README results table from a benchmark artifact, so the published
# numbers are a view of a recorded run and not hand-typed.
#
#   scripts/readme_table.sh [bench/results/<run>.tsv]    default: the newest run

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# A corpus the size of Silesia is a run of its own, so a table may come from
# more than one artifact; every run uses the same script and manifest.
if [ "$#" -gt 0 ]; then tsvs=("$@"); else tsvs=("$(ls -t "$ROOT"/bench/results/*.tsv | head -1)"); fi

awk -F'\t' '
    $2 == "total" {
        size[$1 "\t" $3] = $5
        bpb[$1 "\t" $3] = $6
        orig[$1] = $4
        if (!($1 in seen)) { order[++n] = $1; seen[$1] = 1 }
        if (!($3 in tool_seen)) { tools[++t] = $3; tool_seen[$3] = 1 }
    }
    END {
        header = "| corpus | original | NEXCOMP | bpb"
        sep = "|---|---|---|---"
        for (i = 1; i <= t; i++) if (tools[i] != "nexcomp") { header = header " | " tools[i]; sep = sep "|---" }
        print header " |"
        print sep "|"
        for (i = 1; i <= n; i++) {
            c = order[i]
            line = "| " c " | " orig[c] " | " size[c "\tnexcomp"] " | " bpb[c "\tnexcomp"]
            for (j = 1; j <= t; j++) if (tools[j] != "nexcomp") line = line " | " size[c "\t" tools[j]]
            print line " |"
        }
    }
' "${tsvs[@]}"

echo
echo "Measured by \`scripts/benchmark.sh\`; environment and per-file numbers in"
links=""
for tsv in "${tsvs[@]}"; do
    md="$(basename "${tsv%.tsv}.md")"
    [ -n "$links" ] && links="$links, "
    links="$links[\`bench/results/$md\`](bench/results/$md)"
done
echo "$links."
