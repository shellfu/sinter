#!/bin/bash
# sinter-first: Claude Code enforcement hooks (managed by `sinter install`;
# edits are overwritten). Injects sinter routing context when the cwd (or
# an ancestor, git-style) has a built sinter graph. Context injection
# ONLY — never a permissionDecision: "allow" would auto-approve the whole
# Bash command, including anything destructive sharing a line with a grep.
# Modes:
#   prompt   — UserPromptSubmit: one always-on router line
#   grep     — PreToolUse(Bash): nudge on recursive-search commands, plus a
#              git-archaeology nudge on git show/diff/diff-tree/log
#   greptool — PreToolUse(Grep): nudge for the dedicated Grep tool
#   task     — PreToolUse(Task|Agent): orchestration rule injected at
#              subagent spawn, so prompts written for subagents mandate
#              sinter for structure claims instead of steering to grep
root=""
d="$PWD"
while [ "$d" != "/" ]; do
  if [ -e "$d/.sinter/graph.redb" ]; then root="$d"; break; fi
  d="$(dirname "$d")"
done
[ -z "$root" ] && exit 0

NUDGE="sinter graph available: if this search asks a structure question (symbol location, callers, blast radius, impact), one sinter call answers it ranked and evidence-backed. Grep remains right for content/function-body text."
TASK_NUDGE="sinter graph available: you are writing a subagent prompt. Structure claims (who calls X, is Y a dependency of Z, blast radius, any *no callers/no usages* proof) must be answered by sinter ask/show/affected/path/impact, never by grep. Mandate that routing in the subagent prompt; steer grep/rg to content-only searches."
GIT_NUDGE="sinter graph available: if you are assessing what a commit or diff changes or affects downstream, sinter impact <rev-range> (e.g. HEAD~1..HEAD) answers changed symbols, blast radius, and affected tests in one call."

case "$1" in
  prompt)
    echo "This repo has a sinter code graph. For structure questions (where is X, who calls X, blast radius, how A reaches B, what a commit/diff affects) query sinter before grep or git archaeology: sinter ask/query/show/affected/path/impact. Queries self-sync against uncommitted edits."
    ;;
  grep)
    cmd=$(jq -r '.tool_input.command // empty' 2>/dev/null)
    if printf '%s' "$cmd" | grep -qE '(^|[|;& ])(rg |grep +(-[a-zA-Z]*[rR]|.* -[rR]))'; then
      printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$NUDGE"
    elif printf '%s' "$cmd" | grep -qE '(^|[|;& ])git +(show|diff|diff-tree|log)\b'; then
      printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$GIT_NUDGE"
    fi
    ;;
  greptool)
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$NUDGE"
    ;;
  task)
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$TASK_NUDGE"
    ;;
esac
exit 0
