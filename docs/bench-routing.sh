#!/bin/bash
# Routing benchmark, phase 1 (Claude Code): do agents pick sinter unprompted?
#
# Clones a repo, onboards it with `sinter init`, then runs clean headless
# Claude sessions asking natural questions that never mention sinter.
# Reads each session's tool trace and reports:
#   - sinter-first rate on graph-eligible questions   (gate: >= 90%)
#   - sinter invocation rate on ineligible questions  (gate: < 10%)
#
# Usage:
#   docs/bench-routing.sh <repo-path-or-url> [reps-per-question]
#
# Model is pinned to Haiku. Override only deliberately:
#   BENCH_MODEL=claude-haiku-4-5-20251001 (default)
#
# EDIT the question arrays for the target repo before a real run — good
# eligible questions name symbols that actually exist in it.
set -euo pipefail

SRC=${1:?usage: bench-routing.sh <repo-path-or-url> [reps-per-question]}
REPS=${2:-2}
MODEL=${BENCH_MODEL:-claude-haiku-4-5-20251001}
case "$MODEL" in
  *haiku*) ;;
  *) echo "refusing non-haiku model '$MODEL' (set BENCH_MODEL explicitly to override... with a haiku model)" >&2; exit 1 ;;
esac

# EDIT ME: structure questions the graph answers (use real symbols).
ELIGIBLE=(
  "What calls connect_grpc_channel? List every call site."
  "What would break if I change the signature of fallback_context?"
  "How does the doctor command reach connect_inner_with_config?"
  "Which functions transitively depend on the config loader?"
  "What does the most recent commit affect downstream?"
)
# EDIT ME: questions sinter should NOT be used for (body/content reading).
INELIGIBLE=(
  "Explain what the body of the main function does, step by step."
  "Is there a TODO or FIXME comment anywhere in the README or docs?"
  "What license is this project under?"
)

WORK=$(mktemp -d)
echo "workdir: $WORK (kept for inspection — delete when done)"
git clone -q "$SRC" "$WORK/repo"
cd "$WORK/repo"
# Neutral start: only what `sinter init` itself installs may steer the agent.
rm -f CLAUDE.md
sinter init --no-scip . >/dev/null
echo "onboarded $(basename "$SRC") with sinter init"

OUT="$WORK/trials"
mkdir -p "$OUT"
n=0
run_trial() { # $1 = eligible|ineligible, $2 = question
  n=$((n + 1))
  local id
  id=$(printf '%03d-%s' "$n" "$1")
  claude -p "$2" \
    --model "$MODEL" \
    --output-format stream-json --verbose \
    --allowedTools Bash Read Grep Glob "mcp__sinter" \
    --mcp-config .mcp.json \
    > "$OUT/$id.jsonl" 2>"$OUT/$id.err" || echo "trial $id: session error" >&2
  echo -n "."
}

for rep in $(seq 1 "$REPS"); do
  for q in "${ELIGIBLE[@]}"; do run_trial eligible "$q"; done
  for q in "${INELIGIBLE[@]}"; do run_trial ineligible "$q"; done
done
echo " done ($n trials)"

python3 - "$OUT" <<'EOF'
import json, sys, glob, os

def classify(path):
    """First search-shaped action wins: sinter vs other. Read = neutral."""
    first = None
    cost = 0.0
    for line in open(path):
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") == "result":
            cost = ev.get("total_cost_usd") or 0.0
        if ev.get("type") != "assistant":
            continue
        for block in ev.get("message", {}).get("content", []):
            if block.get("type") != "tool_use":
                continue
            name = block.get("name", "")
            cmd = (block.get("input") or {}).get("command", "")
            if name.startswith("mcp__sinter") or (name == "Bash" and "sinter " in cmd):
                kind = "sinter"
            elif name in ("Grep", "Glob") or (
                name == "Bash" and any(t in cmd for t in ("grep", "rg ", "find "))
            ):
                kind = "search"
            else:
                continue
            if first is None:
                first = kind
    return first, cost

rows, total_cost = [], 0.0
for path in sorted(glob.glob(os.path.join(sys.argv[1], "*.jsonl"))):
    arm = "eligible" if "eligible" in os.path.basename(path) and "ineligible" not in os.path.basename(path) else "ineligible"
    first, cost = classify(path)
    total_cost += cost
    rows.append((os.path.basename(path), arm, first))
    print(f"  {os.path.basename(path):28} first-search={first}")

el = [r for r in rows if r[1] == "eligible"]
inel = [r for r in rows if r[1] == "ineligible"]
el_hit = sum(1 for r in el if r[2] == "sinter")
inel_hit = sum(1 for r in inel if r[2] == "sinter")
print()
if el:
    rate = el_hit / len(el) * 100
    print(f"eligible: sinter-first {el_hit}/{len(el)} = {rate:.0f}%  (gate: >=90%)")
if inel:
    rate = inel_hit / len(inel) * 100
    print(f"ineligible: sinter used {inel_hit}/{len(inel)} = {rate:.0f}%  (gate: <10%)")
print(f"total cost: ${total_cost:.2f}")
print(f"traces kept in: {sys.argv[1]}")
EOF
