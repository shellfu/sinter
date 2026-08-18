#!/bin/bash
# sinter-first: Claude Code enforcement hooks (managed by `sinter install`;
# edits are overwritten). Injects sinter routing context when the cwd (or
# an ancestor, git-style) has a built sinter graph.
# SECURITY INVARIANT (absolute): this script must NEVER emit
# permissionDecision "allow" — that would auto-approve the ENTIRE Bash
# command, including anything destructive sharing a line with a grep.
# "deny" only blocks and is safe; default modes stay context-injection
# only and emit no permissionDecision at all.
# Modes:
#   prompt   — UserPromptSubmit: one always-on router line
#   grep     — PreToolUse(Bash): nudge on recursive-search commands, plus a
#              git-archaeology nudge on git show/diff/diff-tree/log
#   greptool — PreToolUse(Grep): nudge for the dedicated Grep tool
#   task     — PreToolUse(Task|Agent): orchestration rule injected at
#              subagent spawn, so prompts written for subagents mandate
#              sinter for structure claims instead of steering to grep
#   grep-strict / greptool-strict — opt-in strict variants: the FIRST
#              matching search of a session is denied with a redirect to
#              sinter (marker file ${TMPDIR:-/tmp}/sinter-strict-<session>);
#              every later one falls through to the nudge. Sinter-first,
#              grep-second, never grep-never. No session_id → nudge only.
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
DENY_REASON="This repo has a sinter code graph: query sinter first for structure questions — sinter ask \\\"<question>\\\", sinter show <symbol>, sinter affected <symbol>, sinter path <A> <B>, sinter impact <rev-range>. If sinter was insufficient, rerun this exact search and it will be allowed."

# strict_deny <input-json>: succeed (0) when this call is the session's
# first matching search — creates the marker so the retry passes. Never
# denies without a session_id to scope the marker.
strict_deny() {
  sid=$(printf '%s' "$1" | jq -r '.session_id // empty' 2>/dev/null | tr -cd 'A-Za-z0-9._-')
  [ -z "$sid" ] && return 1
  marker="${TMPDIR:-/tmp}/sinter-strict-$sid"
  [ -e "$marker" ] && return 1
  : > "$marker"
  return 0
}
emit_deny() {
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}' "$DENY_REASON"
}

case "$1" in
  prompt)
    echo "This repo has a sinter code graph. For structure questions (where is X, who calls X, blast radius, how A reaches B, what a commit/diff affects) query sinter before grep or git archaeology: sinter ask/query/show/affected/path/impact. Queries self-sync against uncommitted edits."
    ;;
  grep|grep-strict)
    input=$(cat)
    cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)
    if printf '%s' "$cmd" | grep -qE '(^|[|;& ])(rg |grep +(-[a-zA-Z]*[rR]|.* -[rR]))'; then
      if [ "$1" = "grep-strict" ] && strict_deny "$input"; then
        emit_deny
      else
        printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$NUDGE"
      fi
    elif printf '%s' "$cmd" | grep -qE '(^|[|;& ])git +(show|diff|diff-tree|log)\b'; then
      # Git archaeology stays advisory in both modes.
      printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$GIT_NUDGE"
    fi
    ;;
  greptool|greptool-strict)
    if [ "$1" = "greptool-strict" ] && strict_deny "$(cat)"; then
      emit_deny
    else
      printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$NUDGE"
    fi
    ;;
  task)
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$TASK_NUDGE"
    ;;
esac
exit 0
