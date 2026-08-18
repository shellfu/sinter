# sinter installer for Windows — one static binary, no dependencies.
#
#   irm https://raw.githubusercontent.com/shellfu/sinter/main/install.ps1 | iex
#
# Downloads the latest release binary for this platform, verifies its
# checksum, and installs to %LOCALAPPDATA%\sinter\bin (override with
# $env:SINTER_INSTALL_DIR). Nothing else is touched; uninstall = delete
# the binary.
$ErrorActionPreference = 'Stop'

$Repo = 'shellfu/sinter'
$InstallDir = if ($env:SINTER_INSTALL_DIR) { $env:SINTER_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'sinter\bin' }
$Base = "https://github.com/$Repo/releases/latest/download"

# PROCESSOR_ARCHITEW6432 is set when running a 32-bit shell on a 64-bit OS.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { $target = 'aarch64-pc-windows-msvc' }
    default { throw "unsupported architecture: $arch (build from source: cargo install --git https://github.com/$Repo sinter-cli)" }
}

$asset = "sinter-$target.zip"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("sinter-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "downloading $asset ..."
    Invoke-WebRequest -Uri "$Base/$asset" -OutFile (Join-Path $tmp $asset) -UseBasicParsing
    Invoke-WebRequest -Uri "$Base/$asset.sha256" -OutFile (Join-Path $tmp "$asset.sha256") -UseBasicParsing

    Write-Host 'verifying checksum ...'
    $want = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
    $got = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($want -ne $got) { throw 'checksum mismatch - refusing to install' }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    $exe = Join-Path $tmp 'sinter.exe'
    if (-not (Test-Path $exe)) { throw "archive did not contain a 'sinter.exe' binary" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $dest = Join-Path $InstallDir 'sinter.exe'
    Copy-Item $exe $dest -Force

    $version = & $dest --version
    Write-Host "installed $version -> $dest"
    if (-not (($env:Path -split ';') -contains $InstallDir)) {
        Write-Host "note: $InstallDir is not on your PATH - add it for the current user:"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$InstallDir;`" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')"
        Write-Host 'then open a new terminal.'
    }
    Write-Host 'next: cd your-repo; sinter init'
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
