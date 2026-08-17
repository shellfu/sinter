#!/bin/bash
# sinter-first: Claude Code enforcement hooks (managed by `sinter install`;
# edits are overwritten). Injects sinter routing context when the cwd (or
# an ancestor, git-style) has a built sinter graph. Context injection
# ONLY — never a permissionDecision: "allow" would auto-approve the whole
# Bash command, including anything destructive sharing a line with a grep.
# Modes:
#   prompt   — UserPromptSubmit: one always-on router line
#   grep     — PreToolUse(Bash): nudge attached to recursive-search commands
#   greptool — PreToolUse(Grep): nudge for the dedicated Grep tool
root=""
d="$PWD"
while [ "$d" != "/" ]; do
  if [ -e "$d/.sinter/graph.redb" ]; then root="$d"; break; fi
  d="$(dirname "$d")"
done
[ -z "$root" ] && exit 0

NUDGE="sinter graph available: if this search asks a structure question (symbol location, callers, blast radius, impact), one sinter call answers it ranked and evidence-backed. Grep remains right for content/function-body text."

case "$1" in
  prompt)
    echo "This repo has a sinter code graph. For structure questions (where is X, who calls X, blast radius, how A reaches B, diff impact) query sinter before grep: sinter ask/query/show/affected/path/impact. Queries self-sync against uncommitted edits."
    ;;
  grep)
    cmd=$(jq -r '.tool_input.command // empty' 2>/dev/null)
    if printf '%s' "$cmd" | grep -qE '(^|[|;& ])(rg |grep +(-[a-zA-Z]*[rR]|.* -[rR]))'; then
      printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$NUDGE"
    fi
    ;;
  greptool)
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$NUDGE"
    ;;
esac
exit 0
