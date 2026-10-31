# Offline end-to-end test of install.ps1 against a fake release built from a given binary.
#
#   pwsh scripts/test-install.ps1 target\debug\sliplane.exe
#
# Serves the fake release from a local Python web server, because Invoke-WebRequest cannot read file:// URLs.
param([Parameter(Mandatory)][string]$Binary)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$version = ((& $Binary --version) -split ' ')[1]
$work = Join-Path ([IO.Path]::GetTempPath()) ("sliplane-test-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$passed = 0
$server = $null

function Pass($name) { Write-Host "ok   $name"; $script:passed++ }
function Fail($message) { Write-Error "FAIL $message"; exit 1 }

# Runs the installer in a child process so a failure cannot end this script, and returns its output and exit code.
function Invoke-Installer {
    $output = & pwsh -NoProfile -File (Join-Path $here 'install.ps1') 2>&1 | Out-String
    [pscustomobject]@{ Output = $output; Code = $LASTEXITCODE }
}

try {
    $target = 'x86_64-pc-windows-msvc'
    $package = "sliplane-v$version-$target"
    $release = Join-Path $work "releases\download\v$version"
    New-Item -ItemType Directory -Path (Join-Path $work $package), $release | Out-Null
    Copy-Item -LiteralPath $Binary -Destination (Join-Path $work "$package\sliplane.exe")
    Compress-Archive -Path (Join-Path $work $package) -DestinationPath (Join-Path $release "$package.zip")
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $release "$package.zip")).Hash.ToLower()
    $checksums = Join-Path $release 'checksums.txt'
    Set-Content -LiteralPath $checksums -Value "$hash  $package.zip" -Encoding ascii

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $server = Start-Process python -ArgumentList '-m', 'http.server', $port, '--bind', '127.0.0.1', '--directory', (Join-Path $work 'releases') -PassThru -WindowStyle Hidden
    for ($i = 0; $i -lt 50; $i++) {
        try { Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$port/" | Out-Null; break } catch { Start-Sleep -Milliseconds 100 }
    }

    $installDirectory = Join-Path $work 'install\bin'
    $env:SLIPLANE_RELEASES_URL = "http://127.0.0.1:$port"
    $env:SLIPLANE_INSTALL_DIR = $installDirectory
    $env:SLIPLANE_VERSION = $version
    $env:SLIPLANE_NO_PATH = '1'   # never touch the real user PATH from a test
    $installed = Join-Path $installDirectory 'sliplane.exe'

    $result = Invoke-Installer
    if ($result.Code -ne 0) { Fail "fresh install failed: $($result.Output)" }
    if (((& $installed --version) -join '') -ne "sliplane $version") { Fail 'installed binary does not report the version' }
    if ($result.Output -notmatch "Installed Sliplane $([regex]::Escape($version))") { Fail "no install message: $($result.Output)" }
    Pass 'fresh install'

    $env:SLIPLANE_VERSION = "v$version"
    $result = Invoke-Installer
    if ($result.Code -ne 0) { Fail "v-prefixed version rejected: $($result.Output)" }
    if ($result.Output -notmatch 'Reinstalled') { Fail "re-install not recognised: $($result.Output)" }
    Pass 'v-prefixed version and re-install'
    $env:SLIPLANE_VERSION = $version

    # A corrupted archive is rejected and the existing install survives untouched
    Set-Content -LiteralPath $installed -Value 'previous install' -NoNewline
    Set-Content -LiteralPath $checksums -Value ("0" * 64 + "  $package.zip") -Encoding ascii
    $result = Invoke-Installer
    if ($result.Code -eq 0) { Fail 'tampered checksum was accepted' }
    if ($result.Output -notmatch 'Checksum verification failed') { Fail "wrong error for bad checksum: $($result.Output)" }
    if ((Get-Content -LiteralPath $installed -Raw) -ne 'previous install') { Fail 'failed install changed the existing binary' }
    if (Test-Path (Join-Path $installDirectory 'sliplane.new.exe')) { Fail 'failed install left a staged file behind' }
    Pass 'bad checksum rejected, existing install kept'

    Remove-Item -LiteralPath $checksums
    $result = Invoke-Installer
    if ($result.Code -eq 0) { Fail 'install without checksums.txt was accepted' }
    if ($result.Output -notmatch 'checksums\.txt') { Fail "wrong error for missing checksums: $($result.Output)" }
    Pass 'missing checksums.txt rejected'
    Set-Content -LiteralPath $checksums -Value "$hash  $package.zip" -Encoding ascii

    $env:SLIPLANE_VERSION = 'not-a-version'
    $result = Invoke-Installer
    if ($result.Code -eq 0 -or $result.Output -notmatch 'Use a version such as') { Fail "garbage version not rejected: $($result.Output)" }
    Pass 'invalid version rejected'

    $env:SLIPLANE_VERSION = '9.9.9'
    $result = Invoke-Installer
    if ($result.Code -eq 0 -or $result.Output -notmatch 'Could not download') { Fail "nonexistent release not rejected: $($result.Output)" }
    Pass 'nonexistent release rejected'

    Write-Host "`n$passed checks passed"
    # The last installer run failed on purpose, and some hosts (GitHub Actions' pwsh step among them)
    # exit with whatever the last native command left here.
    $global:LASTEXITCODE = 0
} finally {
    if ($server) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
