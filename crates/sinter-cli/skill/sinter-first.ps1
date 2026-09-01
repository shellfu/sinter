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

$Nudge = 'sinter graph present: for symbol/caller/dependency/blast-radius questions use sinter query/show/affected/deps/path or sinter grep --within; rg only for unbounded text.'
$GitNudge = 'sinter graph: impact <rev-range> for changed symbols and downstream effects; overlap for collision risk; --workspace for cross-repo.'

$DenyReason = 'This repo has a sinter graph. Use sinter context <task>, sinter ask, query/show, affected/deps/path, assert no-callers (accept only holds_for_indexed_snapshot, retain universe/limitations), unresolved for graph gaps (not_proven is non-conclusive), cite/verify-doc, grep --within affected(SYM), or impact. If insufficient, rerun this exact search.'

function Emit([string]$Text) {
    Write-Output ('{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"' + $Text + '"}}')
}

# Return a marker path for a valid session without placing the raw session ID
# in the filesystem. The platform temp directory is per-user on Windows.
function Get-SessionMarkerPath([string]$Class, [string]$InputJson) {
    $sid = ''
    try { $sid = ([string]($InputJson | ConvertFrom-Json).session_id) } catch { $sid = '' }
    if ([string]::IsNullOrWhiteSpace($sid)) { return $null }
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $hash = $sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($sid)) } finally { $sha.Dispose() }
    $token = -join ($hash | ForEach-Object { $_.ToString('x2') })
    $markerDir = Join-Path ([IO.Path]::GetTempPath()) 'sinter-hooks'
    try { New-Item -ItemType Directory -Path $markerDir -Force | Out-Null } catch { return $null }
    return Join-Path $markerDir "$Class-$token"
}

# Return true only for the first marker in a valid session, false for an
# existing marker, and null when no safe marker can be created. CreateNew is
# atomic across concurrent hook processes and never follows an existing file.
function New-SessionMarker([string]$Class, [string]$InputJson) {
    $marker = Get-SessionMarkerPath $Class $InputJson
    if (-not $marker) { return $null }
    try {
        $stream = [IO.File]::Open($marker, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $stream.Dispose()
        return $true
    }
    catch [IO.IOException] { return $false }
    catch { return $null }
}

function Test-StrictDeny([string]$InputJson) {
    return (New-SessionMarker 'strict' $InputJson) -eq $true
}

function Emit-Once([string]$Class, [string]$InputJson, [string]$Text) {
    $first = New-SessionMarker $Class $InputJson
    if ($null -eq $first -or $first) { Emit $Text }
}

function EmitDeny {
    Write-Output ('{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"' + $DenyReason + '"}}')
}

switch ($Mode) {
    'prompt' {
        $raw = ''
        try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        if ((New-SessionMarker 'prompt' $raw) -eq $false) { exit 0 }
        Write-Output 'This repo has a sinter graph. Unfamiliar repo: sinter map; starting a task: sinter context <task>; vague discovery: ask; exact symbol: query/show; relations: affected/deps/path; production-caller proof: assert no-callers (accept only holds_for_indexed_snapshot, retain universe/limitations); unresolved for graph gaps, not_proven is non-conclusive; cite/verify-doc for citations; grep --within affected(SYM) for bounded text; impact/overlap for diffs.'
    }
    { $_ -in 'grep', 'grep-strict' } {
        $raw = ''
        try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        $cmd = ''
        try { $cmd = ($raw | ConvertFrom-Json).tool_input.command } catch { $cmd = '' }
        if (-not $cmd) { exit 0 }
        if ($cmd -match '(^|[|;& ])(rg |ag |git +grep|(xargs|-exec) +(grep|rg)|grep +(-[a-zA-Z]*[rR]|.* -[rR])|find .* -i?name)') {
            if ($Mode -eq 'grep-strict' -and (Test-StrictDeny $raw)) { EmitDeny } else { Emit-Once 'search' $raw $Nudge }
        }
        # Git archaeology stays advisory in both modes.
        elseif ($cmd -match '(^|[|;& ])git +log .*-[SG]') { Emit-Once 'git' $raw $GitNudge }
    }
    { $_ -in 'greptool', 'greptool-strict' } {
        $raw = ''
        try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        if ($Mode -eq 'greptool-strict' -and (Test-StrictDeny $raw)) { EmitDeny } else { Emit-Once 'search' $raw $Nudge }
    }
}
exit 0
