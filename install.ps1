# Tensor installer for Windows.
# Usage: irm https://github.com/jacobzymet/tensorUI/releases/latest/download/install.ps1 | iex
& {
    $ErrorActionPreference = 'Stop'

    $Repo = 'jacobzymet/tensorUI'
    $GitHub = "https://github.com/$Repo"
    $BinName = 'tensor.exe'

    function Write-Info([string]$Message) {
        Write-Host $Message
    }

    function Write-InstallError([string]$Message) {
        Write-Host "error: $Message" -ForegroundColor Red
        exit 1
    }

    function Invoke-Curl {
        param(
            [string[]]$CurlArgs,
            [switch]$AllowFail,
            [switch]$Quiet
        )
        if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue)) {
            Write-InstallError 'missing required command: curl.exe'
        }
        $all = @('-H', 'User-Agent: tensor-install')
        if ($env:GITHUB_TOKEN) {
            $all += @('-H', "Authorization: Bearer $($env:GITHUB_TOKEN)")
        }
        $all += $CurlArgs
        $previousError = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            if ($Quiet) {
                & curl.exe @all 1>$null 2>$null
            } else {
                & curl.exe @all
            }
            $code = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousError
        }
        if (-not $AllowFail -and $code -ne 0) {
            Write-InstallError "download failed (curl exit $code)"
        }
        return $code
    }

    function Get-ArgValue([string[]]$Names) {
        for ($i = 0; $i -lt $script:InstallerArgs.Count; $i++) {
            $current = $script:InstallerArgs[$i]
            foreach ($name in $Names) {
                if ($current -eq $name) {
                    if ($i + 1 -ge $script:InstallerArgs.Count) {
                        Write-InstallError "$name requires a value"
                    }
                    return $script:InstallerArgs[$i + 1]
                }
            }
        }
        return $null
    }

    function Test-Flag([string[]]$Names) {
        foreach ($arg in $script:InstallerArgs) {
            foreach ($name in $Names) {
                if ($arg -eq $name) { return $true }
            }
        }
        return $false
    }

    function Show-Help {
        @"
Install Tensor from GitHub Releases (Windows).

Usage:
  install.ps1 [options]
  irm https://github.com/jacobzymet/tensorUI/releases/latest/download/install.ps1 | iex

Options:
  --version, -v <ver>  Release to install (default: latest)
  --dir, -d <path>     Install directory (default: %LOCALAPPDATA%\tensor\bin)
  --no-path            Do not add the install directory to your user PATH
  -h, --help           Show this help

Environment:
  TENSOR_VERSION       Same as --version
  TENSOR_INSTALL_DIR   Same as --dir
  TENSOR_NO_PATH       Set to 1 to skip PATH changes
  GITHUB_TOKEN         Optional token if GitHub rate-limits you
"@ | Write-Host
    }

    $script:InstallerArgs = @($args)
    if (Test-Flag @('--help', '-h')) {
        Show-Help
        return
    }

    if ($PSVersionTable.PSVersion.Major -ge 6 -and -not $IsWindows) {
        Write-InstallError 'this installer is for Windows. On Unix, use install-linux.sh or install-macos.sh.'
    }

    if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue)) {
        Write-InstallError 'missing required command: curl.exe'
    }

    $version = Get-ArgValue @('--version', '-v')
    if (-not $version) { $version = $env:TENSOR_VERSION }

    $installDir = Get-ArgValue @('--dir', '-d')
    if (-not $installDir) { $installDir = $env:TENSOR_INSTALL_DIR }
    if (-not $installDir) { $installDir = Join-Path $env:LOCALAPPDATA 'tensor\bin' }

    $noPath = (Test-Flag @('--no-path')) -or ($env:TENSOR_NO_PATH -eq '1')

    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -eq 'ARM64') {
        Write-Info 'No native Windows ARM64 build is published; installing the x64 binary.'
        $target = 'x86_64-pc-windows-msvc'
    } elseif ($arch -eq 'AMD64') {
        $target = 'x86_64-pc-windows-msvc'
    } else {
        Write-InstallError "unsupported architecture: $arch. Releases cover Windows x64."
    }

    if (-not $version) {
        Write-Info 'Looking up the latest Tensor release...'
        $lookupArgs = @('-H', 'User-Agent: tensor-install')
        if ($env:GITHUB_TOKEN) {
            $lookupArgs += @('-H', "Authorization: Bearer $($env:GITHUB_TOKEN)")
        }
        $lookupArgs += @('-fsSLI', '--connect-timeout', '20', '--max-time', '30', '-o', 'NUL', '-w', '%{url_effective}', "$GitHub/releases/latest")
        $previousError = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $final = & curl.exe @lookupArgs
            $lookupCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousError
        }
        if ($lookupCode -ne 0 -or [string]::IsNullOrWhiteSpace($final)) {
            Write-InstallError 'could not resolve the latest release'
        }
        $tag = ($final.Trim() -split '/')[-1]
    } else {
        $tag = $version
    }

    if ($tag -notlike 'v*') { $tag = "v$tag" }
    $version = $tag.TrimStart('v')
    if (-not $version) { Write-InstallError 'could not determine a release version' }
    Write-Info "Installing Tensor $version ($target)..."

    $workDir = Join-Path ([IO.Path]::GetTempPath()) ("tensor-install-" + [guid]::NewGuid().ToString('n'))
    New-Item -ItemType Directory -Path $workDir | Out-Null
    try {
        $archive = Join-Path $workDir 'tensor.zip'
        $base = "$GitHub/releases/download/$tag"
        $asset = $null
        $url = $null
        foreach ($prefix in @('tensor', 'tensorui')) {
            $candidate = "$prefix-$version-$target.zip"
            $candidateUrl = "$base/$candidate"
            $probe = Invoke-Curl -AllowFail -Quiet -CurlArgs @('-fsSLI', '--connect-timeout', '20', '--max-time', '30', '-o', 'NUL', $candidateUrl)
            if ($probe -eq 0) {
                $asset = $candidate
                $url = $candidateUrl
                break
            }
        }
        if (-not $asset) {
            Write-InstallError "no Windows archive found for $tag ($target)"
        }
        Write-Info "Downloading $asset..."
        [void](Invoke-Curl -CurlArgs @('-fL', '--connect-timeout', '20', '--retry', '3', '--retry-delay', '1', '--max-time', '600', '-o', $archive, $url))

        $sumsPath = Join-Path $workDir 'SHA256SUMS'
        $sumsStatus = Invoke-Curl -AllowFail -Quiet -CurlArgs @('-fsSL', '--connect-timeout', '20', '--max-time', '30', '-o', $sumsPath, "$base/SHA256SUMS")
        if ($sumsStatus -eq 0) {
            $expected = $null
            foreach ($line in Get-Content -Path $sumsPath) {
                if ($line -match '^([A-Fa-f0-9]{64})\s+\*?(\S+)$' -and $Matches[2].TrimStart('./') -eq $asset) {
                    $expected = $Matches[1].ToLowerInvariant()
                    break
                }
            }
            if (-not $expected) {
                Write-InstallError "SHA256SUMS does not list $asset"
            }
            $actual = (Get-FileHash -Path $archive -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actual -ne $expected) {
                Write-InstallError "checksum mismatch for $asset"
            }
            Write-Info 'Checksum verified.'
        }

        $extract = Join-Path $workDir 'extract'
        Expand-Archive -Path $archive -DestinationPath $extract -Force
        $src = Get-ChildItem -Path $extract -Recurse -File | Where-Object {
            $_.Name -eq 'tensor.exe' -or $_.Name -eq 'tensorui.exe'
        } | Sort-Object { if ($_.Name -eq 'tensor.exe') { 0 } else { 1 } } | Select-Object -First 1
        if (-not $src) {
            Write-InstallError 'archive did not contain a tensor executable'
        }

        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        $dest = Join-Path $installDir $BinName
        try {
            Copy-Item -Path $src.FullName -Destination $dest -Force
        } catch {
            Write-InstallError "could not write $dest. Quit Tensor if it is running, then retry."
        }

        Write-Info "Installed tensor $version to $dest"

        try {
            & $dest --version
        } catch {
            # Version flag should exist; ignore if a pre-rename binary differs.
        }

        $webviewKeys = @(
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
            'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
            'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
        )
        $hasWebView2 = $false
        foreach ($key in $webviewKeys) {
            if (Test-Path $key) { $hasWebView2 = $true; break }
        }
        if (-not $hasWebView2) {
            Write-Info ''
            Write-Info 'Microsoft Edge WebView2 was not found. Install it for the desktop window, or run: tensor --browser'
            Write-Info '  https://developer.microsoft.com/microsoft-edge/webview2/'
        }

        if (-not $noPath) {
            $normalized = $installDir.TrimEnd('\')
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            if ($null -eq $userPath) { $userPath = '' }
            $already = $false
            foreach ($part in ($userPath -split ';')) {
                if ($part.TrimEnd('\') -eq $normalized) { $already = $true; break }
            }
            if (-not $already) {
                if ([string]::IsNullOrWhiteSpace($userPath)) {
                    $newPath = $normalized
                } else {
                    $newPath = "$userPath;$normalized"
                }
                [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
                Write-Info "Added $normalized to your user PATH."
            }
            if ($env:Path -notlike "*$normalized*") {
                $env:Path = "$normalized;$env:Path"
            }
        }

        Write-Info ''
        Write-Info 'Launch Tensor with:  tensor'
        Write-Info 'Browser UI:          tensor --browser'
        Write-Info 'Headless:            tensor --headless'
        Write-Info 'Open a new terminal if this session still cannot find tensor.'
    } finally {
        if (Test-Path $workDir) {
            Remove-Item -Recurse -Force $workDir -ErrorAction SilentlyContinue
        }
    }
} @args
