# Install fieldtrip from a GitHub release.
#
#   irm https://github.com/eddieland/fieldtrip/releases/latest/download/install.ps1 | iex
#
# Environment:
#   FIELDTRIP_VERSION      Version to install, e.g. 0.1.0. Defaults to the latest release.
#   FIELDTRIP_INSTALL_DIR  Where to put the binary. Defaults to %LOCALAPPDATA%\fieldtrip\bin.
#
# Supports Windows on x64. Windows on Arm runs the x64 build under emulation.

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'

    $repo = 'eddieland/fieldtrip'
    $target = 'x86_64-pc-windows-msvc'
    $archive = "fieldtrip-$target.zip"

    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    if ($arch -ne 'X64' -and $arch -ne 'Arm64') {
        throw "fieldtrip-install: unsupported architecture: $arch"
    }

    $version = $env:FIELDTRIP_VERSION
    if ($version) {
        $base = "https://github.com/$repo/releases/download/v$($version.TrimStart('v'))"
    } else {
        $base = "https://github.com/$repo/releases/latest/download"
    }

    $installDir = $env:FIELDTRIP_INSTALL_DIR
    if (-not $installDir) {
        $installDir = Join-Path $env:LOCALAPPDATA 'fieldtrip\bin'
    }

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("fieldtrip-" + [System.Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor
            [Net.SecurityProtocolType]::Tls12

        $label = if ($version) { $version } else { 'latest' }
        Write-Host "fieldtrip-install: downloading $archive ($label)"
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile (Join-Path $tmp $archive)
        Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')

        $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match "\s\*?$([regex]::Escape($archive))$" }
        if (-not $line) {
            throw "fieldtrip-install: no checksum for $archive in SHA256SUMS"
        }
        $expected = ($line -split '\s+')[0].ToLowerInvariant()
        $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $archive)).Hash.ToLowerInvariant()
        if ($expected -ne $actual) {
            throw "fieldtrip-install: checksum mismatch for ${archive}: expected $expected, got $actual"
        }

        Expand-Archive -Path (Join-Path $tmp $archive) -DestinationPath $tmp -Force
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        $exe = Join-Path $installDir 'fieldtrip.exe'
        Copy-Item -Path (Join-Path $tmp "fieldtrip-$target\fieldtrip.exe") -Destination $exe -Force

        Write-Host "fieldtrip-install: installed $(& $exe --version) to $exe"

        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = if ($userPath) { $userPath -split ';' } else { @() }
        if ($entries -notcontains $installDir) {
            $newPath = (@($entries | Where-Object { $_ }) + $installDir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            $env:Path = "$env:Path;$installDir"
            Write-Host "fieldtrip-install: added $installDir to your user PATH; restart your terminal and MCP client to pick it up"
        }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}
