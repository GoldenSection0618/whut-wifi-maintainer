param(
    [Parameter(Mandatory)]
    [string]$OutputDirectory,
    [string]$Worker = "$PSScriptRoot/../target/cargo/release/whut-wifi-maintainer.exe",
    [switch]$NoRestore
)

$ErrorActionPreference = 'Stop'
$workerPath = (Resolve-Path -LiteralPath $Worker).Path
if (Test-Path -LiteralPath $OutputDirectory) {
    throw "Publish directory already exists: $OutputDirectory"
}

$publishArgs = @('publish', "$PSScriptRoot/WhutWifi.WinUI.csproj", '-c', 'Release',
    '-o', $OutputDirectory, "-p:KeepaliveBinary=$workerPath")
if ($NoRestore) {
    $publishArgs += '--no-restore'
}
dotnet @publishArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

foreach ($file in 'whut-wifi-maintainer.exe', 'whut-wifi-maintainer.dll',
    'whut-wifi-maintainer.pri', 'whut-wifi-worker.exe', 'Assets/wifi.ico') {
    if (!(Test-Path -LiteralPath (Join-Path $OutputDirectory $file) -PathType Leaf)) {
        throw "Windows package is missing $file"
    }
}
