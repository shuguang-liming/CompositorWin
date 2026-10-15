[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CompilerPath,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')]
    [string]$PackageName = 'Compositor-Windows-zh-CN-original-ui',
    [string]$RecordDirectory = 'build\validation\current',
    [ValidatePattern('^$|^Compositor-QA-[a-f0-9]{12}$')]
    [string]$QaName = ''
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'project.ps1')
$root = Get-ProjectRoot
$payload = Resolve-ProjectPath $root "dist/$PackageName"
$record = Resolve-ProjectPath $root $RecordDirectory
[IO.Directory]::CreateDirectory($record) | Out-Null
$compiler = (Get-Item -LiteralPath $CompilerPath).FullName
if ((Split-Path -Leaf $compiler) -ne 'makensis.exe') { throw 'Use the official NSIS makensis.exe compiler.' }
$compilerVersion = (& $compiler /VERSION) -join ''
if ($LASTEXITCODE -ne 0 -or $compilerVersion.Trim() -ne 'v3.11') { throw 'The reproducible installer requires official NSIS 3.11.' }
$compilerCopying = Join-Path (Split-Path -Parent $compiler) 'COPYING'
if (-not (Test-Path -LiteralPath $compilerCopying -PathType Leaf)) { throw 'Official NSIS COPYING is missing.' }
$files = [ordered]@{
    'compositor.exe' = 'HASH_EXE'; 'README.md' = 'HASH_README'; 'LICENSES.md' = 'HASH_LICENSES'
    'SKILL.md' = 'HASH_SKILL'; 'runtime-imports.json' = 'HASH_RUNTIME'; 'package-files.json' = 'HASH_MANIFEST'
    'USER_AGENT_SKILL.md' = 'HASH_USER_SKILL'
}
$cargoText = [IO.File]::ReadAllText((Join-Path $root 'Cargo.toml'))
$versionMatch = [regex]::Match($cargoText, '(?ms)^\[package\]\s*.*?^version\s*=\s*"(\d+\.\d+\.\d+)"\s*$')
if (-not $versionMatch.Success) { throw 'Cargo package version must be a stable numeric major.minor.patch version.' }
$appVersion = $versionMatch.Groups[1].Value
$versionParts = $appVersion.Split('.')
if (@($versionParts | Where-Object { [uint64]$_ -gt 65535 }).Count) { throw 'The package version exceeds Windows PE version component limits.' }
$windowsVersion = $appVersion + '.0'
$executableVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo((Join-Path $payload 'compositor.exe'))
if ($executableVersion.ProductVersion -ne $appVersion -or $executableVersion.FileVersion -ne $appVersion -or
    $executableVersion.FileMajorPart -ne [int]$versionParts[0] -or
    $executableVersion.FileMinorPart -ne [int]$versionParts[1] -or
    $executableVersion.FileBuildPart -ne [int]$versionParts[2] -or $executableVersion.FilePrivatePart -ne 0) {
    throw 'The portable executable version differs from Cargo.toml; package the new Release before creating its installer.'
}
$manifest = @(Get-Content -LiteralPath (Join-Path $payload 'package-files.json') -Raw | ConvertFrom-Json)
$fileRecords = @()
$defines = @(('!define APP_VERSION "' + $appVersion + '"'), ('!define WINDOWS_VERSION "' + $windowsVersion + '"'))
foreach ($name in $files.Keys) {
    $path = Resolve-ProjectPath $root "dist/$PackageName/$name"
    $item = Get-Item -LiteralPath $path
    if ($item.PSIsContainer) { throw "Installer payload must be a regular file: $name" }
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    if ($name -ne 'package-files.json') {
        $entries = @($manifest | Where-Object { $_.path -ceq $name })
        if ($entries.Count -ne 1 -or $entries[0].sha256 -ne $hash -or $entries[0].bytes -ne $item.Length) {
            throw "Installer payload differs from the validated portable package: $name"
        }
    }
    $defines += '!define ' + $files[$name] + ' "' + $hash + '"'
    $fileRecords += [pscustomobject]@{ path = $name; bytes = $item.Length; sha256 = $hash }
}
$runtime = Get-Content -LiteralPath (Join-Path $payload 'runtime-imports.json') -Raw | ConvertFrom-Json
if ($runtime.executable.sha256 -ne $fileRecords[0].sha256) { throw 'The installer executable does not match its runtime audit.' }
if ($runtime.staticCrtVerified -ne $true) { throw 'The installer executable is not verified as statically linked to the MSVC/UCRT runtime.' }
if ($runtime.requirements.targetWindows -ne 'Windows 10/11 x64') { throw 'The runtime audit does not target Windows 10/11 x64.' }
if ($runtime.manifest.windows10CompatibilityId -ne '{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}' -or
    $runtime.manifest.perMonitorV2 -ne $true -or $runtime.manifest.requestedExecutionLevel -ne 'asInvoker') {
    throw 'The executable manifest does not declare the Windows 10/11 compatibility and DPI policy.'
}
$importPolicy = $runtime.windows10ImportPolicy
if ($null -eq $importPolicy -or $importPolicy.passed -ne $true -or @($importPolicy.forbiddenImportsFound).Count -ne 0) {
    throw 'The runtime audit found imports outside the Windows 10 API baseline.'
}
$python = (Get-Command python -CommandType Application -ErrorAction Stop).Source
# The EXE contains full originals, including this exact compiler's copyright.
$licenseResult = & $python -B -X utf8 (Join-Path $root 'generate-open-source-licenses.py') --check --verify-exe (Join-Path $payload 'compositor.exe') --verify-package-index (Join-Path $payload 'LICENSES.md') --nsis-copying $compilerCopying
if ($LASTEXITCODE -ne 0) { throw 'Embedded license, package index or official NSIS copyright verification failed.' }
$licenseSummary = ($licenseResult -join [Environment]::NewLine) | ConvertFrom-Json
$mode = if ($QaName) { $QaName } else { 'production' }
$installerSource = [IO.File]::ReadAllText((Join-Path $root 'installer.nsi'))
foreach ($requiredInstallerPolicy in @(
    'ManifestSupportedOS\s+Win10',
    '\$\{RunningX64\}',
    '\$\{AtLeastWin10\}',
    'CurrentBuildNumber',
    'IntCmp\s+\$0\s+19045',
    '\u4ec5\u652f\u6301 64 \u4f4d Windows 10 \u6216 Windows 11',
    '\u4ec5\u652f\u6301 Windows 10\uff0822H2\uff09\u6216 Windows 11'
)) {
    if ($installerSource -notmatch $requiredInstallerPolicy) {
        throw "Installer is missing the Windows 10/11 compatibility policy: $requiredInstallerPolicy"
    }
}
$outputRelative = if ($QaName) { "$RecordDirectory/$QaName-Setup.exe" } else { 'dist/Compositor-Windows-Setup-x64.exe' }
$output = Resolve-ProjectPath $root $outputRelative
if (Test-Path -LiteralPath $output) { throw 'An existing setup executable is retained. Move it to a review record before rebuilding.' }
$definePath = Join-Path $record "installer-$mode.nsh"
[IO.File]::WriteAllLines($definePath, $defines, (New-Object Text.UTF8Encoding($false)))
$arguments = @('/INPUTCHARSET', 'UTF8', "/DPAYLOAD_DEFINES=$definePath", "/DPAYLOAD_DIR=$payload", "/DSETUP_OUTPUT=$output", "/DAPP_ICON=$(Join-Path $root 'assets\app-icon.ico')")
if ($QaName) { $arguments += @("/DAPP_NAME=$QaName", "/DAPP_ID=$QaName") }
$arguments += (Join-Path $root 'installer.nsi')
$timer = [Diagnostics.Stopwatch]::StartNew()
& $compiler @arguments | Tee-Object -FilePath (Join-Path $record "installer-$mode-build.log")
$exitCode = $LASTEXITCODE
$timer.Stop()
if ($exitCode -ne 0) { throw "Installer compilation failed ($exitCode)." }
$report = [pscustomobject]@{
    status = 'passed'; checkedUtc = (Get-Date).ToUniversalTime().ToString('o')
    mode = $mode; sourcePackage = $payload; seconds = $timer.Elapsed.TotalSeconds
    appVersion = $appVersion; windowsVersion = $windowsVersion
    compiler = $compiler; compilerVersion = $compilerVersion.Trim()
    compilerSha256 = (Get-FileHash -LiteralPath $compiler -Algorithm SHA256).Hash
    compilerCopyrightSha256 = (Get-FileHash -LiteralPath $compilerCopying -Algorithm SHA256).Hash
    embeddedLicenses = $licenseSummary
    sourceSha256 = (Get-FileHash -LiteralPath (Join-Path $root 'installer.nsi') -Algorithm SHA256).Hash
    executableSha256 = $fileRecords[0].sha256; installedFiles = $fileRecords
    installer = $output; installerBytes = (Get-Item -LiteralPath $output).Length
    installerSha256 = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash
    installDirectory = '%LOCALAPPDATA%\Programs\' + $(if ($QaName) { $QaName } else { 'Compositor' })
    scope = 'current-user'; associationsCreated = $false
}
Write-ProjectJson $report (Join-Path $record "installer-$mode.json")
$report | ConvertTo-Json -Depth 8
