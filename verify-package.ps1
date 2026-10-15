[CmdletBinding()]
param(
    [string]$RepositoryRoot,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')]
    [string]$PackageName = 'Compositor-Windows',
    [string]$ReportPath = 'build/validation/current/package-verification.json'
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'project.ps1')
$root = Get-ProjectRoot $RepositoryRoot
$packageRelative = "dist/$PackageName"
$packageRoot = Resolve-ProjectPath $root $packageRelative
$archivePath = Resolve-ProjectPath $root "dist/$PackageName-x64.zip"
$manifest = @(Get-Content -LiteralPath (Join-Path $packageRoot 'package-files.json') -Raw | ConvertFrom-Json)
if ($manifest.Count -eq 0) { throw 'The package manifest is empty.' }
$expected = @{}
foreach ($record in $manifest) {
    if ($expected.ContainsKey($record.path)) { throw "Duplicate package manifest entry: $($record.path)" }
    $path = Resolve-ProjectPath $root "$packageRelative/$($record.path)"
    $file = Get-Item -LiteralPath $path
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    if ($file.PSIsContainer -or $file.Length -ne $record.bytes -or $hash -ne $record.sha256) {
        throw "Packaged file differs from its manifest: $($record.path)"
    }
    $expected[$record.path] = $record
}
$manifestFile = Get-Item -LiteralPath (Join-Path $packageRoot 'package-files.json')
$expected['package-files.json'] = [pscustomobject]@{
    path = 'package-files.json'; bytes = $manifestFile.Length
    sha256 = (Get-FileHash -LiteralPath $manifestFile.FullName -Algorithm SHA256).Hash
}
$actualFiles = @(Get-ChildItem -LiteralPath $packageRoot -Recurse -File)
if ($actualFiles.Count -ne $expected.Count) { throw 'Unrecorded files exist in the package directory.' }
$releaseHash = (Get-FileHash -LiteralPath (Resolve-ProjectPath $root 'build/rust/release/compositor.exe') -Algorithm SHA256).Hash
if ($expected['compositor.exe'].sha256 -ne $releaseHash) { throw 'The package does not contain the validated Release executable.' }
$runtime = Get-Content -LiteralPath (Join-Path $packageRoot 'runtime-imports.json') -Raw | ConvertFrom-Json
if ($runtime.executable.sha256 -ne $releaseHash) { throw 'The PE dependency audit describes a different executable.' }
if ($runtime.staticCrtVerified -ne $true) { throw 'The packaged executable is not verified as statically linked to the MSVC/UCRT runtime.' }
if ($runtime.requirements.targetWindows -ne 'Windows 10/11 x64') { throw 'The package runtime audit does not target Windows 10/11 x64.' }
if ($runtime.manifest.windows10CompatibilityId -ne '{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}' -or
    $runtime.manifest.perMonitorV2 -ne $true -or $runtime.manifest.requestedExecutionLevel -ne 'asInvoker') {
    throw 'The packaged executable manifest does not declare the Windows 10/11 compatibility and DPI policy.'
}
$importPolicy = $runtime.windows10ImportPolicy
if ($null -eq $importPolicy -or $importPolicy.passed -ne $true -or @($importPolicy.forbiddenImportsFound).Count -ne 0) {
    throw 'The package runtime audit found imports outside the Windows 10 API baseline.'
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
$checked = @{}
try {
    foreach ($entry in $archive.Entries) {
        if ($entry.FullName.EndsWith('/')) { continue }
        $name = $entry.FullName.Replace('\', '/')
        if ($checked.ContainsKey($name) -or -not $expected.ContainsKey($name)) { throw "Unexpected ZIP entry: $name" }
        $stream = $entry.Open()
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try { $hash = [System.BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '') }
        finally { $sha.Dispose(); $stream.Dispose() }
        if ($entry.Length -ne $expected[$name].bytes -or $hash -ne $expected[$name].sha256) {
            throw "ZIP data differs from the package manifest: $name"
        }
        $checked[$name] = $true
    }
} finally { $archive.Dispose() }
if ($checked.Count -ne $expected.Count) { throw 'The ZIP omits packaged files.' }
$python = (Get-Command python -CommandType Application -ErrorAction Stop).Source
$licenseResult = & $python -B -X utf8 (Join-Path $root 'generate-open-source-licenses.py') --check --verify-exe (Join-Path $packageRoot 'compositor.exe') --verify-package-index (Join-Path $packageRoot 'LICENSES.md')
if ($LASTEXITCODE -ne 0) { throw 'Embedded license original-text or package index verification failed.' }
$licenseSummary = ($licenseResult -join [Environment]::NewLine) | ConvertFrom-Json
$report = [pscustomobject]@{
    status = 'passed'; checkedUtc = (Get-Date).ToUniversalTime().ToString('o')
    package = $packageRelative; manifestFiles = $manifest.Count; zipFiles = $checked.Count
    embeddedLicenses = $licenseSummary
    executableSha256 = $releaseHash
    archiveBytes = (Get-Item -LiteralPath $archivePath).Length
    archiveSha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash
}
$reportFile = Resolve-ProjectPath $root $ReportPath
[System.IO.Directory]::CreateDirectory((Split-Path -Parent $reportFile)) | Out-Null
Write-ProjectJson $report $reportFile
$report | ConvertTo-Json
