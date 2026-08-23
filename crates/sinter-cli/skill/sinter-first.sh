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
#   prompt   — UserPromptSubmit: one router line, once per session
#   grep     — PreToolUse(Bash): nudge on recursive-search commands
#              (rg/ag/grep -r/git grep/find -name), plus a git-archaeology
#              nudge on `git log -S/-G`; each class at most once per session
#   greptool — PreToolUse(Grep): nudge for the dedicated Grep tool (shares
#              the search class with grep)
#   grep-strict / greptool-strict — opt-in strict variants: the FIRST
#              matching search of a session is denied with a redirect to
#              sinter; the retry gets one advisory nudge, and later searches
#              in that session are silent. Sinter-first, grep-second, never
#              grep-never. No session_id → nudge only.
root=""
d="$PWD"
while [ "$d" != "/" ]; do
  if [ -e "$d/.sinter/graph.redb" ]; then root="$d"; break; fi
  d="$(dirname "$d")"
done
[ -z "$root" ] && exit 0

NUDGE="sinter graph: unfamiliar repo -> map; vague discovery -> ask; exact symbol -> query/show; relations -> affected/deps/path; negative proof -> unresolved, with incomplete coverage reported as not_proven. Grep remains for content/function bodies."
GIT_NUDGE="sinter graph: use impact <rev-range> for changed symbols, downstream effects, and tests; use overlap for collision risk; add --workspace for cross-repo analysis."
DENY_REASON="This repo has a sinter graph. Run sinter map first if unfamiliar; use sinter ask for vague discovery, sinter query/show for exact symbols, sinter affected/deps/path for relations, sinter unresolved for negative proofs (incomplete coverage is not_proven), or sinter impact for diffs. If insufficient, rerun this exact search."

# Print a marker path for a valid session without placing the raw session ID
# in the filesystem. The per-user directory is private; refusing an unsafe
# directory makes callers use the visible no-session fallback.
session_marker_path() {
  local class=$1 input=$2 sid token uid marker_dir old_umask made_dir
  sid=$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)
  [ -z "$sid" ] && return 1
  if command -v sha256sum >/dev/null 2>&1; then
    token=$(printf '%s' "$sid" | sha256sum | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    token=$(printf '%s' "$sid" | shasum -a 256 | awk '{print $1}')
  else
    return 1
  fi
  uid=$(id -u 2>/dev/null) || return 1
  marker_dir="${TMPDIR:-/tmp}/sinter-hooks-$uid"
  [ -L "$marker_dir" ] && return 1
  old_umask=$(umask)
  umask 077
  mkdir -p -- "$marker_dir" 2>/dev/null
  made_dir=$?
  umask "$old_umask"
  [ "$made_dir" -eq 0 ] && [ -d "$marker_dir" ] && [ -O "$marker_dir" ] || return 1
  printf '%s/%s-%s' "$marker_dir" "$class" "$token"
}

# Succeed only for the first marker in a valid session. Marker directories
# make the check-and-create atomic across concurrent hook processes. Return 2
# when there is no safe session marker so advisory callers remain visible.
mark_session_once() {
  local marker
  marker=$(session_marker_path "$1" "$2") || return 2
  if mkdir -- "$marker" 2>/dev/null; then
    return 0
  fi
  [ -d "$marker" ] && [ ! -L "$marker" ] && return 1
  return 2
}

strict_deny() {
  mark_session_once strict "$1"
}

emit_once() {
  local class=$1 input=$2 text=$3 marked
  mark_session_once "$class" "$input"
  marked=$?
  if [ "$marked" -eq 0 ] || [ "$marked" -eq 2 ]; then
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}' "$text"
  fi
}
emit_deny() {
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}' "$DENY_REASON"
}

case "$1" in
  prompt)
    input=$(cat)
    mark_session_once prompt "$input"
    [ $? -eq 1 ] && exit 0
    echo "This repo has a sinter graph. Unfamiliar repo: sinter map first. Then use ask for vague discovery; query/show for exact symbols; affected/deps/path for relations; unresolved for negative proofs (incomplete coverage is not_proven); impact/overlap for changes; workspace/--workspace across repos. Use ensure/doctor/scip for setup or repair; read source for function bodies."
    ;;
  grep|grep-strict)
    input=$(cat)
    cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)
    if printf '%s' "$cmd" | grep -qE '(^|[|;& ])(rg |ag |git +grep|(xargs|-exec) +(grep|rg)|grep +(-[a-zA-Z]*[rR]|.* -[rR])|find .* -i?name)'; then
      if [ "$1" = "grep-strict" ] && strict_deny "$input"; then
        emit_deny
      else
        emit_once search "$input" "$NUDGE"
      fi
    elif printf '%s' "$cmd" | grep -qE '(^|[|;& ])git +log .*-[SG]'; then
      # Git archaeology stays advisory in both modes.
      emit_once git "$input" "$GIT_NUDGE"
    fi
    ;;
  greptool|greptool-strict)
    input=$(cat)
    if [ "$1" = "greptool-strict" ] && strict_deny "$input"; then
      emit_deny
    else
      emit_once search "$input" "$NUDGE"
    fi
    ;;
esac
exit 0
