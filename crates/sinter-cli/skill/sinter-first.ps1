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
#              sinter (marker file <temp>/sinter-strict-<session>); every
#              later one falls through to the nudge. Sinter-first,
#              grep-second, never grep-never. No session_id → nudge only.
param([string]$Mode)

$root = $null
$d = (Get-Location).Path
while ($d) {
    if (Test-Path (Join-Path $d '.sinter/graph.redb')) { $root = $d; break }
    $parent = Split-Path $d -Parent
    if (-not $parent -or $parent -eq $d) { break }
    $d = $parent
}
if (-not $root) { exit 0 }

$Nudge = 'sinter graph available: if this search asks a structure question (symbol location, callers, blast radius, impact), one sinter call answers it ranked and evidence-backed. Grep remains right for content/function-body text.'
$TaskNudge = 'sinter graph available: you are writing a subagent prompt. Structure claims (who calls X, is Y a dependency of Z, blast radius, any *no callers/no usages* proof) must be answered by sinter ask/show/affected/deps/path/impact, never by grep. Mandate that routing in the subagent prompt; steer grep/rg to content-only searches.'
$GitNudge = 'sinter graph available: if you are assessing what a commit or diff changes or affects downstream, sinter impact <rev-range> (e.g. HEAD~1..HEAD) answers changed symbols, blast radius, and affected tests in one call.'

$DenyReason = 'This repo has a sinter code graph: query sinter first for structure questions — sinter ask \"<question>\", sinter show <symbol>, sinter affected <symbol>, sinter path <A> <B>, sinter impact <rev-range>. If sinter was insufficient, rerun this exact search and it will be allowed.'

function Emit([string]$Text) {
    Write-Output ('{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"' + $Text + '"}}')
}

# True when this call is the session's first matching search — creates the
# marker so the retry passes. Never denies without a session_id to scope
# the marker.
function Test-StrictDeny([string]$InputJson) {
    $sid = ''
    try { $sid = ([string]($InputJson | ConvertFrom-Json).session_id) } catch { $sid = '' }
    if ($sid) { $sid = $sid -replace '[^A-Za-z0-9._-]', '' }
    if (-not $sid) { return $false }
    $marker = Join-Path ([IO.Path]::GetTempPath()) "sinter-strict-$sid"
    if (Test-Path $marker) { return $false }
    New-Item -ItemType File -Path $marker -Force | Out-Null
    return $true
}

function EmitDeny {
    Write-Output ('{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"' + $DenyReason + '"}}')
}

switch ($Mode) {
    'prompt' {
        Write-Output 'This repo has a sinter code graph. For structure questions (where is X, who calls X, blast radius, how A reaches B, what a commit/diff affects) query sinter before grep or git archaeology: sinter ask/query/show/affected/deps/path/impact. Queries self-sync against uncommitted edits.'
    }
    { $_ -in 'grep', 'grep-strict' } {
        $raw = ''
        try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        $cmd = ''
        try { $cmd = ($raw | ConvertFrom-Json).tool_input.command } catch { $cmd = '' }
        if (-not $cmd) { exit 0 }
        if ($cmd -match '(^|[|;& ])(rg |grep +(-[a-zA-Z]*[rR]|.* -[rR]))') {
            if ($Mode -eq 'grep-strict' -and (Test-StrictDeny $raw)) { EmitDeny } else { Emit $Nudge }
        }
        # Git archaeology stays advisory in both modes.
        elseif ($cmd -match '(^|[|;& ])git +(show|diff|diff-tree|log)\b') { Emit $GitNudge }
    }
    { $_ -in 'greptool', 'greptool-strict' } {
        $raw = ''
        if ($Mode -eq 'greptool-strict') {
            try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        }
        if ($Mode -eq 'greptool-strict' -and (Test-StrictDeny $raw)) { EmitDeny } else { Emit $Nudge }
    }
    'task' { Emit $TaskNudge }
}
exit 0
