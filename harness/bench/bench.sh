#!/usr/bin/env bash
# Head-to-head benchmark: sinter vs the prototype (graphify), same machine,
# same repos, same questions. Raw outputs land in harness/bench/logs/;
# this script prints a markdown summary. No network, no API — both tools
# run their local binaries only.
set -u
SINTER=${SINTER:-$(dirname "$0")/../../target/release/sinter}
GRAPHIFY=${GRAPHIFY:-graphify}
OUT=$(dirname "$0")/logs
mkdir -p "$OUT"
TIMEOUT=${TIMEOUT:-900}

declare -A REPOS=(
  [bl]="$HOME/Black_Lantern_Complete_Build_Guide_Documents"
  [sinter]="$HOME/projects/sinter"
  [proto]="$HOME/projects/graphify"
  [skaffold]="$HOME/projects/skaffold"
)

ms() { # wall-clock ms of a command
  local start end
  start=$(date +%s%N)
  "$@" >/dev/null 2>&1
  end=$(date +%s%N)
  echo $(( (end - start) / 1000000 ))
}

echo "| repo | tool | index build | reindex (no-op) | artifact size |"
echo "|---|---|---|---|---|"
for key in bl sinter proto skaffold; do
  repo=${REPOS[$key]}
  [ -d "$repo" ] || continue
  # --- sinter ---
  rm -rf "$repo/.sinter"
  s_build=$(ms "$SINTER" build "$repo")
  s_noop=$(ms "$SINTER" build "$repo")
  s_size=$(du -sh "$repo/.sinter" 2>/dev/null | cut -f1)
  echo "| $key | sinter | ${s_build}ms | ${s_noop}ms | $s_size |"
  # --- prototype ---
  rm -rf "$repo/graphify-out"
  g_build=$(ms timeout "$TIMEOUT" "$GRAPHIFY" update "$repo")
  if [ ! -f "$repo/graphify-out/graph.json" ]; then
    echo "| $key | graphify | DNF (>${TIMEOUT}s or error) | - | - |"
    continue
  fi
  g_noop=$(ms timeout "$TIMEOUT" "$GRAPHIFY" update "$repo")
  g_size=$(du -sh "$repo/graphify-out" 2>/dev/null | cut -f1)
  echo "| $key | graphify | ${g_build}ms | ${g_noop}ms | $g_size |"
done

echo
echo "| repo | question | tool | latency | rank of expected | output chars |"
echo "|---|---|---|---|---|---|"
while IFS=$'\t' read -r key question expected; do
  case "$key" in \#*|"") continue;; esac
  repo=${REPOS[$key]}
  [ -d "$repo" ] || continue
  slug=$(echo "$key-$question" | tr -cs 'a-zA-Z0-9' '-' | cut -c1-50)

  # sinter ask: rank = numbered hit containing expected
  t0=$(date +%s%N); "$SINTER" ask "$question" --repo "$repo" --limit 10 \
    > "$OUT/$slug.sinter.txt" 2>&1; t1=$(date +%s%N)
  s_lat=$(( (t1 - t0) / 1000000 ))
  s_rank=$(grep -n "^[0-9]*\. " "$OUT/$slug.sinter.txt" | grep -i "$expected" \
    | head -1 | sed 's/^\([0-9]*\):.*/\1/')
  s_rank=$(grep "^[0-9]*\. " "$OUT/$slug.sinter.txt" | grep -in "$expected" \
    | head -1 | cut -d: -f1); s_rank=${s_rank:-miss}
  s_chars=$(wc -c < "$OUT/$slug.sinter.txt")
  echo "| $key | ${question:0:38} | sinter | ${s_lat}ms | $s_rank | $s_chars |"

  # graphify query: rank = ordinal of NODE-ish output line containing expected
  if [ -f "$repo/graphify-out/graph.json" ]; then
    t0=$(date +%s%N); timeout 120 "$GRAPHIFY" query "$question" \
      --graph "$repo/graphify-out/graph.json" > "$OUT/$slug.graphify.txt" 2>&1; t1=$(date +%s%N)
    g_lat=$(( (t1 - t0) / 1000000 ))
    g_rank=$(grep -v "^\s*$" "$OUT/$slug.graphify.txt" | grep -in "$expected" \
      | head -1 | cut -d: -f1); g_rank=${g_rank:-miss}
    g_chars=$(wc -c < "$OUT/$slug.graphify.txt")
    echo "| $key | ${question:0:38} | graphify | ${g_lat}ms | line $g_rank | $g_chars |"
  else
    echo "| $key | ${question:0:38} | graphify | no graph | - | - |"
  fi
done < "$(dirname "$0")/questions.tsv"
