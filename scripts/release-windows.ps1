#Requires -Version 5.1
<#
.SYNOPSIS
  Build the Windows release packages locally, like the CI windows-x86_64 job.

.DESCRIPTION
  Usage: scripts/release-windows.ps1 -Version <version> -OutputDir <dir>
    -Version     MHC_BUILD_VERSION, e.g. 2026.11.12.1
    -OutputDir   receives mhc-windows-x86_64.zip and mhc-windows-x86_64-mit.zip (+ .sha256)
    -ProtocPath  protoc.exe; required because an SSH session does not inherit it
    -OmowrightCommit  the pinned omowright SHA; computed by release-all.sh, which
                  reads the manifest on a host where bun -e works reliably

  Needs: cargo, bun, node, git, protoc. Honors CARGO_TARGET_DIR. Never uploads
  anything. Set -SkipMit to build only the default profile.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [string]$ProtocPath = $env:PROTOC,
    [string]$OmowrightCommit = "",
    [switch]$SkipMit
)

# Native exit codes are invisible to $ErrorActionPreference, and cargo writes
# ordinary progress to stderr, so Stop would abort on the first progress line.
$ErrorActionPreference = "Continue"
$Version = $Version -replace '^v', ''
$exe = ".exe"

function Step($name, $block) {
    Write-Log "=== $name"
    & $block
    if ($LASTEXITCODE -ne 0) { throw "$name failed with exit code $LASTEXITCODE" }
}

function Assert-Version($binary) {
    $got = (& $binary --version).Trim()
    if ($got -ne $Version) {
        throw "version mismatch: $binary reports '$got', expected '$Version' (stale CARGO_TARGET_DIR?)"
    }
    Write-Log "    version ok: $got"
}

# PowerShell 5.1 mangles the quotes in `bun -e '...'`, so the commit came back
# empty and git fetch silently fell back to the default branch, checking out a
# tree with no src/index.js. Read the manifest with node from a script file.
function Get-OmowrightCommit($repoRoot) {
    $probe = Join-Path ([System.IO.Path]::GetTempPath()) ("mhc-commit-" + [guid]::NewGuid().ToString("N").Substring(0, 8) + ".mjs")
    $manifestPath = Join-Path $repoRoot "assets/omo-latest/manifest.json"
    if (-not (Test-Path $manifestPath)) {
        throw "manifest not found at $manifestPath (is this script running from inside the repo?)"
    }
    $body = @'
import { readFile } from "node:fs/promises";
const manifest = JSON.parse(await readFile(process.argv[2], "utf8"));
console.log(manifest.browserRuntime.dependency.commit);
'@
    Set-Content -Path $probe -Value $body -Encoding utf8
    try {
        $commit = (& node $probe $manifestPath 2>$null | Out-String).Trim()
    }
    finally {
        Remove-Item $probe -Force -ErrorAction SilentlyContinue
    }
    if ($commit -notmatch '^[0-9a-f]{40}$') {
        throw "could not read browserRuntime.dependency.commit from assets/omo-latest/manifest.json (got '$commit')"
    }
    return $commit
}

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# Write-Host does not survive `cmd 1>` redirection, so progress goes to a file;
# without it a hung step is indistinguishable from a slow one.
$logPath = Join-Path $repo "release-windows.log"
function Write-Log($message) {
    $line = "[$(Get-Date -Format HH:mm:ss)] $message"
    Write-Host $line
    Add-Content -Path $logPath -Value $line -Encoding utf8
}
Set-Content -Path $logPath -Value "release-windows.ps1 v$Version" -Encoding utf8
if (-not (Test-Path $OutputDir)) { New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null }
$OutputDir = (Resolve-Path $OutputDir).Path

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" }
$env:CARGO_TARGET_DIR = $targetDir
$env:MHC_BUILD_VERSION = $Version
$rel = Join-Path $targetDir "release"

if (-not $ProtocPath) {
    $found = Get-Command protoc -ErrorAction SilentlyContinue
    if ($found) { $ProtocPath = $found.Source }
}
if (-not $ProtocPath -or -not (Test-Path $ProtocPath)) {
    throw "protoc not found. Pass -ProtocPath <path to protoc.exe> or set PROTOC."
}
$env:PROTOC = $ProtocPath
$env:PATH = "$env:PATH;" + (Split-Path -Parent $ProtocPath)
Write-Log "protoc: $ProtocPath"

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("mhc-rel-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $work -Force | Out-Null
try {
    $commit = if ($OmowrightCommit) { $OmowrightCommit } else { Get-OmowrightCommit $repo }
    if ($commit -notmatch '^[0-9a-f]{40}$') { throw "invalid omowright commit '$commit'" }
    Write-Log "omowright: $commit"

    Step "materialize omowright runtime" {
        git init -q "$work/omowright"
        git -C "$work/omowright" fetch -q --depth 1 https://github.com/code-yeongyu/omowright.git $commit
        git -C "$work/omowright" checkout -q FETCH_HEAD
        Push-Location "$work/omowright"
        bun install --frozen-lockfile --ignore-scripts --production 2>&1 | Out-Null
        Pop-Location
        node assets/omo-latest/stage-omowright-runtime.mjs --source "$work/omowright" --target "$work/omowright-runtime"
    }

    Step "SUL build" {
        cargo build --release --locked -p maho-cli -p maho-ast-grep-mcp -p maho-lsp-daemon -p maho-git-bash-mcp -p maho-senpi-desktop-engine --bins
    }
    Assert-Version "$rel/mhc$exe"

    $sulStage = Join-Path $work "dist/mhc-windows-x86_64"
    Step "SUL package" {
        New-Item -ItemType Directory -Path $sulStage -Force | Out-Null
        bun tools/package-native.mjs --binary "$rel/mhc$exe" --launcher-binary "$rel/mhc-launcher$exe" --output $sulStage --skill-source latest --git-bash-binary "$rel/omo-git-bash$exe" --desktop-engine-binary "$rel/senpi-desktop-engine$exe" --omowright-runtime "$work/omowright-runtime"
        bun tools/package-native.mjs --verify-only $sulStage
    }
    Compress-Archive -Path "$sulStage/*" -DestinationPath (Join-Path $OutputDir "mhc-windows-x86_64.zip") -Force
    Write-Log "    wrote mhc-windows-x86_64.zip"

    if (-not $SkipMit) {
        Step "MIT build" {
            cargo build --release --locked --no-default-features -p maho-cli --bins
        }
        Assert-Version "$rel/mhc$exe"
        $mitStage = Join-Path $work "dist/mhc-windows-x86_64-mit"
        Step "MIT package" {
            New-Item -ItemType Directory -Path $mitStage -Force | Out-Null
            bun tools/package-native.mjs --mit --binary "$rel/mhc$exe" --launcher-binary "$rel/mhc-launcher$exe" --output $mitStage
            bun tools/package-native.mjs --verify-only $mitStage
        }
        Compress-Archive -Path "$mitStage/*" -DestinationPath (Join-Path $OutputDir "mhc-windows-x86_64-mit.zip") -Force
        Write-Log "    wrote mhc-windows-x86_64-mit.zip"
    }
}
finally {
    if (Test-Path $work) { Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue }
}

Get-ChildItem $OutputDir -Filter "mhc-windows-*.zip" | ForEach-Object {
    $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
    "$hash  $($_.Name)" | Out-File -Encoding ascii "$($_.FullName).sha256"
    Write-Log "    $($_.Name) $($_.Length) $hash"
}
Write-Log "wrote release assets for $Version to $OutputDir"
