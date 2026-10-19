# Install mhc on Windows from a GitHub Release.
#
#   irm https://raw.githubusercontent.com/Indosaram/mhc/main/install.ps1 | iex
#
# Environment:
#   MHC_VERSION      release tag to install (default: latest), e.g. v2026.10.19
#   MHC_INSTALL_DIR  where the release directory is unpacked (default: %LOCALAPPDATA%\mhc)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'Indosaram/mhc'
$version = if ($env:MHC_VERSION) { $env:MHC_VERSION } else { 'latest' }
$installDir = if ($env:MHC_INSTALL_DIR) { $env:MHC_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'mhc' }

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($arch -ne 'X64') { throw "mhc install: no prebuilt mhc for Windows $arch; build from source: https://github.com/$repo#build-from-source" }
$target = 'windows-x86_64'
$asset = "mhc-$target.zip"
$base = if ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$work = Join-Path $installDir (".install." + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Force -Path $work | Out-Null
try {
    Write-Host "Downloading $asset ($version)..."
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $work $asset)
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile (Join-Path $work "$asset.sha256")
    $expected = ((Get-Content (Join-Path $work "$asset.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $work $asset)).Hash.ToLower()
    if ($expected -ne $actual) { throw "mhc install: checksum mismatch for $asset (expected $expected, got $actual)" }

    Expand-Archive -Path (Join-Path $work $asset) -DestinationPath $work -Force
    $staged = Join-Path $work "mhc-$target"
    if (-not (Test-Path (Join-Path $staged 'mhc.exe'))) { throw "mhc install: archive does not contain mhc-$target\mhc.exe" }

    Get-ChildItem -Force $staged | ForEach-Object {
        $destination = Join-Path $installDir $_.Name
        if (Test-Path $destination) { Remove-Item -Recurse -Force $destination }
        Move-Item $_.FullName $destination
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $installDir)) {
    [Environment]::SetEnvironmentVariable('Path', ($(if ($userPath) { "$userPath;" } else { '' }) + $installDir), 'User')
    Write-Host "Added $installDir to your user PATH (open a new terminal to pick it up)."
}
$env:Path = "$installDir;$env:Path"
Write-Host "Installed mhc $(& (Join-Path $installDir 'mhc.exe') --version) to $installDir"
