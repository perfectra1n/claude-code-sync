# claude-code-sync installer for Windows.
#
#   irm https://raw.githubusercontent.com/perfectra1n/claude-code-sync/main/install.ps1 | iex
#
# `irm | iex` cannot take arguments, so options are environment variables:
#
#   $env:CCS_VERSION      Release to install (default: latest)
#   $env:CCS_INSTALL_DIR  Install directory (default: %LOCALAPPDATA%\Programs\claude-code-sync)
#   $env:CCS_NO_PATH      Set to 1 to skip adding the install directory to the user PATH
#
# When run as a file, the same options are parameters: .\install.ps1 -Version v0.3.3 -InstallDir C:\tools
# Re-running the installer upgrades an existing install in place.

function Install-ClaudeCodeSync {
    [CmdletBinding()]
    param(
        [string]$Version = $(if ($env:CCS_VERSION) { $env:CCS_VERSION } else { 'latest' }),
        [string]$InstallDir = $(if ($env:CCS_INSTALL_DIR) { $env:CCS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\claude-code-sync' }),
        [switch]$NoPath = $($env:CCS_NO_PATH -eq '1')
    )

    $ErrorActionPreference = 'Stop'
    # Invoke-WebRequest's progress bar slows downloads by an order of magnitude on Windows PowerShell 5.1.
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 defaults to TLS 1.0, which GitHub rejects.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'perfectra1n/claude-code-sync'
    $bin = 'claude-code-sync.exe'

    # OSArchitecture reports the real hardware even from an emulated x64 PowerShell on ARM64.
    try {
        $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    } catch {
        $osArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    }
    $arches = switch -Regex ($osArch) {
        '^(X64|AMD64)$' { @('x86_64') }
        # Windows on ARM runs x64 binaries under emulation, so fall back to x86_64
        # for releases that predate the native ARM64 build.
        '^(Arm64|ARM64)$' { @('aarch64', 'x86_64') }
        default { throw "Unsupported architecture: $osArch (prebuilt: x86_64, aarch64). Try: cargo install claude-code-sync" }
    }

    if ($Version -eq 'latest') {
        $base = "https://github.com/$repo/releases/latest/download"
    } else {
        if (-not $Version.StartsWith('v')) { $Version = "v$Version" }
        $base = "https://github.com/$repo/releases/download/$Version"
    }

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ccs-install-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $asset = $null
        foreach ($arch in $arches) {
            $candidate = "claude-code-sync-windows-$arch.exe.zip"
            Write-Host "Downloading $candidate ($Version)..."
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$base/$candidate" -OutFile (Join-Path $tmp $candidate)
                $asset = $candidate
                if ($arch -ne $arches[0]) { Write-Warning "No native $($arches[0]) build in $Version; installed the $arch build (runs under emulation)." }
                break
            } catch {
                if ($arch -eq $arches[-1]) { throw "Download failed: $base/$candidate ($($_.Exception.Message))" }
            }
        }

        $zip = Join-Path $tmp $asset
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile "$zip.sha256"
        $expected = ((Get-Content "$zip.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($expected -ne $actual) { throw "Checksum mismatch for $asset (expected $expected, got $actual)" }

        Expand-Archive -Path $zip -DestinationPath $tmp -Force
        $src = Join-Path $tmp $bin
        if (-not (Test-Path $src)) { throw "Archive did not contain $bin" }

        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        $dest = Join-Path $InstallDir $bin
        # A running .exe cannot be overwritten but can be renamed, so move any
        # existing copy aside first; the stale file is cleaned up on the next run.
        $old = "$dest.old"
        if (Test-Path $old) { Remove-Item -Force $old -ErrorAction SilentlyContinue }
        if (Test-Path $dest) { Move-Item -Force $dest $old }
        Move-Item -Force $src $dest

        Write-Host "Installed $(& $dest --version) to $dest"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $onPath = ($userPath -split ';') -contains $InstallDir
    if (-not $onPath -and -not $NoPath) {
        $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        $env:Path = "$env:Path;$InstallDir"
        Write-Host "Added $InstallDir to your user PATH (open a new terminal to pick it up)."
    } elseif (-not $onPath) {
        Write-Host "Note: $InstallDir is not on your PATH."
    }
}

Install-ClaudeCodeSync @args
