<#
.SYNOPSIS
Build, audit, package and diagnose CompositorWin from one development script.
.DESCRIPTION
PowerShell commands keep their original named parameters. Python commands retain
their original --options and run the readable embedded source in memory. Python
3.11+ is required for Python tools; GenerateIcon also requires Pillow.
.EXAMPLE
.\project.ps1 Build -Action All -Offline -Jobs 1
.EXAMPLE
.\project.ps1 GenerateLicenses -PythonPath python --check
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Position = 0)]
    [string]$Command = 'Help',
    [string]$PythonPath,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ToolArguments
)
$script:ProjectPythonPath = $PythonPath
Set-StrictMode -Version Latest

function Get-ProjectRoot([string]$RepositoryRoot) {
    if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
        $RepositoryRoot = $PSScriptRoot
    }
    $root = [System.IO.Path]::GetFullPath($RepositoryRoot).TrimEnd('\', '/')
    if (-not (Test-Path -LiteralPath $root -PathType Container)) { throw "Repository directory is missing: $root" }
    return $root
}

function Resolve-ProjectPath([string]$Root, [string]$RelativePath) {
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [System.IO.Path]::IsPathRooted($RelativePath) -or $RelativePath.Contains(':')) {
        throw "Manifest requires a relative file path: $RelativePath"
    }
    $parts = $RelativePath.Replace('\', '/').Split('/')
    if ($parts -contains '..' -or $parts -contains '.' -or $parts -contains '') { throw "Unsafe relative path: $RelativePath" }
    $full = [System.IO.Path]::GetFullPath((Join-Path $Root $RelativePath))
    $prefix = $Root + [System.IO.Path]::DirectorySeparatorChar
    if (-not $full.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) { throw "Path leaves repository: $RelativePath" }
    $cursor = $full
    while ($cursor -and $cursor.Length -ge $Root.Length) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Reparse points are excluded from project operations: $cursor"
            }
        }
        if ($cursor.Equals($Root, [System.StringComparison]::OrdinalIgnoreCase)) { break }
        $cursor = Split-Path -Parent $cursor
    }
    return $full
}

function Write-ProjectJson([object]$Value, [string]$Path) {
    $json = ConvertTo-Json -InputObject $Value -Depth 12
    [System.IO.File]::WriteAllText($Path, $json + [Environment]::NewLine, (New-Object System.Text.UTF8Encoding($false)))
}

function Invoke-ProjectPython {
    param([string]$Name, [string[]]$Arguments = @(), [string]$RepositoryRoot)
    $toolRoot = Get-ProjectRoot $RepositoryRoot
    $python = if ($script:ProjectPythonPath) {
        (Get-Command $script:ProjectPythonPath -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    } else {
        (Get-Command python -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    }
    # stdin avoids Windows PowerShell 5.1's lossy quoting of multiline -c arguments.
    $script:ProjectPythonBootstrap | & $python -B -X utf8 - $toolRoot $Name @Arguments
}

# Former build.ps1
function Invoke-ProjectBuild {
[CmdletBinding()]
param(
    [ValidateSet('Check', 'Test', 'Release', 'Package', 'All')]
    [string]$Action = 'All',
    [switch]$Offline,
    [ValidateRange(1, 256)]
    [int]$Jobs = 1,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')]
    [string]$PackageName = 'Compositor-Windows',
    [string]$RecordDirectory = 'build\validation\current'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = [System.IO.Path]::GetFullPath($PSScriptRoot)
$env:CARGO_TARGET_DIR = Join-Path $projectRoot 'build\rust'
$env:CARGO_BUILD_JOBS = $Jobs.ToString()
$env:TEMP = Join-Path $projectRoot 'build\temp'
$env:TMP = $env:TEMP
[System.IO.Directory]::CreateDirectory($env:TEMP) | Out-Null
if ($env:CARGO_HOME) { $env:PATH = "$(Join-Path $env:CARGO_HOME 'bin');$env:PATH" }
$cargo = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
if (-not (Test-Path -LiteralPath (Join-Path $projectRoot 'Cargo.lock') -PathType Leaf)) { throw 'The dependency lock file is missing.' }
$recordDirectory = [System.IO.Path]::GetFullPath((Join-Path $projectRoot $RecordDirectory))
if (-not $recordDirectory.StartsWith($projectRoot.TrimEnd('\') + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Validation records must remain inside this Windows workspace.'
}
[System.IO.Directory]::CreateDirectory($recordDirectory) | Out-Null
$steps = New-Object 'System.Collections.Generic.List[object]'
$started = (Get-Date).ToUniversalTime()
$completed = $false
$packaged = $false
$toolchain = (& $cargo --version) -join ''
if ($LASTEXITCODE -ne 0) { throw 'Cannot execute the installed Cargo toolchain.' }

function Get-ReleaseCrtFlags {
    $variable = if (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) { 'CARGO_ENCODED_RUSTFLAGS' } else { 'RUSTFLAGS' }
    $current = [Environment]::GetEnvironmentVariable($variable, 'Process')
    if ($variable -eq 'RUSTFLAGS' -and -not (Test-Path Env:RUSTFLAGS)) {
        # A global rustflags variable overrides Cargo's target/build configuration.
        # Stop rather than silently discard an existing customized configuration.
        $configured = @(Get-ChildItem Env: | Where-Object { $_.Name -match '^CARGO_(TARGET_.+|BUILD)_RUSTFLAGS$' } | ForEach-Object { $_.Name })
        $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path ([Environment]::GetFolderPath('UserProfile')) '.cargo' }
        $directories = @($cargoHome)
        $ancestor = [IO.DirectoryInfo]$projectRoot
        while ($null -ne $ancestor) {
            $directories += Join-Path $ancestor.FullName '.cargo'
            $ancestor = $ancestor.Parent
        }
        foreach ($directory in $directories | Select-Object -Unique) {
            foreach ($name in @('config', 'config.toml')) {
                $configuration = Join-Path $directory $name
                if ((Test-Path -LiteralPath $configuration -PathType Leaf) -and
                    [IO.File]::ReadAllText($configuration) -match '(?m)^\s*(?!#)(?:rustflags|[^#\r\n=]+\.rustflags)\s*=') {
                    $configured += $configuration
                }
            }
        }
        if ($configured.Count) {
            throw "Release CRT flags would override configured rustflags: $($configured -join ', '). Supply the intended combined flags through RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS."
        }
    }
    if ($current -match '(?i)(?:^|[,\s=\x1f])-crt-static(?:$|[,\s\x1f])') {
        throw 'Release rustflags explicitly disable crt-static; remove the conflicting -crt-static option.'
    }
    $separator = if ($variable -eq 'CARGO_ENCODED_RUSTFLAGS') { [string][char]31 } else { ' ' }
    $addition = '-C' + $separator + 'target-feature=+crt-static'
    $value = if ([string]::IsNullOrEmpty($current)) { $addition } else { $current + $separator + $addition }
    return [pscustomobject]@{ variable = $variable; value = $value; feature = '+crt-static' }
}

function Invoke-Cargo([string[]]$Arguments) {
    $actual = @($Arguments)
    if ($Offline -and $Arguments[0] -ne 'fmt') { $actual = @($Arguments[0], '--offline') + $Arguments[1..($Arguments.Count - 1)] }
    if ($Arguments[0] -ne 'fmt') {
        $separator = [Array]::IndexOf($actual, '--')
        if ($separator -ge 0) {
            $actual = $actual[0..($separator - 1)] + @('-j', $Jobs.ToString()) + $actual[$separator..($actual.Count - 1)]
        } else { $actual += @('-j', $Jobs.ToString()) }
    }
    Write-Output "cargo $($actual -join ' ')"
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $releaseCrt = $Arguments[0] -eq 'build' -and '--release' -in $Arguments
    $savedFlags = @{}
    $crtFlags = $null
    foreach ($name in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')) {
        $savedFlags[$name] = @{ exists = (Test-Path "Env:$name"); value = [Environment]::GetEnvironmentVariable($name, 'Process') }
    }
    try {
        if ($releaseCrt) {
            $crtFlags = Get-ReleaseCrtFlags
            Set-Item -LiteralPath "Env:$($crtFlags.variable)" -Value $crtFlags.value
            Write-Output "Release CRT: -C target-feature=+crt-static via $($crtFlags.variable); restored after this invocation."
        }
        & $cargo @actual
        $code = $LASTEXITCODE
    }
    finally {
        foreach ($name in $savedFlags.Keys) {
            if ($savedFlags[$name].exists) { Set-Item -LiteralPath "Env:$name" -Value $savedFlags[$name].value }
            else { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        }
        $timer.Stop()
    }
    $steps.Add([pscustomobject]@{
        command = 'cargo ' + ($actual -join ' '); seconds = $timer.Elapsed.TotalSeconds; exitCode = $code
        staticCrt = $releaseCrt; rustflagsEnvironment = $(if ($null -ne $crtFlags) { $crtFlags.variable } else { $null })
        crtTargetFeature = $(if ($releaseCrt) { '+crt-static' } else { $null })
    })
    if ($code -ne 0) { throw "Cargo validation failed with exit code ${code}: $($actual -join ' ')" }
}

Push-Location $projectRoot
try {
    # This standalone workspace retains the original migration audit as historical evidence.
    Write-Output 'Standalone Windows workspace: original macOS checks remain in the source migration records.'
    if ($Action -in @('Check', 'All')) {
        Invoke-Cargo -Arguments @('fmt', '--check')
        Invoke-Cargo -Arguments @('check', '--locked', '--all-targets')
        Invoke-Cargo -Arguments @('clippy', '--locked', '--all-targets', '--', '-D', 'warnings')
    }
    if ($Action -in @('Test', 'All')) { Invoke-Cargo -Arguments @('test', '--locked') }
    if ($Action -in @('Release', 'Package', 'All')) { Invoke-Cargo -Arguments @('build', '--locked', '--release', '--bin', 'compositor') }
    # All runs checks, tests and a release build. Packaging is an explicit separate action.
    if ($Action -eq 'Package') {
        $releaseDirectory = Join-Path $projectRoot ("dist\$PackageName")
        $archive = Join-Path $projectRoot ("dist\$PackageName-x64.zip")
        if ((Test-Path -LiteralPath $releaseDirectory) -or (Test-Path -LiteralPath $archive)) { throw 'Existing package directory/archive is retained. Choose a fresh destination before packaging.' }
        [System.IO.Directory]::CreateDirectory($releaseDirectory) | Out-Null
        $sourceFiles = @(
            @{ source = (Join-Path $env:CARGO_TARGET_DIR 'release\compositor.exe'); destination = 'compositor.exe' },
            @{ source = (Join-Path $projectRoot 'packaging/runtime-imports.json'); destination = 'runtime-imports.json' },
            @{ source = (Join-Path $projectRoot 'skills/compositor-editor/SKILL.md'); destination = 'SKILL.md' },
            @{ source = (Join-Path $projectRoot 'skills/compositor-guide/SKILL.md'); destination = 'USER_AGENT_SKILL.md' }
        )
        # Ship one user guide, the Agent skill and the executable's runtime audit.
        foreach ($record in $sourceFiles) { Copy-Item -LiteralPath $record.source -Destination (Join-Path $releaseDirectory $record.destination) }
        $readme = [System.IO.File]::ReadAllText((Join-Path $projectRoot 'README.md'))
        $guide = [regex]::Match($readme, '(?s)<!-- USER-GUIDE-BEGIN -->\s*(.*?)\s*<!-- USER-GUIDE-END -->')
        if (-not $guide.Success) { throw 'The README user guide section is missing.' }
        [System.IO.File]::WriteAllText((Join-Path $releaseDirectory 'README.md'), $guide.Groups[1].Value + [Environment]::NewLine, (New-Object System.Text.UTF8Encoding($false)))
        # Full original notices ship inside the EXE; keep one small discoverable entry file.

        Invoke-ProjectPython -Name 'generate-open-source-licenses' -Arguments @('--check', '--verify-exe', (Join-Path $releaseDirectory 'compositor.exe'), '--write-package-index', (Join-Path $releaseDirectory 'LICENSES.md'))
        if ($LASTEXITCODE -ne 0) { throw 'Embedded license original-text verification or package index generation failed.' }

        $packageFiles = @(Get-ChildItem -LiteralPath $releaseDirectory -Recurse -File | ForEach-Object {
            [pscustomobject]@{ path = $_.FullName.Substring($releaseDirectory.Length + 1).Replace('\', '/'); bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
        })
        Write-ProjectJson @($packageFiles) (Join-Path $releaseDirectory 'package-files.json')
        # Compression is a packaging choice, measured separately from EXE code size.
        $zipReport = Join-Path $recordDirectory ("package-zip-$PackageName.json")
        Invoke-ProjectPython -Name 'package-zip' -Arguments @('--package-dir', $releaseDirectory, '--output', $archive, '--report', $zipReport)
        if ($LASTEXITCODE -ne 0) { throw 'Release ZIP compression or content verification failed; partial output and evidence are retained.' }
        $packaged = $true
        Get-FileHash -LiteralPath $archive -Algorithm SHA256
    }
    $completed = $true
}
finally {

    $report = [pscustomobject]@{ action = $Action; completed = $completed; startedUtc = $started.ToString('o'); endedUtc = (Get-Date).ToUniversalTime().ToString('o'); cargo = $toolchain; targetDirectory = $env:CARGO_TARGET_DIR; jobs = $Jobs; steps = @($steps.ToArray()); packagingPerformed = $packaged; releaseCrtPolicy = 'Only build --release appends -C target-feature=+crt-static; check/test flags and caches are unchanged, and both rustflags environment variables are restored.'; originalBaselineVerification = 'Historical migration evidence; not applicable to this standalone Windows workspace.' }
    Write-ProjectJson $report (Join-Path $recordDirectory ("build-$($Action.ToLowerInvariant()).json"))
    Pop-Location
}
}

# Former package-installer.ps1
function Invoke-ProjectInstaller {
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CompilerPath,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')]
    [string]$PackageName = 'Compositor-Windows-zh-CN-original-ui',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,100}\.exe$')]
    [string]$InstallerName = 'Compositor-Windows-Setup-x64.exe',
    [string]$RecordDirectory = 'build\validation\current',
    [ValidatePattern('^$|^Compositor-QA-[a-f0-9]{12}$')]
    [string]$QaName = ''
)
$ErrorActionPreference = 'Stop'

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

# The EXE contains full originals, including this exact compiler's copyright.
$licenseResult = Invoke-ProjectPython -Name 'generate-open-source-licenses' -Arguments @('--check', '--verify-exe', (Join-Path $payload 'compositor.exe'), '--verify-package-index', (Join-Path $payload 'LICENSES.md'), '--nsis-copying', $compilerCopying)
if ($LASTEXITCODE -ne 0) { throw 'Embedded license, package index or official NSIS copyright verification failed.' }
$licenseSummary = ($licenseResult -join [Environment]::NewLine) | ConvertFrom-Json
$mode = if ($QaName) { $QaName } else { 'production' }
$installerSource = [IO.File]::ReadAllText((Join-Path $root 'packaging/installer.nsi'))
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
$outputRelative = if ($QaName) { "$RecordDirectory/$QaName-Setup.exe" } else { "dist/$InstallerName" }
$output = Resolve-ProjectPath $root $outputRelative
if (Test-Path -LiteralPath $output) { throw 'An existing setup executable is retained. Move it to a review record before rebuilding.' }
$definePath = Join-Path $record "installer-$mode.nsh"
[IO.File]::WriteAllLines($definePath, $defines, (New-Object Text.UTF8Encoding($false)))
$arguments = @('/INPUTCHARSET', 'UTF8', "/DPAYLOAD_DEFINES=$definePath", "/DPAYLOAD_DIR=$payload", "/DSETUP_OUTPUT=$output", "/DAPP_ICON=$(Join-Path $root 'assets\app-icon.ico')")
if ($QaName) { $arguments += @("/DAPP_NAME=$QaName", "/DAPP_ID=$QaName") }
$arguments += (Join-Path $root 'packaging/installer.nsi')
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
    sourceSha256 = (Get-FileHash -LiteralPath (Join-Path $root 'packaging/installer.nsi') -Algorithm SHA256).Hash
    executableSha256 = $fileRecords[0].sha256; installedFiles = $fileRecords
    installer = $output; installerBytes = (Get-Item -LiteralPath $output).Length
    installerSha256 = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash
    installDirectory = '%LOCALAPPDATA%\Programs\' + $(if ($QaName) { $QaName } else { 'Compositor' })
    scope = 'current-user'; associationsCreated = $false
}
Write-ProjectJson $report (Join-Path $record "installer-$mode.json")
$report | ConvertTo-Json -Depth 8
}

# Former verify-package.ps1
function Test-ProjectPackage {
[CmdletBinding()]
param(
    [string]$RepositoryRoot,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')]
    [string]$PackageName = 'Compositor-Windows',
    [string]$ReportPath = 'build/validation/current/package-verification.json'
)
$ErrorActionPreference = 'Stop'

$root = Get-ProjectRoot $RepositoryRoot
$packageRelative = "dist/$PackageName"
$packageRoot = Resolve-ProjectPath $root $packageRelative
$archivePath = Resolve-ProjectPath $root "dist/$PackageName-x64.zip"
$manifest = Get-Content -LiteralPath (Join-Path $packageRoot 'package-files.json') -Raw | ConvertFrom-Json
$manifest = @($manifest)
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

$licenseResult = Invoke-ProjectPython -Name 'generate-open-source-licenses' -RepositoryRoot $root -Arguments @('--check', '--verify-exe', (Join-Path $packageRoot 'compositor.exe'), '--verify-package-index', (Join-Path $packageRoot 'LICENSES.md'))
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
}

# Former verify-windows-compatibility.ps1
function Test-ProjectWindowsCompatibility {
[CmdletBinding()]
param(
    [string]$ExePath = 'build\rust\release\compositor.exe',
    [string]$OutputDirectory = '',
    [ValidateRange(5, 120)]
    [int]$StartupTimeoutSeconds = 30,
    [ValidateRange(5, 60)]
    [int]$CloseTimeoutSeconds = 20,
    [switch]$KeepProcesses
)

# Run an isolated GUI smoke test for the two renderer paths that are shipped by
# the Windows build. This script is deliberately read-only with respect to the
# project and never uses the user's normal preferences directory.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = [IO.Path]::GetFullPath($PSScriptRoot)
$exe = if ([IO.Path]::IsPathRooted($ExePath)) {
    [IO.Path]::GetFullPath($ExePath)
} else {
    [IO.Path]::GetFullPath((Join-Path $root $ExePath))
}
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "Executable not found: $exe"
}
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $stamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
    $OutputDirectory = "build\validation\windows-compatibility-$stamp"
}
$out = if ([IO.Path]::IsPathRooted($OutputDirectory)) {
    [IO.Path]::GetFullPath($OutputDirectory)
} else {
    [IO.Path]::GetFullPath((Join-Path $root $OutputDirectory))
}
if (-not $out.StartsWith($root.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'OutputDirectory must remain inside this workspace.'
}
if (Test-Path -LiteralPath $out) {
    throw "Output directory already exists: $out"
}
[IO.Directory]::CreateDirectory($out) | Out-Null

function Write-Json([object]$Value, [string]$Path) {
    $Value | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $Path -Encoding utf8
}

function Get-HostRecord {
    $os = Get-CimInstance Win32_OperatingSystem
    $build = 0
    [void][int]::TryParse([string]$os.BuildNumber, [ref]$build)
    $family = if ($build -ge 22000) { 'Windows 11' } elseif ($build -ge 10240) { 'Windows 10' } else { 'Other' }
    [ordered]@{
        caption = [string]$os.Caption
        version = [string]$os.Version
        build = $build
        architecture = [string]$os.OSArchitecture
        familyByBuild = $family
        processArchitecture = if ([Environment]::Is64BitProcess) { 'x64' } else { 'x86' }
        osArchitectureIs64Bit = [Environment]::Is64BitOperatingSystem
        powershell = $PSVersionTable.PSVersion.ToString()
    }
}

function Wait-Until([scriptblock]$Condition, [int]$TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Invoke-RendererSmoke([string]$Name, [string]$Renderer) {
    $case = Join-Path $out $Name
    $data = Join-Path $case 'preferences'
    $smoke = Join-Path $case 'smoke'
    [IO.Directory]::CreateDirectory($data) | Out-Null
    [IO.Directory]::CreateDirectory($smoke) | Out-Null
    $ready = Join-Path $case 'ready.json'
    $stdout = Join-Path $case 'stdout.log'
    $stderr = Join-Path $case 'stderr.log'
    $envBackup = @{}
    foreach ($nameToSave in @('COMPOSITOR_DATA_DIR', 'COMPOSITOR_QA_READY_PATH', 'COMPOSITOR_SMOKE_OUTPUT_DIR', 'COMPOSITOR_RENDERER')) {
        $envBackup[$nameToSave] = [Environment]::GetEnvironmentVariable($nameToSave, 'Process')
    }
    $process = $null
    $forcedTermination = $false
    try {
        [Environment]::SetEnvironmentVariable('COMPOSITOR_DATA_DIR', $data, 'Process')
        [Environment]::SetEnvironmentVariable('COMPOSITOR_QA_READY_PATH', $ready, 'Process')
        [Environment]::SetEnvironmentVariable('COMPOSITOR_SMOKE_OUTPUT_DIR', $smoke, 'Process')
        if ([string]::IsNullOrEmpty($Renderer)) {
            [Environment]::SetEnvironmentVariable('COMPOSITOR_RENDERER', $null, 'Process')
        } else {
            [Environment]::SetEnvironmentVariable('COMPOSITOR_RENDERER', $Renderer, 'Process')
        }
        $process = Start-Process -FilePath $exe -ArgumentList '--smoke-test' -WorkingDirectory $root -WindowStyle Normal -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
        # Windows PowerShell 5.1 can otherwise lose ExitCode when the process
        # exits between refreshes. Retain the process handle before waiting.
        $null = $process.Handle
        $started = [DateTime]::UtcNow
        $null = Wait-Until { (Test-Path -LiteralPath $ready -PathType Leaf) -or $process.HasExited } $StartupTimeoutSeconds
        $null = Wait-Until { (Test-Path -LiteralPath (Join-Path $smoke 'gui-smoke.json') -PathType Leaf) -or $process.HasExited } $StartupTimeoutSeconds
        $readyObserved = Test-Path -LiteralPath $ready -PathType Leaf
        $smokeObserved = Test-Path -LiteralPath (Join-Path $smoke 'gui-smoke.json') -PathType Leaf
        $initiallyExited = $process.HasExited
        $closeRequested = $false
        $closed = $false
        $exitCode = $null
        if (-not $initiallyExited -and -not $KeepProcesses) {
            $closeRequested = $process.CloseMainWindow()
            $closed = $process.WaitForExit($CloseTimeoutSeconds * 1000)
            if (-not $closed -and -not $process.HasExited) {
                # Only terminate the process started by this invocation, and
                # report the forced cleanup as a failed graceful-close check.
                $process.Kill()
                $process.WaitForExit()
                $forcedTermination = $true
            }
        }
        if ($process.HasExited) { $exitCode = $process.ExitCode }
        $readyValue = if (Test-Path -LiteralPath $ready -PathType Leaf) { Get-Content -LiteralPath $ready -Raw -Encoding utf8 | ConvertFrom-Json } else { $null }
        $smokeValue = if (Test-Path -LiteralPath (Join-Path $smoke 'gui-smoke.json') -PathType Leaf) { Get-Content -LiteralPath (Join-Path $smoke 'gui-smoke.json') -Raw -Encoding utf8 | ConvertFrom-Json } else { $null }
        [ordered]@{
            renderer = if ([string]::IsNullOrEmpty($Renderer)) { 'auto' } else { $Renderer }
            pid = $process.Id
            elapsedMs = ([DateTime]::UtcNow - $started).TotalMilliseconds
            readyObserved = $readyObserved
            smokeObserved = $smokeObserved
            smokePassed = $null -ne $smokeValue -and $smokeValue.passed -eq $true
            adapter = if ($null -ne $readyValue) { $readyValue.adapter } else { $null }
            closeRequested = $closeRequested
            gracefulClose = $closed -and -not $forcedTermination
            forcedTermination = $forcedTermination
            exitCode = $exitCode
            dataDirectory = $data
            diagnosticsLog = Join-Path $data 'preferences\diagnostics.log'
        }
    } finally {
        if ($null -ne $process -and -not $KeepProcesses -and -not $process.HasExited) {
            $process.Kill()
            $process.WaitForExit()
        }
        foreach ($nameToRestore in $envBackup.Keys) {
            [Environment]::SetEnvironmentVariable($nameToRestore, $envBackup[$nameToRestore], 'Process')
        }
    }
}

$hostRecord = Get-HostRecord
$cases = @()
foreach ($caseDefinition in @(
    @{ Name = 'auto'; Renderer = '' },
    @{ Name = 'glow'; Renderer = 'glow' },
    @{ Name = 'wgpu'; Renderer = 'wgpu' }
)) {
    try {
        $cases += Invoke-RendererSmoke $caseDefinition.Name $caseDefinition.Renderer
    } catch {
        $cases += [ordered]@{
            renderer = $caseDefinition.Name
            readyObserved = $false
            smokeObserved = $false
            smokePassed = $false
            gracefulClose = $false
            forcedTermination = $false
            exitCode = $null
            error = $_.Exception.Message
        }
    }
}

$passed = [bool]$hostRecord.osArchitectureIs64Bit -and @($cases | Where-Object {
    $_.readyObserved -and $_.smokePassed -and $_.gracefulClose -and $_.exitCode -eq 0 -and -not $_.forcedTermination
}).Count -eq $cases.Count
$report = [ordered]@{
    schema = 'compositor-windows-compatibility/v1'
    checkedUtc = (Get-Date).ToUniversalTime().ToString('o')
    executable = [ordered]@{ path = $exe; bytes = (Get-Item -LiteralPath $exe).Length; sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash }
    host = $hostRecord
    cases = @($cases)
    passed = $passed
    limitations = @(
        'This script exercises the current host and cannot replace a real Windows 10 22H2 machine or VM.',
        'It does not emulate a missing D3D12/Vulkan driver; the glow case verifies the shipped OpenGL fallback path.',
        'Codec availability (HEIF/HEVC WIC) and vendor driver behavior remain host-dependent.'
    )
}
Write-Json $report (Join-Path $out 'report.json')
$report | ConvertTo-Json -Depth 12
if (-not $passed) { exit 1 }
}

# <PROJECT-PYTHON-BOOTSTRAP>
$script:ProjectPythonBootstrap = @'
"""Load the readable Python tools embedded in the single project.ps1 entry point."""
from pathlib import Path
import re
import sys
import types

PROJECT_ROOT = Path(sys.argv[1]).resolve()
PROJECT_SCRIPT = PROJECT_ROOT / "project.ps1"
SOURCES = dict(re.findall(r"(?ms)^# <PROJECT-PYTHON name=\"([^\"]+)\">\r?\n[ \t]*'[^']+' = @'\r?\n(.*?)\r?\n'@\r?\n# </PROJECT-PYTHON>", PROJECT_SCRIPT.read_text(encoding="utf-8-sig")))
MODULES = {}

def project_module(name, module_name=None):
    if name in MODULES:
        return MODULES[name]
    source = SOURCES[name]
    module = types.ModuleType(module_name or ("project_" + name.replace("-", "_")))
    module.__file__ = str(PROJECT_ROOT / (name + ".py"))
    module.project_module = project_module
    MODULES[name] = module
    sys.modules[module.__name__] = module
    exec(compile(source, str(PROJECT_SCRIPT) + "::" + name, "exec"), module.__dict__)
    return module

if __name__ == "__main__":
    tool = sys.argv[2]
    sys.argv = [str(PROJECT_SCRIPT) + " " + tool, *sys.argv[3:]]
    project_module(tool, "__main__")
'@
# </PROJECT-PYTHON-BOOTSTRAP>

# Python source is kept readable here; no helper scripts are shipped or cached.
$script:ProjectPythonSources = @{
# <PROJECT-PYTHON name="audit-runtime">
    'audit-runtime' = @'
"""Record PE imports of an EXE matching a completed All validation.

Inputs default to the current build validation directory. Each audit writes raw
PE evidence and a report copy into a fresh directory to retain earlier records.
"""
import argparse
from pathlib import Path
import datetime
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent
EXTERNAL_CRT = re.compile(
    r"^(?:(?:vcruntime|msvcp|msvcr|concrt)[^/\\]*|ucrtbased?|(?:api|ext)-ms-win-crt-[^/\\]*)\.dll$",
    re.I,
)

# These imports are documented as Windows 11-only entry points.  Keep this
# list intentionally small and explicit: ordinary Win10 APIs may be used by
# graphics/input backends and are accepted by the Windows 10 22H2 baseline.
WINDOWS_11_ONLY_IMPORTS = {
    "GetMachineTypeAttributes",
    "GetCurrentPackageInfo3",
    "GetCurrentPackagePath2",
    "GetKnownFolderPathEx",
    "GetWindowBand",
    "SetWindowBand",
}
WINDOWS_10_SUPPORTED_OS = "{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def identity(path):
    data = path.read_bytes()
    return {"path": str(path.resolve()), "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
            "modifiedUtc": datetime.datetime.fromtimestamp(path.stat().st_mtime, datetime.timezone.utc).isoformat()}


def report_path(path):
    return path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else str(path)


def parse_dependents(text):
    parts = re.split(r"following delay load dependencies\s*:", text, maxsplit=1, flags=re.I)
    descriptor = r"^\s+([A-Za-z0-9_.-]+\.dll)\s*$"
    direct = re.findall(descriptor, parts[0], flags=re.M | re.I)
    delayed = re.findall(descriptor, parts[1], flags=re.M | re.I) if len(parts) == 2 else []
    require(direct, "No direct DLL import descriptors found")
    return direct, delayed


def require_static_crt(direct, delayed):
    forbidden = sorted({name for name in direct + delayed if EXTERNAL_CRT.fullmatch(name)}, key=str.lower)
    require(not forbidden, "Release requires an external CRT DLL: " + ", ".join(forbidden))


def parse_imported_symbols(text):
    """Read named imports after dumpbin's hexadecimal hint/ordinal column.

    /IMPORTS emits lines such as ``0 GetProcAddress`` and ``A0 NtWriteFile``.
    Matching a symbol alone would silently miss every ordinary dumpbin line.
    Require parsed names so an unexpected tool format fails the audit closed.
    """
    symbols = set(re.findall(
        r"^\s+[0-9A-Fa-f]+\s+([A-Za-z_?@][A-Za-z0-9_?@$]*)\s*$", text, re.M,
    ))
    require(symbols, "No named PE imports found in dumpbin /IMPORTS output")
    return symbols


def forbidden_windows11_imports(text):
    return sorted(WINDOWS_11_ONLY_IMPORTS & parse_imported_symbols(text))


def find_dumpbin(selected):
    if selected:
        return selected.resolve(strict=True)
    installed = shutil.which("dumpbin")
    if installed:
        return Path(installed).resolve(strict=True)
    vswhere = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
    if vswhere.is_file():
        result = subprocess.run([str(vswhere), "-latest", "-products", "*", "-requires",
                                 "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-find",
                                 "VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe"],
                                capture_output=True, text=True, check=True)
        candidates = sorted(Path(line) for line in result.stdout.splitlines() if line.strip())
        if candidates:
            return candidates[-1].resolve(strict=True)
    raise ValueError("dumpbin.exe was not found; use --dumpbin PATH or a Visual Studio developer shell")


def find_mt(selected):
    """Find the Windows SDK manifest tool used by the PE resource gate."""
    if selected:
        return selected.resolve(strict=True)
    installed = shutil.which("mt") or shutil.which("mt.exe")
    if installed:
        return Path(installed).resolve(strict=True)
    candidates = []
    if os.environ.get("WindowsSdkVerBinPath"):
        candidates.append(Path(os.environ["WindowsSdkVerBinPath"]) / "x64" / "mt.exe")
    sdk = os.environ.get("WindowsSdkDir")
    if sdk:
        candidates.extend(Path(sdk).glob("bin/*/x64/mt.exe"))
    program_files_x86 = os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")
    candidates.extend(Path(program_files_x86).glob("Windows Kits/10/bin/*/x64/mt.exe"))
    existing = sorted((path for path in candidates if path.is_file()), key=lambda path: str(path))
    if existing:
        return existing[-1].resolve(strict=True)
    raise ValueError("mt.exe was not found; use --mt PATH or install the Windows SDK")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "build/rust/release/compositor.exe")
    parser.add_argument("--validation", type=Path, default=ROOT / "build/validation/current/build-all.json")
    parser.add_argument("--validation-log", type=Path, default=ROOT / "build/validation/current/all.log")
    parser.add_argument("--evidence-dir", type=Path, help="Fresh directory for raw evidence and an audit report copy")
    parser.add_argument("--dumpbin", type=Path, help="Optional dumpbin.exe path; otherwise detect the installed Build Tools")
    parser.add_argument("--mt", type=Path, help="Optional mt.exe path; otherwise detect the installed Windows SDK")
    return parser.parse_args(argv)


def verify_validation(validation, validation_log, exe):
    record = json.loads(validation.read_text(encoding="utf-8-sig"))
    require(record.get("action") == "All" and record.get("completed") is True,
            "Expected a completed All validation")
    steps = record.get("steps", [])
    require(len(steps) == 5, "Expected all five validation steps")
    log = validation_log.read_text(encoding="utf-8-sig")
    for step, gate in zip(steps, ("fmt", "check", "clippy", "test", "build")):
        tokens = shlex.split(step.get("command", ""), posix=False)
        require(type(step.get("exitCode")) is int and step["exitCode"] == 0
                and tokens[:2] == ["cargo", gate], f"Missing or failed validation step: {gate}")
        require(step["command"] in log, f"Validation log does not contain the {gate} command")
        if gate == "fmt":
            require("--check" in tokens, "Formatting was not checked")
        else:
            require("--locked" in tokens, f"Validation step was not locked: {gate}")
        if gate in ("check", "clippy"):
            require("--all-targets" in tokens, f"Validation did not cover all targets: {gate}")
        if gate == "clippy":
            require("--" in tokens and tokens[tokens.index("--") + 1:] == ["-D", "warnings"],
                    "Clippy did not deny warnings")
        if gate == "build":
            require("--release" in tokens and "--bin" in tokens
                    and tokens[tokens.index("--bin") + 1:tokens.index("--bin") + 2] == ["compositor"],
                    "Validation did not build the compositor Release binary")
    ended = datetime.datetime.fromisoformat(record["endedUtc"].replace("Z", "+00:00"))
    started = datetime.datetime.fromisoformat(record["startedUtc"].replace("Z", "+00:00"))
    require(started.tzinfo is not None and ended.tzinfo is not None and ended >= started,
            "Validation timestamps must be ordered and timezone-aware")
    target = (Path(record["targetDirectory"]).resolve(strict=True) / "release/compositor.exe").resolve(strict=True)
    target_identity, exe_identity = identity(target), identity(exe)
    require(target_identity["bytes"] == exe_identity["bytes"] and target_identity["sha256"] == exe_identity["sha256"],
            "Selected EXE differs from the current validated target")
    require(target.stat().st_mtime <= ended.timestamp() + 1,
            "Target EXE was modified after the validation completed")
    hash_fields = []
    for field in ("executable", "inputExecutable"):
        entry = record.get(field)
        if isinstance(entry, dict) and "sha256" in entry:
            require(entry["sha256"].lower() == exe_identity["sha256"], f"Validation {field} SHA256 mismatch")
            for size in ("bytes", "length"):
                require(size not in entry or entry[size] == exe_identity["bytes"], f"Validation {field} size mismatch")
            hash_fields.append(field)
    if "executableSha256" in record:
        require(record["executableSha256"].lower() == exe_identity["sha256"], "Validation executableSha256 mismatch")
        hash_fields.append("executableSha256")
    return {"completedVerified": True, "allFiveStepsPassed": True, "recordedCommandsPresentInLog": True,
            "executableMatchesCurrentValidatedTarget": True, "targetMtimeNoLaterThanValidationEnd": True,
            "mtimeToleranceSeconds": 1, "validatedTargetExecutable": target_identity,
            "validationExecutableHashFields": hash_fields,
            "hashMatchScope": "Selected EXE equals the current targetDirectory/release/compositor.exe by bytes and SHA256; target mtime is no later than validation completion plus one second.",
            "limitations": ["Legacy All records have no build-time EXE hash or cryptographic link to the text log. Current-target equality and mtime do not retroactively establish build-time hash or source-to-binary provenance."]}, target, exe_identity


def main(argv=None):
    args = parse_args(argv)
    dumpbin = find_dumpbin(args.dumpbin)
    mt = find_mt(args.mt)
    exe, validation, validation_log = (path.resolve(strict=True) for path in
                                      (args.exe, args.validation, args.validation_log))
    validation_identity, log_identity = identity(validation), identity(validation_log)
    verification, target, exe_identity = verify_validation(validation, validation_log, exe)
    target_identity = identity(target)
    previous_path = ROOT / "packaging/runtime-imports.json"
    previous_identity, tool_identity = identity(previous_path), identity(dumpbin)
    evidence_dir = args.evidence_dir or ROOT / "build/validation" / (
        "runtime-audit-" + datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ"))
    evidence_dir = evidence_dir.resolve()
    evidence_dir.mkdir(parents=True, exist_ok=False)
    evidence, evidence_files = {}, {}
    for key, switch in (("dependents", "/DEPENDENTS"), ("headers", "/HEADERS"),
                        ("imports", "/IMPORTS"), ("vcruntime140Imports", "/IMPORTS:VCRUNTIME140.dll")):
        output = subprocess.run([str(dumpbin), switch, str(exe)], capture_output=True, check=True)
        evidence[key] = output.stdout.decode("utf-8", "replace")
        path = evidence_dir / f"pe-{key}.txt"
        with path.open("x", encoding="utf-8", newline="\n") as stream:
            stream.write(evidence[key])
        evidence_files[key] = identity(path)
    descriptors, delayed_descriptors = parse_dependents(evidence["dependents"])
    require_static_crt(descriptors, delayed_descriptors)
    imported_symbols = parse_imported_symbols(evidence["imports"])
    forbidden_imports = forbidden_windows11_imports(evidence["imports"])
    require(not forbidden_imports,
            "Release imports Windows 11-only APIs: " + ", ".join(forbidden_imports))
    headers = evidence["headers"]
    for required in ("PE32+", "machine (x64)", "Windows GUI", "High Entropy Virtual Addresses", "Dynamic base", "NX compatible"):
        require(required in headers, f"PE header validation failed: {required}")
    require(identity(exe) == exe_identity and identity(target) == target_identity,
            "EXE or target changed during dumpbin")
    require(identity(validation) == validation_identity and identity(validation_log) == log_identity,
            "Validation record/log changed during dumpbin")
    require(identity(previous_path) == previous_identity and identity(dumpbin) == tool_identity,
            "Previous audit or dumpbin changed during audit")
    manifest_output = evidence_dir / "app-manifest.xml"
    manifest_result = subprocess.run(
        [str(mt), f"-inputresource:{exe};#1", f"-out:{manifest_output}"],
        capture_output=True,
    )
    require(manifest_result.returncode == 0 and manifest_output.is_file(),
            "The Release executable has no readable RT_MANIFEST resource")
    manifest_raw = manifest_output.read_bytes()
    try:
        manifest_text = manifest_raw.decode("utf-8-sig")
    except UnicodeDecodeError:
        manifest_text = manifest_raw.decode("utf-16")
    manifest_lower = manifest_text.lower()
    require(WINDOWS_10_SUPPORTED_OS.lower() in manifest_lower,
            "The Release manifest does not declare Windows 10/11 compatibility")
    require('level="asInvoker"'.lower() in manifest_lower,
            "The Release manifest must request asInvoker execution")
    require("permonitorv2" in manifest_lower,
            "The Release manifest must declare PerMonitorV2 DPI awareness")
    manifest_evidence = identity(manifest_output)
    evidence_files["manifest"] = manifest_evidence
    evidence["manifest"] = manifest_text
    verification["executableAndValidationStableAcrossDumpbin"] = True
    result = {
        "schemaVersion": 1,
        "scope": "Direct and delay-loaded PE imports of this optimized Windows x64 Release; dynamically loaded drivers and WIC codecs are not exhaustively enumerated.",
        "uiLanguage": "zh-CN", "checkedUtc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "executable": {"path": str(exe), "length": exe_identity["bytes"],
                       "modifiedUtc": datetime.datetime.fromtimestamp(exe.stat().st_mtime, datetime.timezone.utc).isoformat(),
                       "sha256": exe_identity["sha256"]},
        "auditTool": {**tool_identity, "versionEvidence": headers.splitlines()[0]},
        "manifestTool": identity(mt),
        "pe": {"format": "PE32+", "machine": "x64", "subsystem": "Windows GUI",
               "highEntropyVirtualAddresses": True, "dynamicBase": True, "nxCompatible": True},
        "directImportDescriptorCount": len(descriptors), "directImportDescriptors": descriptors,
        "delayImportDescriptorCount": len(delayed_descriptors), "delayImportDescriptors": delayed_descriptors,
        "windows10ImportPolicy": {
            "target": "Windows 10 22H2 x64 and Windows 11 x64",
            "parsedNamedImportCount": len(imported_symbols),
            "symbolEvidence": "dumpbin /IMPORTS hexadecimal hint followed by exact symbol name",
            "forbiddenWindows11OnlyImports": sorted(WINDOWS_11_ONLY_IMPORTS),
            "forbiddenImportsFound": forbidden_imports,
            "passed": not forbidden_imports,
        },
        "caseInsensitiveDistinctDllCount": len({name.lower() for name in descriptors + delayed_descriptors}),
        "caseInsensitiveDistinctDlls": sorted({name.lower() for name in descriptors + delayed_descriptors}),
        "staticCrtVerified": True,
        "manifest": {
            "windows10CompatibilityId": WINDOWS_10_SUPPORTED_OS,
            "perMonitorV2": True,
            "requestedExecutionLevel": "asInvoker",
            "sha256": manifest_evidence["sha256"],
        },
        "requirements": {
            "targetWindows": "Windows 10/11 x64",
            "graphics": "A graphics driver supporting Direct3D 12 or Vulkan; backend dependencies can load dynamically.",
            "cRuntime": "The MSVC and Universal C runtimes are statically linked. No separate Visual C++ Redistributable installation is required.",
            "runtimePolicy": "Direct and delay-loaded VCRUNTIME, MSVCP, MSVCR, CONCRT, UCRTBASE and api/ext-ms-win-crt DLL imports are rejected.",
            "heic": "A compatible installed Windows HEIF WIC codec is needed to decode HEIC/HEIF.",
            "separateOnnxRuntimeDllRequired": False,
            "hostRuntimeVersionMeaning": "Observed installed host versions only; they are not declared minimum supported versions.",
            "bundledSystemDlls": False,
        },
        "buildValidation": report_path(validation),
        "buildValidationLog": report_path(validation_log), "buildValidationRecordIdentity": validation_identity,
        "buildValidationLogIdentity": log_identity, "buildValidationVerification": verification,
        "previousChineseAudit": report_path(previous_path), "previousChineseAuditIdentity": previous_identity,
        "rawEvidence": evidence,
        "rawEvidenceFiles": evidence_files, "evidenceDirectory": str(evidence_dir),
    }
    text = json.dumps(result, indent=2) + "\n"
    with (evidence_dir / "runtime-imports.json").open("x", encoding="utf-8", newline="\n") as stream:
        stream.write(text)
    (ROOT / "packaging/runtime-imports.json").write_text(text, encoding="utf-8")
    print(json.dumps({"bytes": exe_identity["bytes"], "sha256": exe_identity["sha256"],
                      "importDescriptors": descriptors, "evidenceDirectory": str(evidence_dir)}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
'@
# </PROJECT-PYTHON>
# <PROJECT-PYTHON name="generate-icon">
    'generate-icon' = @'
"""Rebuild the Windows icon from the original Compositor artwork (requires Pillow)."""

from io import BytesIO
from pathlib import Path
import struct

from PIL import Image


def main():
    root = Path(__file__).resolve().parent
    source_path = root / "assets" / "app-icon-1024.png"
    output = root / "assets" / "app-icon.ico"
    sizes = (16, 24, 32, 48, 64, 128, 256)
    with Image.open(source_path) as original:
        if original.size != (1024, 1024):
            raise ValueError("The original artwork must be 1024 by 1024 pixels.")
        source = original.convert("RGBA")
        frames = []
        for size in sizes:
            frame = source.resize((size, size), Image.Resampling.LANCZOS)
            encoded = BytesIO()
            frame.save(encoded, format="PNG", compress_level=9)
            frames.append(encoded.getvalue())

    header = struct.pack("<HHH", 0, 1, len(sizes))
    offset = len(header) + 16 * len(sizes)
    directory = bytearray()
    for size, frame in zip(sizes, frames):
        dimension = size if size < 256 else 0
        directory.extend(
            struct.pack("<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(frame), offset)
        )
        offset += len(frame)
    encoded_icon = header + directory + b"".join(frames)
    with Image.open(BytesIO(encoded_icon)) as icon:
        if icon.ico.sizes() != {(size, size) for size in sizes}:
            raise ValueError("Icon frame sizes failed verification.")
        for size in sizes:
            if icon.ico.getimage((size, size)).getchannel("A").getextrema() != (0, 255):
                raise ValueError(f"Icon frame {size} is missing transparency.")
    output.write_bytes(encoded_icon)
    print(f"{output}: {len(encoded_icon)} bytes; RGBA sizes {sizes}")


if __name__ == "__main__":
    main()
'@
# </PROJECT-PYTHON>
# <PROJECT-PYTHON name="generate-open-source-licenses">
    'generate-open-source-licenses' = @'
"""Generate and audit the application's compressed, byte-exact license resource."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import tomllib
from urllib.parse import quote, urlsplit
import zlib

ROOT = Path(__file__).resolve().parent
ASSET = ROOT / "assets/open-source-licenses.bin"
MAGIC = b"COMPOSITOR-OPEN-SOURCE-LICENSES\0v1\0"
MAX_PART_BYTES = 16 * 1024 * 1024
BUNDLE = project_module("package-licenses")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_ranges(raw, originals):
    ranges, cursor = [], 0
    while (start := raw.find(BUNDLE.SECTION, cursor)) >= 0:
        start += len(BUNDLE.SECTION)
        end = raw.index(b" -->\n", start)
        record = json.loads(raw[start:end])
        begin = end + len(b" -->\n") + len(f"{record['fence']}text\n".encode())
        finish = begin + record["bytes"]
        require(raw[begin:finish] == originals[record["path"]], "Original range differs")
        ranges.append({"path": record["path"], "offset": begin, "bytes": record["bytes"], "sha256": record["sha256"]})
        cursor = finish + len(f"\n{record['fence']}\n".encode())
    require(len(ranges) == len(originals), "Original range count differs")
    return ranges


def https_url(value):
    parsed = urlsplit(value)
    require(parsed.scheme == "https" and parsed.netloc and not parsed.username, f"Expected an HTTPS license link: {value}")
    return value


def component(name, version, license_expression, repository, sources, license_links):
    require(bool(license_links), f"No online license link for {name}")
    return {"name": name, "version": version, "license": license_expression,
            "repository": repository or "", "url": https_url(license_links[0]["url"]),
            "licenseLinks": license_links, "originals": sources}


def spdx_links(expression):
    tokens = re.findall(r"[A-Za-z0-9][A-Za-z0-9.+-]*", expression)
    links, seen = [], set()
    for token in tokens:
        if token in {"AND", "OR", "WITH"}:
            continue
        if token not in seen:
            seen.add(token)
            # SPDX publishes both license and exception pages under /licenses/.
            links.append({"name": token, "url": https_url(f"https://spdx.org/licenses/{quote(token, safe='-.+')}.html")})
    return links


def generate(source_bundle):
    summary, originals = BUNDLE.verify_bundle(source_bundle)
    raw = source_bundle.read_bytes()
    require(len(raw) <= MAX_PART_BYTES, "The license source exceeds the bounded embedded format")
    ranges = source_ranges(raw, originals)
    identities = {entry["path"]: index for index, entry in enumerate(ranges)}
    package = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    version = package["version"]
    require(package["license"] == "Apache-2.0", "The application license must match the Apache-2.0 declaration")
    require(originals.get("CompositorWin-LICENSE.md") == (ROOT / "LICENSE.md").read_bytes(), "The bundled application license differs from LICENSE.md")
    components = [
        component("CompositorWin", version, package["license"], "https://github.com/shuguang-liming/CompositorWin", [identities["CompositorWin-LICENSE.md"]], [{"name": "Apache-2.0", "url": "https://www.apache.org/licenses/LICENSE-2.0"}]),
        component("Compositor (original project)", "", "MIT", "https://github.com/robbietilton/Compositor", [identities["LICENSE"]], [{"name": "MIT", "url": "https://spdx.org/licenses/MIT.html"}]),
        component("U²-Netp model", "", "Apache-2.0", "https://github.com/xuebinqin/U-2-Net", [identities["LICENSE-U2NET.txt"]], [{"name": "Apache-2.0", "url": "https://spdx.org/licenses/Apache-2.0.html"}]),
    ]
    nsis = "installer-licenses/NSIS-3.11-COPYING.txt"
    if nsis in identities:
        components.append(component("NSIS", "3.11", "zlib/libpng, bzip2, CPL-1.0 and included notices", "https://nsis.sourceforge.io/", [identities[nsis]], [{"name": "NSIS licenses", "url": "https://nsis.sourceforge.io/License"}]))
    for notice in BUNDLE.dependency_notices(originals["THIRD-PARTY-NOTICES.json"]):
        # The original bundle also preserves an older self-package notice.
        # Keep its bytes intact, while the active application row uses Cargo.
        if notice["name"] == package["name"]:
            continue
        prefix = f"third-party-licenses/{notice['name']}-{notice['version']}-"
        sources = [index for path, index in identities.items() if path.startswith(prefix)]
        require(bool(sources), f"Missing actual license original for {notice['name']} {notice['version']}; metadata is not a license text")
        require(any(re.search(r"licen[sc]e|copying|copyright|notice|(?:^|[-_])(?:OFL|UFL)(?:[.-]|$)", ranges[source]["path"][len(prefix):], re.I) for source in sources),
                f"Missing actual license text for {notice['name']} {notice['version']}; declarations alone do not suffice")
        links = spdx_links(notice["license"])
        components.append(component(notice["name"], notice["version"], notice["license"], notice["repository"], sources, links))
    index = {"formatVersion": 1, "bundleBytes": len(raw), "bundleSha256": digest(raw),
             "dependencyNotices": summary["dependencyNotices"], "licenseTexts": summary["licenseTexts"],
             "components": components, "originals": ranges}
    index_raw = (json.dumps(index, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
    require(len(index_raw) <= MAX_PART_BYTES, "The license index exceeds the bounded embedded format")
    index_compressed, raw_compressed = zlib.compress(index_raw, 9), zlib.compress(raw, 9)
    header = struct.pack("<IIII", len(index_compressed), len(index_raw), len(raw_compressed), len(raw))
    return MAGIC + header + index_compressed + raw_compressed, index, index_raw, originals


def unpack(resource):
    require(resource.startswith(MAGIC), "Missing embedded license resource identity")
    start = len(MAGIC)
    a, b, c, d = struct.unpack_from("<IIII", resource, start)
    start += 16
    require(max(b, d) <= MAX_PART_BYTES and len(resource) == start + a + c, "Invalid embedded resource lengths")
    index_raw = zlib.decompress(resource[start:start + a])
    bundle_raw = zlib.decompress(resource[start + a:])
    require(len(index_raw) == b and len(bundle_raw) == d, "Embedded decoded lengths differ")
    index = json.loads(index_raw)
    require(index["bundleBytes"] == d and index["bundleSha256"] == digest(bundle_raw), "Embedded source identity differs")
    return index_raw, bundle_raw


def package_index(index):
    return (
        "# 开源软件许可证\n\n"
        "在 CompositorWin 中打开“帮助 → 开源软件许可证”。点击组件名称可在线查看许可证，"
        "选择“随附原文”可离线查看该组件的完整许可和版权声明。\n\n"
        "完整原文已压缩内嵌在 compositor.exe 中，按需解压，不需要另外附带大段文本。"
        "压缩不改变原始字节或许可条款；各组件仍分别适用自己的许可证。\n\n"
        "- CompositorWin 主程序：Apache-2.0，https://www.apache.org/licenses/LICENSE-2.0\n"
        "- 原 Compositor 项目：MIT，https://spdx.org/licenses/MIT.html\n"
        "- 原项目：https://github.com/robbietilton/Compositor\n"
        "- U²-Net 模型：Apache-2.0，https://spdx.org/licenses/Apache-2.0.html\n"
        "- 模型项目：https://github.com/xuebinqin/U-2-Net\n"
        "- NSIS：https://nsis.sourceforge.io/License\n\n"
        f"随附 {index['dependencyNotices']} 项组件声明和 {index['licenseTexts']} 份许可原文。\n"
        f"完整原文 SHA256：`{index['bundleSha256']}`。\n"
    ).encode("utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Read-only exact regeneration check; do not write the resource")
    parser.add_argument("--verify-exe", type=Path, help="Require the executable to contain the exact checked resource bytes")
    parser.add_argument("--nsis-copying", type=Path, help="Require the bundle to preserve this compiler's exact COPYING bytes")
    parser.add_argument("--write-package-index", type=Path, help="Exclusively create the small release LICENSES.md entry point")
    parser.add_argument("--verify-package-index", type=Path, help="Read-only exact check of the small release LICENSES.md entry point")
    args = parser.parse_args()
    expected, index, index_raw, originals = generate(ROOT / "LICENSES.md")
    verify_only = args.check or args.verify_exe is not None or args.verify_package_index is not None
    if verify_only:
        require(ASSET.read_bytes() == expected, "Embedded licenses are stale; run project.ps1 GenerateLicenses")
    else:
        ASSET.write_bytes(expected)
    restored_index, restored_bundle = unpack(ASSET.read_bytes())
    require(restored_index == index_raw and restored_bundle == (ROOT / "LICENSES.md").read_bytes(), "Embedded originals differ byte-for-byte")
    if args.nsis_copying is not None:
        require(originals.get("installer-licenses/NSIS-3.11-COPYING.txt") == args.nsis_copying.read_bytes(), "NSIS compiler COPYING differs from the embedded original")
    executable_offset = None
    if args.verify_exe is not None:
        executable = args.verify_exe.read_bytes()
        executable_offset = executable.find(expected)
        require(executable_offset >= 0, "The executable does not contain the exact verified license resource")
        recovered_index, recovered_bundle = unpack(executable[executable_offset:executable_offset + len(expected)])
        require(recovered_index == index_raw and recovered_bundle == restored_bundle, "Executable license metadata or originals differ")
    small_index = package_index(index)
    if args.write_package_index is not None:
        with args.write_package_index.open("xb") as stream:
            stream.write(small_index)
    if args.verify_package_index is not None:
        require(args.verify_package_index.read_bytes() == small_index, "Packaged license entry point differs")
    print(json.dumps({"status": "passed", "dependencyNotices": index["dependencyNotices"], "licenseTexts": index["licenseTexts"],
                      "bundleBytes": index["bundleBytes"], "bundleSha256": index["bundleSha256"], "indexSha256": digest(index_raw),
                      "resourceBytes": len(expected), "resourceSha256": digest(expected), "packageIndexBytes": len(small_index),
                      "executableOffset": executable_offset}, ensure_ascii=False))


if __name__ == "__main__":
    main()
'@
# </PROJECT-PYTHON>
# <PROJECT-PYTHON name="package-licenses">
    'package-licenses' = @'
"""Combine release license originals and dependency notices into one Markdown file."""
import argparse
import hashlib
import json
from pathlib import Path
import re


SUMMARY = b"<!-- LICENSES-SUMMARY "
SECTION = b"<!-- LICENSES-SOURCE "


def require(condition, message):
    if not condition:
        raise ValueError(message)


def dependency_notices(raw):
    notices = json.loads(raw.decode("utf-8-sig"))
    require(isinstance(notices, list) and notices, "Dependency notices are missing")
    identities = set()
    for notice in notices:
        require(isinstance(notice, dict), "Invalid dependency notice")
        identity = (notice.get("name"), notice.get("version"))
        require(all(isinstance(value, str) and value for value in identity), "Missing component identity")
        require(identity not in identities, f"Duplicate dependency identity: {identity}")
        identities.add(identity)
    return notices


def append_source(output, name, raw):
    raw.decode("utf-8")  # Fail rather than silently alter an unsupported original.
    runs = [len(match[0]) for match in re.finditer(rb"`+", raw)]
    fence = "`" * max(3, max(runs, default=0) + 1)
    record = {"path": name, "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest(), "fence": fence}
    output.extend(f"\n## {name}\n\n".encode("utf-8"))
    output.extend(SECTION + json.dumps(record, ensure_ascii=False).encode("utf-8") + b" -->\n")
    output.extend(f"{fence}text\n".encode("ascii"))
    output.extend(raw)
    output.extend(f"\n{fence}\n".encode("ascii"))


def verify_bundle(path):
    data = path.read_bytes()
    data.decode("utf-8")
    summary_start = data.index(SUMMARY) + len(SUMMARY)
    summary_end = data.index(b" -->\n", summary_start)
    summary = json.loads(data[summary_start:summary_end])
    require(summary.get("schemaVersion") == 1, "Unsupported license bundle")
    originals, cursor = {}, summary_end
    while (start := data.find(SECTION, cursor)) >= 0:
        start += len(SECTION)
        end = data.index(b" -->\n", start)
        record = json.loads(data[start:end])
        name = record["path"]
        require(name not in originals, f"Duplicate bundled source: {name}")
        opening = f"{record['fence']}text\n".encode("ascii")
        begin = end + len(b" -->\n")
        require(data[begin:begin + len(opening)] == opening, f"Missing text fence: {name}")
        begin += len(opening)
        finish = begin + record["bytes"]
        raw = data[begin:finish]
        require(len(raw) == record["bytes"] and hashlib.sha256(raw).hexdigest() == record["sha256"],
                f"Bundled source size or hash differs: {name}")
        closing = f"\n{record['fence']}\n".encode("ascii")
        require(data[finish:finish + len(closing)] == closing, f"Missing closing fence: {name}")
        originals[name] = raw
        cursor = finish + len(closing)
    require({"LICENSE", "LICENSE-U2NET.txt", "THIRD-PARTY-NOTICES.json"} <= originals.keys(),
            "Application, model or dependency notices are missing")
    require(len(originals) == summary["licenseTexts"] + 1, "License text count differs")
    require(any(name.startswith("third-party-licenses/") for name in originals), "Third-party texts are missing")
    notices = dependency_notices(originals["THIRD-PARTY-NOTICES.json"])
    require(len(notices) == summary["dependencyNotices"], "Dependency notice count differs")
    return summary, originals


def create_bundle(args):
    _, source_originals = verify_bundle(args.source_bundle)
    files = sorted(args.license_dir.iterdir(), key=lambda path: path.name)
    require(files and all(path.is_file() and not path.is_symlink() for path in files),
            "Expected a nonempty directory of license files")
    originals = {
        "LICENSE": source_originals["LICENSE"],
        "LICENSE-U2NET.txt": source_originals["LICENSE-U2NET.txt"],
        "THIRD-PARTY-NOTICES.json": args.notices.read_bytes(),
        "CompositorWin-LICENSE.md": (Path(__file__).resolve().parent / "LICENSE.md").read_bytes(),
    }
    # Installer stubs/plugins accompany the app, outside the Cargo dependency graph.
    originals.update({name: raw for name, raw in source_originals.items()
                      if name.startswith("installer-licenses/")})
    originals.update({f"third-party-licenses/{path.name}": path.read_bytes() for path in files})
    notices = dependency_notices(originals["THIRD-PARTY-NOTICES.json"])
    summary = {"schemaVersion": 1, "dependencyNotices": len(notices), "licenseTexts": len(originals) - 1}
    header = (
        "# Compositor Windows 许可证与第三方声明\n\n"
        "本文件汇集应用、模型、第三方依赖和字体的原始许可证、版权声明及组件来源。"
        "各组件适用各自许可证；合并文件不改变任何许可条款。\n\n"
        "CompositorWin 主程序及 Windows 移植新增代码采用 Apache License 2.0，"
        "完整条款同时见根目录 LICENSE.md 和下方 CompositorWin-LICENSE.md。"
        "原 Compositor 项目（Copyright (c) 2026 Wonder Assembly LLC）保留 MIT 许可，"
        "其原文见下方 LICENSE；模型、依赖与字体分别保留各自许可。\n\n"
        "下方原文保留原始字节和换行，每段标注原文件名、大小及 SHA256。"
        "使用编辑器搜索组件名或文件名即可定位。\n\n"
        "原项目：https://github.com/robbietilton/Compositor\n\n"
        "模型来源：https://github.com/danielgatis/rembg/releases/download/v0.0.0/u2netp.onnx\n\n"
        "原模型项目：https://github.com/xuebinqin/U-2-Net\n\n"
    )
    output = bytearray(header.encode("utf-8"))
    output.extend(SUMMARY + json.dumps(summary).encode("ascii") + b" -->\n")
    for name, raw in originals.items():
        append_source(output, name, raw)
    with args.output.open("xb") as stream:
        stream.write(output)
    observed, restored = verify_bundle(args.output)
    require(restored == originals, "The combined file differs from the source originals")
    return observed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    create.add_argument("--source-bundle", type=Path, required=True)
    create.add_argument("--notices", type=Path, required=True)
    create.add_argument("--license-dir", type=Path, required=True)
    create.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("path", type=Path)
    args = parser.parse_args()
    summary = create_bundle(args) if args.command == "create" else verify_bundle(args.path)[0]
    print(json.dumps({"status": "passed", **summary}))


if __name__ == "__main__":
    main()
'@
# </PROJECT-PYTHON>
# <PROJECT-PYTHON name="package-zip">
    'package-zip' = @'
"""Create and verify a new release ZIP with Python 3.11+ and deflate level 9.

The package manifest excludes itself. Its actual bytes are added to the verified
inventory, so every ZIP file, including package-files.json, is checked by SHA256.
Existing output files are retained. Failed partial archives remain as evidence.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import sys
import time
import traceback
import zipfile
import zlib


def require(condition, message):
    if not condition:
        raise ValueError(message)


def absolute(path):
    # Inspect lexical paths before resolving anything that might be a junction.
    return Path(os.path.abspath(path))


def check_plain_path(path):
    for component in (*reversed(path.parents), path):
        try:
            attributes = component.lstat()
        except FileNotFoundError:
            continue
        require(not stat.S_ISLNK(attributes.st_mode) and
                not getattr(attributes, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT,
                f"Reparse points and symbolic links are excluded: {component}")


def require_new(path):
    check_plain_path(path)
    require(not os.path.lexists(path), f"Existing output is retained: {path}")


def member_name(value):
    require(isinstance(value, str) and value, "Expected a nonempty relative package path")
    name = value.replace("\\", "/")
    require(not name.startswith("/") and not re.match(r"^[A-Za-z]:", name), f"Rooted package path: {value}")
    for component in name.split("/"):
        require(component not in ("", ".", "..") and not re.search(r'[<>:"|?*\x00-\x1f]', component)
                and not component.endswith((".", " "))
                and not re.match(r"^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)", component, re.I),
                f"Unsafe package path component: {value}")
    return name


def stream_digest(stream):
    checksum, length = hashlib.sha256(), 0
    while data := stream.read(1024 * 1024):
        checksum.update(data)
        length += len(data)
    return {"bytes": length, "sha256": checksum.hexdigest()}


def file_identity(path):
    with path.open("rb") as stream:
        return {"path": str(absolute(path)), **stream_digest(stream)}


def inventory_package(package):
    check_plain_path(package)
    require(package.is_dir(), f"Missing package directory: {package}")
    actual, folded = {}, set()
    for directory, children, files in os.walk(package, followlinks=False):
        for name in (*children, *files):
            entry = Path(directory) / name
            check_plain_path(entry)
            require(entry.is_dir() or stat.S_ISREG(entry.stat().st_mode), f"Non-regular package entry: {entry}")
        for name in files:
            path = Path(directory) / name
            relative = member_name(path.relative_to(package).as_posix())
            require(relative.casefold() not in folded, f"Duplicate package path: {relative}")
            folded.add(relative.casefold())
            actual[relative] = {key: value for key, value in file_identity(path).items() if key != "path"}
    manifest_path = package / "package-files.json"
    require("package-files.json" in actual, "The package manifest is missing")
    raw_manifest = manifest_path.read_bytes()
    require(hashlib.sha256(raw_manifest).hexdigest() == actual["package-files.json"]["sha256"],
            "Package manifest changed while reading")
    manifest = json.loads(raw_manifest.decode("utf-8-sig"))
    require(isinstance(manifest, list) and manifest, "The package manifest must be a nonempty array")
    expected, folded = {}, set()
    for record in manifest:
        require(isinstance(record, dict), "Package manifest entries must be objects")
        relative = member_name(record["path"])
        require(relative.casefold() != "package-files.json", "The manifest must exclude its own self-referential hash")
        require(relative.casefold() not in folded, f"Duplicate manifest entry: {relative}")
        folded.add(relative.casefold())
        require(isinstance(record["bytes"], int) and not isinstance(record["bytes"], bool) and record["bytes"] >= 0
                and isinstance(record["sha256"], str) and re.fullmatch(r"[a-fA-F0-9]{64}", record["sha256"]),
                f"Invalid manifest size or SHA256: {relative}")
        expected[relative] = {"bytes": record["bytes"], "sha256": record["sha256"].lower()}
    expected["package-files.json"] = actual["package-files.json"]
    require(actual == expected, "Package files, lengths or hashes differ from the manifest (including its own bytes)")
    return dict(sorted(expected.items()))


def verify_archive(archive_path, expected):
    checked, folded = {}, set()
    directories = {parent.as_posix() for name in expected for parent in Path(name).parents if parent.as_posix() != "."}
    with zipfile.ZipFile(archive_path, "r") as archive:
        for entry in archive.infolist():
            normalized = entry.filename.replace("\\", "/")
            directory = normalized.endswith("/")
            name = member_name(normalized[:-1] if directory else normalized)
            require(name.casefold() not in folded, f"Duplicate ZIP path: {name}")
            folded.add(name.casefold())
            if directory:
                require(name in directories and entry.file_size == 0, f"Unexpected ZIP directory: {name}")
                continue
            require(name in expected, f"Unexpected ZIP file: {name}")
            with archive.open(entry, "r") as stream:
                observed = stream_digest(stream)
            require(entry.file_size == observed["bytes"] and observed == expected[name],
                    f"ZIP file differs from the package: {name}")
            checked[name] = {**observed, "compressedBytes": entry.compress_size, "compressionMethod": entry.compress_type}
    require(set(checked) == set(expected), "The ZIP omits package files")
    return checked


def publish_new(partial, output):
    # Exclusive creation avoids replacement even if another process races us.
    require_new(output)
    with partial.open("rb") as source, output.open("xb") as target:
        shutil.copyfileobj(source, target, length=1024 * 1024)
    require(file_identity(partial)["sha256"] == file_identity(output)["sha256"], "Published archive differs from its partial")
    partial.unlink()


def pack(package, output, report_path):
    require(sys.version_info >= (3, 11), "Packaging requires Python 3.11 or newer")
    package, output, report_path = map(absolute, (package, output, report_path))
    partial = output.with_suffix(output.suffix + ".partial")
    require(len({output, partial, report_path}) == 3, "Archive, partial and report paths must differ")
    require(all(not path.is_relative_to(package) for path in (output, partial, report_path)),
            "Archive and report outputs must stay outside the packaged directory")
    for path in (output, partial, report_path):
        require_new(path)
    expected = inventory_package(package)
    for path in (output, report_path):
        path.parent.mkdir(parents=True, exist_ok=True)
        check_plain_path(path.parent)
    report = {"schemaVersion": 1, "complete": False, "startedUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
              "packageDirectory": str(package), "partialArchive": str(partial), "packageFiles": expected,
              "manifestSelfHashIncluded": True,
              "compressor": {"implementation": "Python zipfile.ZIP_DEFLATED", "level": 9,
                             "python": sys.executable, "pythonVersion": platform.python_version(),
                             "zlibBuildVersion": zlib.ZLIB_VERSION, "zlibRuntimeVersion": zlib.ZLIB_RUNTIME_VERSION,
                             "timestampPolicy": "Clamp source dates to the ZIP-supported 1980-2107 range; file contents are unchanged."},
              "timingScope": "Compression seconds cover opening/writing/closing the ZIP; verification and publication are separate.",
              "scope": "Release packaging strategy; archive savings are not attributed to executable code optimization."}
    try:
        started = time.perf_counter()
        with zipfile.ZipFile(partial, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9,
                             allowZip64=True, strict_timestamps=False) as archive:
            for relative in expected:
                archive.write(package / relative, arcname=relative, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        report["compressionSeconds"] = time.perf_counter() - started
        started = time.perf_counter()
        report["zipFiles"] = verify_archive(partial, expected)
        require(inventory_package(package) == expected, "Package inputs changed during compression")
        report["verificationSeconds"] = time.perf_counter() - started
        started = time.perf_counter()
        publish_new(partial, output)
        report["publicationSeconds"] = time.perf_counter() - started
        report["archive"] = file_identity(output)
        report["complete"] = True
    except Exception:
        report["error"] = traceback.format_exc()
        if partial.is_file():
            report["failedPartial"] = file_identity(partial)
    report["endedUtc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    with report_path.open("x", encoding="utf-8") as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    report = pack(args.package_dir, args.output, args.report)
    print(json.dumps({"complete": report["complete"], "archive": report.get("archive"), "report": str(absolute(args.report))}))
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        traceback.print_exc()
        sys.exit(1)
'@
# </PROJECT-PYTHON>
}

function Invoke-ProjectCommand {
    param([string]$Name, [hashtable]$Options, [string[]]$Arguments)
    if ($null -eq $Arguments) { $Arguments = @() }
    $pythonCommands = @{
        AuditRuntime = 'audit-runtime'; GenerateIcon = 'generate-icon'
        GenerateLicenses = 'generate-open-source-licenses'; PackageLicenses = 'package-licenses'
        PackageZip = 'package-zip'
    }
    $powershellCommands = @{
        Build = 'Invoke-ProjectBuild'; PackageInstaller = 'Invoke-ProjectInstaller'
        VerifyPackage = 'Test-ProjectPackage'; VerifyWindowsCompatibility = 'Test-ProjectWindowsCompatibility'
    }
    if ($Name -eq 'Help') {
        @'
CompositorWin development tools (one source script)

  project.ps1 Build [-Action All|Check|Test|Release|Package] [-Offline] [-Jobs 1]
                   [-PackageName NAME] [-RecordDirectory PATH]
  project.ps1 PackageInstaller -CompilerPath MAKENSIS [-PackageName NAME]
                   [-InstallerName NAME] [-RecordDirectory PATH] [-QaName NAME]
  project.ps1 VerifyPackage [-RepositoryRoot PATH] [-PackageName NAME] [-ReportPath PATH]
  project.ps1 VerifyWindowsCompatibility [-ExePath PATH] [-OutputDirectory PATH]
                   [-StartupTimeoutSeconds 30] [-CloseTimeoutSeconds 20] [-KeepProcesses]
  project.ps1 AuditRuntime [--exe PATH] [--validation PATH] [--validation-log PATH]
                   [--evidence-dir PATH] [--dumpbin PATH] [--mt PATH]
  project.ps1 GenerateLicenses [--check] [--verify-exe PATH] [--nsis-copying PATH]
                   [--write-package-index PATH] [--verify-package-index PATH]
  project.ps1 PackageLicenses create|verify [--help]
  project.ps1 PackageZip --package-dir PATH --output PATH --report PATH
  project.ps1 GenerateIcon

Use -PythonPath PATH to select Python 3.11+; GenerateIcon also needs Pillow.
Python subcommands accept --help. PowerShell subcommands retain their named options.
'@
        return
    }
    if ($pythonCommands.ContainsKey($Name)) {
        if ($Options.Count) { throw "Use --options for Python subcommand ${Name}: $($Options.Keys -join ', ')" }
        Invoke-ProjectPython -Name $pythonCommands[$Name] -Arguments $Arguments
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        return
    }
    if (-not $powershellCommands.ContainsKey($Name)) { throw "Unknown command: $Name. Run project.ps1 Help." }
    # Bind only this PowerShell command's options. Declaring their union on the
    # script would also consume Python --output/--report as abbreviated PS names.
    $target = Get-Command $powershellCommands[$Name] -CommandType Function
    for ($i = 0; $i -lt $Arguments.Count; $i++) {
        if ($Arguments[$i] -notmatch '^-(?<name>[^:\s]+)(?::(?<value>.*))?$') {
            throw "Expected a named option for ${Name}: $($Arguments[$i])"
        }
        $requested = $Matches['name']
        $inline = $Matches.ContainsKey('value')
        $value = $Matches['value']
        $keys = @($target.Parameters.Keys | Where-Object { $_ -eq $requested })
        if (-not $keys.Count) { $keys = @($target.Parameters.Keys | Where-Object { $_.StartsWith($requested, [System.StringComparison]::OrdinalIgnoreCase) }) }
        if ($keys.Count -ne 1) { throw "Unknown or ambiguous option for ${Name}: -$requested" }
        $key = $keys[0]
        if ($Options.ContainsKey($key)) { throw "Repeated option for ${Name}: -$key" }
        if ($target.Parameters[$key].ParameterType -eq [System.Management.Automation.SwitchParameter]) {
            if (-not $inline) { $value = $true }
            elseif ($value -match '^(?:\$?true|1)$') { $value = $true }
            elseif ($value -match '^(?:\$?false|0)$') { $value = $false }
            else { throw "Expected true or false for -${key}: $value" }
        } elseif (-not $inline) {
            $i++
            if ($i -ge $Arguments.Count) { throw "Missing value for -$key" }
            $value = $Arguments[$i]
        }
        $Options[$key] = $value
    }
    & $target @Options
}

# Dot-sourcing loads the shared helpers without running a build or any command.
if ($MyInvocation.InvocationName -ne '.') {
    $ErrorActionPreference = 'Stop'
    $options = @{}
    foreach ($key in $PSBoundParameters.Keys) {
        if ($key -notin @('Command', 'PythonPath', 'ToolArguments')) { $options[$key] = $PSBoundParameters[$key] }
    }
    Invoke-ProjectCommand -Name $Command -Options $options -Arguments $ToolArguments
}
