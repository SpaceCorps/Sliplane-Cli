# Install the Sliplane CLI (https://github.com/SpaceCorps/Sliplane-Cli) on Windows without admin rights.
#
#   irm https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.ps1 | iex
#
# Optional environment variables, set before running:
#   SLIPLANE_VERSION        a version such as 1.3.0 (default: the latest release)
#   SLIPLANE_INSTALL_DIR    where to put sliplane.exe (default: %LOCALAPPDATA%\sliplane-cli\bin)
#   SLIPLANE_NO_PATH=1      do not add the install directory to your user PATH
#   SLIPLANE_RELEASES_URL   a mirror of the GitHub releases page, for air-gapped installs and tests
#
# Written for Windows, following the flow of install.sh, which is adapted from the official Sliplane
# CLI's installer (Apache License 2.0, Copyright 2026 Sliplane GmbH); see THIRD_PARTY_NOTICES.md.

$ErrorActionPreference = 'Stop'

# Everything lives in a function so a truncated download cannot run half an install.
function Install-Sliplane {
    # Windows PowerShell 5.1 defaults to protocols GitHub no longer accepts.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    # Progress rendering makes Invoke-WebRequest several times slower on 5.1.
    $ProgressPreference = 'SilentlyContinue'

    $architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    switch ($architecture) {
        'AMD64' { }
        'ARM64' { Write-Host 'Windows on ARM: installing the x64 build, which runs under emulation.' }
        default { throw "Supported architectures are x64 and ARM64; this machine reports $architecture." }
    }
    $target = 'x86_64-pc-windows-msvc'

    $releases = if ($env:SLIPLANE_RELEASES_URL) { $env:SLIPLANE_RELEASES_URL.TrimEnd('/') } else { 'https://github.com/SpaceCorps/Sliplane-Cli/releases' }
    $version = $env:SLIPLANE_VERSION
    if (-not $version) {
        # The /latest URL redirects to /tag/vX.Y.Z; reading the redirect avoids GitHub API rate limits.
        $request = [System.Net.HttpWebRequest]::Create("$releases/latest")
        $request.Method = 'HEAD'
        $request.AllowAutoRedirect = $false
        try {
            $response = $request.GetResponse()
        } catch {
            throw 'Could not find the latest release. Check your connection and try again.'
        }
        try { $location = $response.Headers['Location'] } finally { $response.Close() }
        if (-not $location -or $location -notmatch '/tag/(v[^/]+)$') {
            throw 'GitHub did not return a published release.'
        }
        $version = $Matches[1]
    }
    $version = $version -replace '^v', ''
    if ($version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') {
        throw 'Use a version such as 1.3.0 or v1.3.0-rc.1.'
    }

    $installDirectory = if ($env:SLIPLANE_INSTALL_DIR) { $env:SLIPLANE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'sliplane-cli\bin' }
    $destination = Join-Path $installDirectory 'sliplane.exe'
    if (Test-Path -LiteralPath $destination -PathType Container) {
        throw "$destination is a directory. Choose another SLIPLANE_INSTALL_DIR."
    }

    $package = "sliplane-v$version-$target"
    $archive = "$package.zip"
    $release = "$releases/download/v$version"
    $work = Join-Path ([IO.Path]::GetTempPath()) ("sliplane-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    $staged = Join-Path $installDirectory 'sliplane.new.exe'
    try {
        Write-Host "Downloading Sliplane $version ($target)..."
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$release/$archive" -OutFile (Join-Path $work $archive)
        } catch {
            throw "Could not download $archive. Does release v$version exist for this platform?"
        }
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$release/checksums.txt" -OutFile (Join-Path $work 'checksums.txt')
        } catch {
            throw "Could not download checksums.txt for v$version. Releases before 1.3.0 have none; a release still being built may need a few minutes."
        }

        $expected = @(Get-Content -LiteralPath (Join-Path $work 'checksums.txt') |
            ForEach-Object { if ($_ -match '^([0-9a-fA-F]{64})\s+\*?(\S+)$' -and $Matches[2] -eq $archive) { $Matches[1] } })
        if ($expected.Count -ne 1) {
            throw "The release must contain exactly one checksum for $archive."
        }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $work $archive)).Hash
        if ($actual -ne $expected[0]) {
            throw 'Checksum verification failed. The existing installation was not changed.'
        }

        Expand-Archive -LiteralPath (Join-Path $work $archive) -DestinationPath $work -Force
        $extracted = Join-Path $work "$package\sliplane.exe"
        if (-not (Test-Path -LiteralPath $extracted -PathType Leaf)) {
            throw 'The release does not contain sliplane.exe.'
        }

        New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null
        Copy-Item -LiteralPath $extracted -Destination $staged -Force
        $reported = (& $staged --version) -join ''
        if ($LASTEXITCODE -ne 0 -or $reported -ne "sliplane $version") {
            throw "The downloaded executable reported '$reported' instead of 'sliplane $version'. The existing installation was not changed."
        }

        $firstInstall = -not (Test-Path -LiteralPath $destination)
        $previous = $null
        if (-not $firstInstall) {
            try { $previous = ((& $destination --version) -join '') -replace '^sliplane ', '' } catch { }
        }
        try {
            Move-Item -LiteralPath $staged -Destination $destination -Force
        } catch {
            throw "Cannot replace $destination. Close any running sliplane processes and try again."
        }
        if ($firstInstall) {
            Write-Host "Installed Sliplane $version to $destination"
        } elseif ($previous -and $previous -ne $version) {
            Write-Host "Updated Sliplane $previous -> $version"
        } else {
            Write-Host "Reinstalled Sliplane $version"
        }
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    }

    $onPath = ($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $installDirectory.TrimEnd('\') }
    if (-not $onPath) {
        if ($env:SLIPLANE_NO_PATH -eq '1') {
            Write-Host "`nAdd $installDirectory to your PATH to use Sliplane."
        } else {
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $entries = if ($userPath) { $userPath -split ';' | Where-Object { $_ } } else { @() }
            if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $installDirectory.TrimEnd('\') })) {
                [Environment]::SetEnvironmentVariable('Path', (($entries + $installDirectory) -join ';'), 'User')
            }
            $env:Path = "$installDirectory;$env:Path"
            Write-Host "`nAdded $installDirectory to your user PATH. Terminals opened from now on will find sliplane."
        }
    }
    $current = Get-Command sliplane -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($current -and $current.Source -ne $destination) {
        Write-Host "`nYour PATH currently selects $($current.Source), not this installation (another Sliplane CLI, such as the official one, uses the same name).`nPut $installDirectory first on PATH to use this one."
    }
    if ($firstInstall) {
        Write-Host "`nGet started:`n`n  sliplane login`n  sliplane agent-readme   # the manual for an LLM agent"
    }
}

Install-Sliplane
