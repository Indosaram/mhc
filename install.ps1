# Install mhc on Windows from a GitHub Release.
#
#   irm https://raw.githubusercontent.com/Indosaram/mhc/main/install.ps1 | iex
#
# Layout (install root R = MHC_INSTALL_DIR):
#   R\mhc.exe            stable launcher; PATH points here and updates never replace it
#   R\current.json       {"schemaVersion":1,"current":"<v>","previous":"<v>"|null}
#   R\versions\<v>\      one complete release package (payload mhc.exe, siblings, skills, manifest.json)
# A flat install from an older release (R\mhc.exe + R\manifest.json, no versions\) is migrated into
# R\versions\<old>\ and kept as `previous`.
#
# Environment:
#   MHC_VERSION      release tag to install (default: latest), e.g. v2026.10.19
#   MHC_INSTALL_DIR  install root (default: %LOCALAPPDATA%\mhc)
#   MHC_PACKAGE_DIR  install an already extracted release package directory instead of downloading one
#   MHC_NO_PATH_UPDATE=1  leave the user PATH untouched
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'Indosaram/mhc'
$version = if ($env:MHC_VERSION) { $env:MHC_VERSION } else { 'latest' }
$installDir = if ($env:MHC_INSTALL_DIR) { $env:MHC_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'mhc' }
$localPackage = $env:MHC_PACKAGE_DIR
$updatePath = $env:MHC_NO_PATH_UPDATE -ne '1'

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($arch -ne 'X64') { throw "mhc install: no prebuilt mhc for Windows $arch; build from source: https://github.com/$repo#build-from-source" }
$target = 'windows-x86_64'
$asset = "mhc-$target.zip"
$base = if ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }

function Get-PayloadVersion([string]$exe) {
    if (-not (Test-Path -LiteralPath $exe)) { return $null }
    # Windows PowerShell 5.1: `2>$null` under $ErrorActionPreference 'Stop' makes the first native
    # stderr line a terminating error, and piping into Select-Object -First 1 stops the process
    # before $LASTEXITCODE is set. $null -ne 0, so a successful `--version` comes back empty.
    $ErrorActionPreference = 'Continue'
    try {
        $output = & $exe --version 2>$null
        $code = $LASTEXITCODE
    } catch { return $null }
    if ($code -ne 0 -or -not $output) { return $null }
    $line = @($output) | Select-Object -First 1
    if (-not $line) { return $null }
    return ([string]$line).Trim() -replace '^[vV]', ''
}

function Test-VersionName([string]$name) {
    return ($name -match '^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$')
}

function Read-Pointer([string]$root) {
    $path = Join-Path $root 'current.json'
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    $pointer = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    if ($pointer.schemaVersion -ne 1 -or -not (Test-VersionName ([string]$pointer.current))) { throw "mhc install: unsupported $path" }
    return $pointer
}

function Write-Pointer([string]$root, [string]$current, $previous) {
    $json = if ($previous) { "{`"schemaVersion`":1,`"current`":`"$current`",`"previous`":`"$previous`"}" } else { "{`"schemaVersion`":1,`"current`":`"$current`",`"previous`":null}" }
    $temporary = Join-Path $root ".current.json.$PID"
    [System.IO.File]::WriteAllText($temporary, "$json`n", (New-Object System.Text.UTF8Encoding($false)))
    $destination = Join-Path $root 'current.json'
    if (Test-Path -LiteralPath $destination) {
        [System.IO.File]::Replace($temporary, $destination, $null)
    } else {
        [System.IO.File]::Move($temporary, $destination)
    }
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$installDir = (Resolve-Path -LiteralPath $installDir).Path
$work = Join-Path $installDir (".install." + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Force -Path $work | Out-Null
$updateLock = $null
$hadPointer = $false
try {
    try {
        $updateLock = [System.IO.File]::Open((Join-Path $installDir '.update.lock'), [System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
    } catch {
        throw 'mhc install: another mhc update or install is running'
    }

    $staged = Join-Path $work "mhc-$target"
    if ($localPackage) {
        if (-not (Test-Path -LiteralPath $localPackage -PathType Container)) { throw "mhc install: MHC_PACKAGE_DIR is not a directory: $localPackage" }
        Copy-Item -LiteralPath $localPackage -Destination $staged -Recurse
    } else {
        Write-Host "Downloading $asset ($version)..."
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $work $asset)
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile (Join-Path $work "$asset.sha256")
        $expected = ((Get-Content (Join-Path $work "$asset.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $work $asset)).Hash.ToLower()
        if ($expected -ne $actual) { throw "mhc install: checksum mismatch for $asset (expected $expected, got $actual)" }
        Expand-Archive -Path (Join-Path $work $asset) -DestinationPath $work -Force
    }
    foreach ($name in @('mhc.exe', 'mhc-launcher.exe', 'manifest.json')) {
        if (-not (Test-Path -LiteralPath (Join-Path $staged $name))) { throw "mhc install: archive does not contain mhc-$target\$name" }
    }

    $new = Get-PayloadVersion (Join-Path $staged 'mhc.exe')
    if (-not $new -or -not (Test-VersionName $new)) { throw "mhc install: cannot determine the packaged mhc version (got '$new')" }

    $oldPointer = Read-Pointer $installDir
    if ($oldPointer) {
        $hadPointer = $true
    }

    New-Item -ItemType Directory -Force -Path (Join-Path $installDir 'versions') | Out-Null
    $destination = Join-Path $installDir "versions\$new"
    $reuse = (Test-Path -LiteralPath (Join-Path $destination 'mhc.exe')) -and
        (Test-Path -LiteralPath (Join-Path $destination 'manifest.json')) -and
        ((Get-FileHash -Algorithm SHA256 (Join-Path $destination 'manifest.json')).Hash -eq (Get-FileHash -Algorithm SHA256 (Join-Path $staged 'manifest.json')).Hash)
    if ($reuse) {
        Write-Host "mhc $new is already installed; reusing it."
    } else {
        if (Test-Path -LiteralPath $destination) { throw "mhc install: version $new already exists with different content; published packages are immutable" }
        Move-Item -LiteralPath $staged -Destination $destination
    }

    $previous = $null
    if ($hadPointer) {
        $previous = if ($oldPointer.current -ne $new) { [string]$oldPointer.current } elseif ($oldPointer.previous) { [string]$oldPointer.previous } else { $null }
    } elseif (Test-Path -LiteralPath (Join-Path $installDir 'mhc.exe')) {
        $flat = Get-PayloadVersion (Join-Path $installDir 'mhc.exe')
        $flatEntries = @(Get-ChildItem -Force -LiteralPath $installDir | Where-Object { -not $_.Name.StartsWith('.') -and $_.Name -ne 'versions' -and $_.Name -ne 'current.json' })
        if ($flat -and (Test-VersionName $flat) -and $flat -ne $new -and -not (Test-Path -LiteralPath (Join-Path $installDir "versions\$flat"))) {
            Write-Host "Migrating the existing flat install ($flat) into versions\$flat..."
            $flatDir = Join-Path $installDir "versions\$flat"
            New-Item -ItemType Directory -Force -Path $flatDir | Out-Null
            foreach ($entry in $flatEntries) { Move-Item -LiteralPath $entry.FullName -Destination (Join-Path $flatDir $entry.Name) }
            $previous = $flat
        } else {
            throw "mhc install: cannot safely migrate the existing flat installation; use a separate MHC_INSTALL_DIR"
        }
    }
    if ($previous -eq $new) { $previous = $null }

    $launcherSource = Join-Path $destination 'mhc-launcher.exe'
    $launcher = Join-Path $installDir 'mhc.exe'
    if (-not $hadPointer -or -not (Test-Path -LiteralPath $launcher)) {
        $launcherNew = Join-Path $installDir '.mhc.new.exe'
        Copy-Item -LiteralPath $launcherSource -Destination $launcherNew -Force
        try {
            Move-Item -LiteralPath $launcherNew -Destination $launcher -Force
        } catch {
            $pendingPath = Join-Path $installDir 'mhc.new.exe'
            Move-Item -LiteralPath $launcherNew -Destination $pendingPath -Force
            Write-Host "The running launcher $launcher is locked; the new launcher was written to $pendingPath. Close all mhc sessions and rename it over mhc.exe."
        }
    }

    Write-Pointer $installDir $new $previous

    $installed = Get-PayloadVersion $launcher
    if ($installed -ne $new) {
        if ($hadPointer) { Write-Pointer $installDir ([string]$oldPointer.current) $oldPointer.previous }
        throw "mhc install: installed launcher reports '$installed', expected '$new'"
    }
} finally {
    if ($updateLock) { $updateLock.Dispose() }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($updatePath -and -not (($userPath -split ';') -contains $installDir)) {
    [Environment]::SetEnvironmentVariable('Path', ($(if ($userPath) { "$userPath;" } else { '' }) + $installDir), 'User')
    Write-Host "Added $installDir to your user PATH (open a new terminal to pick it up)."
}
$env:Path = "$installDir;$env:Path"
Write-Host "Installed mhc $installed to $installDir"
