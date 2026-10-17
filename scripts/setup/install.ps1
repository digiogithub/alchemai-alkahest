<#
.SYNOPSIS
Alkahest installer for Windows (PowerShell 5.1 or 7+).

.DESCRIPTION
Detects architecture and GPU, installs alkahest.exe from a release archive whose SHA-256 is
verified against the release checksum file (or from a local build), adds it to the user PATH
and installs the recommended llama.cpp engine build. Nothing downloaded is executed before its
checksum matches. Use -DryRun to see every step without changing anything.

The release URLs are placeholders until the release pipeline (ALK-US-0074) publishes
archives; use -FromLocal or -BaseUrl until then.

.PARAMETER DryRun
Print what would happen; change nothing, download nothing (same as --dry-run in install.sh).

.PARAMETER Variant
Engine accelerator build (cpu, cuda12.4, vulkan, rocm, ...); the best one when omitted.

.PARAMETER Prefix
Install alkahest.exe into <Prefix>\bin (default: $env:LOCALAPPDATA\Programs\Alkahest).

.PARAMETER NoEngine
Do not run `alkahest engines install llama-cpp`.

.PARAMETER Service
Also run `alkahest service install --user`.

.PARAMETER Version
Release to install (default: latest).

.PARAMETER BaseUrl
Release base URL (default: placeholder).

.PARAMETER FromLocal
Use an existing alkahest.exe instead of downloading.
#>
[CmdletBinding()]
param(
    [switch]$DryRun,
    [string]$Variant = '',
    [string]$Prefix = '',
    [switch]$NoEngine,
    [switch]$Service,
    [string]$Version = $(if ($env:ALKAHEST_VERSION) { $env:ALKAHEST_VERSION } else { 'latest' }),
    [string]$BaseUrl = $(if ($env:ALKAHEST_RELEASE_BASE_URL) { $env:ALKAHEST_RELEASE_BASE_URL } else { 'https://github.com/digio-es/alkahest/releases/download' }),
    [string]$FromLocal = ''
)

$ErrorActionPreference = 'Stop'

function Write-Info([string]$Message) { Write-Host "==> $Message" }

# Invoke-Step prints the command in dry-run mode and runs it otherwise.
function Invoke-Step([string]$Exe, [string[]]$Arguments) {
    if ($DryRun) {
        Write-Host "[dry-run] $Exe $($Arguments -join ' ')"
    } else {
        & $Exe @Arguments
        if ($LASTEXITCODE -ne 0) { throw "$Exe exited with code $LASTEXITCODE" }
    }
}

# Get-GpuDescription names the GPU family without failing when no tool is present.
function Get-GpuDescription {
    if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) {
        $listing = & nvidia-smi -L 2>$null
        if ($listing -match '^GPU') { return 'NVIDIA (CUDA)' }
    }
    try {
        $names = (Get-CimInstance Win32_VideoController -ErrorAction Stop).Name -join ' '
        if ($names -match 'AMD|Radeon') { return 'AMD (ROCm/Vulkan)' }
        if ($names -match 'Intel') { return 'Intel (Vulkan)' }
        if ($names -match 'NVIDIA') { return 'NVIDIA (CUDA)' }
    } catch { }
    return 'none detected (CPU)'
}

$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { $target = 'aarch64-pc-windows-msvc' }
    default { throw "unsupported architecture $arch" }
}
Write-Info "platform: windows/$arch, GPU: $(Get-GpuDescription)"

if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA 'Programs\Alkahest' }
$binDir = Join-Path $Prefix 'bin'
$dest = Join-Path $binDir 'alkahest.exe'

$source = $null
$tmp = $null
try {
    if ($FromLocal) {
        if (-not (Test-Path -LiteralPath $FromLocal -PathType Leaf)) {
            throw "-FromLocal: $FromLocal is not a file"
        }
        $source = $FromLocal
        Write-Info "using local binary $source"
    } else {
        $archive = "alkahest-$Version-$target.zip"
        if ($Version -eq 'latest') {
            $root = $BaseUrl -replace '/download$', ''
            $urlDir = "$root/latest/download"
        } else {
            $urlDir = "$BaseUrl/v$Version"
        }
        Write-Info "release archive: $urlDir/$archive"
        Write-Info "checksum file:   $urlDir/SHA256SUMS"
        if ($DryRun) {
            Write-Host '[dry-run] download archive and SHA256SUMS, verify the SHA-256, extract'
        } else {
            $tmp = Join-Path ([IO.Path]::GetTempPath()) ("alkahest-" + [Guid]::NewGuid())
            New-Item -ItemType Directory -Path $tmp | Out-Null
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$urlDir/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')
                Invoke-WebRequest -UseBasicParsing -Uri "$urlDir/$archive" -OutFile (Join-Path $tmp $archive)
            } catch {
                throw "download failed (release URLs are placeholders until ALK-US-0074; try -FromLocal): $_"
            }
            $line = Get-Content (Join-Path $tmp 'SHA256SUMS') |
                Where-Object { ($_ -split '\s+', 2)[1] -replace '^\*', '' -eq $archive } |
                Select-Object -First 1
            if (-not $line) { throw "$archive is not listed in SHA256SUMS" }
            $want = ($line -split '\s+')[0].ToLowerInvariant()
            $got = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $tmp $archive)).Hash.ToLowerInvariant()
            if ($want -ne $got) { throw "checksum mismatch for $archive (expected $want, got $got)" }
            Write-Info 'checksum verified'
            Expand-Archive -LiteralPath (Join-Path $tmp $archive) -DestinationPath (Join-Path $tmp 'x')
            $found = Get-ChildItem -Path (Join-Path $tmp 'x') -Recurse -Filter 'alkahest.exe' | Select-Object -First 1
            if (-not $found) { throw 'archive does not contain alkahest.exe' }
            $source = $found.FullName
        }
    }

    # Install, idempotently: skip the copy when the file is identical.
    $same = $false
    if (-not $DryRun -and $source -and (Test-Path -LiteralPath $dest)) {
        $same = (Get-FileHash -LiteralPath $source).Hash -eq (Get-FileHash -LiteralPath $dest).Hash
    }
    if ($same) {
        Write-Info "$dest is already up to date"
    } else {
        Write-Info "installing alkahest into $binDir"
        if ($DryRun) {
            Write-Host "[dry-run] copy alkahest.exe to $dest"
        } else {
            New-Item -ItemType Directory -Path $binDir -Force | Out-Null
            Copy-Item -LiteralPath $source -Destination $dest -Force
        }
    }
} finally {
    if ($tmp -and (Test-Path -LiteralPath $tmp)) { Remove-Item -Recurse -Force -LiteralPath $tmp }
}

# Add the bin directory to the user PATH once.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = if ($userPath) { $userPath -split ';' } else { @() }
if ($entries -notcontains $binDir) {
    Write-Info "adding $binDir to the user PATH"
    if ($DryRun) {
        Write-Host "[dry-run] set user PATH to include $binDir"
    } else {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $binDir) -join ';'), 'User')
        Write-Info 'open a new terminal for the PATH change to take effect'
    }
}

$alk = if ($DryRun) { 'alkahest' } else { $dest }
if (-not $NoEngine) {
    Write-Info 'installing the llama.cpp engine'
    $engineArgs = @('engines', 'install', 'llama-cpp')
    if ($Variant) { $engineArgs += @('--variant', $Variant) }
    Invoke-Step $alk $engineArgs
} else {
    Write-Info 'skipping engine install (-NoEngine)'
}

if ($Service) {
    Write-Info 'installing the per-user service'
    Invoke-Step $alk @('service', 'install', '--user')
}

Write-Info 'done. Try: alkahest pull <model> ; alkahest run <model>'
