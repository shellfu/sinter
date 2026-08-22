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

$Nudge = 'sinter graph: unfamiliar repo -> map; vague discovery -> ask; exact symbol -> query/show; relations -> affected/deps/path; negative proof -> unresolved, with incomplete coverage reported as not_proven. Grep remains for content/function bodies.'
$TaskNudge = 'sinter graph: require subagents to run map first when unfamiliar, then route structure through ask/query/show/affected/deps/path; use unresolved for negative proofs and report incomplete coverage as not_proven; use impact/overlap for changes. Reserve grep/rg for content.'
$GitNudge = 'sinter graph: use impact <rev-range> for changed symbols, downstream effects, and tests; use overlap for collision risk; add --workspace for cross-repo analysis.'

$DenyReason = 'This repo has a sinter graph. Run sinter map first if unfamiliar; use sinter ask for vague discovery, sinter query/show for exact symbols, sinter affected/deps/path for relations, sinter unresolved for negative proofs (incomplete coverage is not_proven), or sinter impact for diffs. If insufficient, rerun this exact search.'

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
        Write-Output 'This repo has a sinter graph. Unfamiliar repo: sinter map first. Then use ask for vague discovery; query/show for exact symbols; affected/deps/path for relations; unresolved for negative proofs (incomplete coverage is not_proven); impact/overlap for changes; workspace/--workspace across repos. Use ensure/doctor/scip for setup or repair; read source for function bodies.'
    }
    { $_ -in 'grep', 'grep-strict' } {
        $raw = ''
        try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        $cmd = ''
        try { $cmd = ($raw | ConvertFrom-Json).tool_input.command } catch { $cmd = '' }
        if (-not $cmd) { exit 0 }
        if ($cmd -match '(^|[|;& ])(rg |git +grep|(xargs|-exec) +(grep|rg)|grep +(-[a-zA-Z]*[rR]|.* -[rR]))') {
            if ($Mode -eq 'grep-strict' -and (Test-StrictDeny $raw)) { EmitDeny } else { Emit-Once 'search' $raw $Nudge }
        }
        # Git archaeology stays advisory in both modes.
        elseif ($cmd -match '(^|[|;& ])git +(show|diff|diff-tree|log)\b') { Emit-Once 'git' $raw $GitNudge }
    }
    { $_ -in 'greptool', 'greptool-strict' } {
        $raw = ''
        try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        if ($Mode -eq 'greptool-strict' -and (Test-StrictDeny $raw)) { EmitDeny } else { Emit-Once 'search' $raw $Nudge }
    }
    'task' {
        $raw = ''
        try { $raw = [Console]::In.ReadToEnd() } catch { $raw = '' }
        Emit-Once 'task' $raw $TaskNudge
    }
}
exit 0
