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
            @{ source = (Join-Path $projectRoot 'runtime-imports.json'); destination = 'runtime-imports.json' },
            @{ source = (Join-Path $projectRoot 'SKILL.md'); destination = 'SKILL.md' },
            @{ source = (Join-Path $projectRoot 'USER_AGENT_SKILL.md'); destination = 'USER_AGENT_SKILL.md' }
        )
        # Ship one user guide, the Agent skill and the executable's runtime audit.
        foreach ($record in $sourceFiles) { Copy-Item -LiteralPath $record.source -Destination (Join-Path $releaseDirectory $record.destination) }
        $readme = [System.IO.File]::ReadAllText((Join-Path $projectRoot 'README.md'))
        $guide = [regex]::Match($readme, '(?s)<!-- USER-GUIDE-BEGIN -->\s*(.*?)\s*<!-- USER-GUIDE-END -->')
        if (-not $guide.Success) { throw 'The README user guide section is missing.' }
        [System.IO.File]::WriteAllText((Join-Path $releaseDirectory 'README.md'), $guide.Groups[1].Value + [Environment]::NewLine, (New-Object System.Text.UTF8Encoding($false)))
        # Full original notices ship inside the EXE; keep one small discoverable entry file.
        $python = (Get-Command python -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        & $python -B -X utf8 (Join-Path $projectRoot 'generate-open-source-licenses.py') --check --verify-exe (Join-Path $releaseDirectory 'compositor.exe') --write-package-index (Join-Path $releaseDirectory 'LICENSES.md')
        if ($LASTEXITCODE -ne 0) { throw 'Embedded license original-text verification or package index generation failed.' }
        . (Join-Path $projectRoot 'project.ps1')
        $packageFiles = @(Get-ChildItem -LiteralPath $releaseDirectory -Recurse -File | ForEach-Object {
            [pscustomobject]@{ path = $_.FullName.Substring($releaseDirectory.Length + 1).Replace('\', '/'); bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
        })
        Write-ProjectJson @($packageFiles) (Join-Path $releaseDirectory 'package-files.json')
        # Compression is a packaging choice, measured separately from EXE code size.
        $zipReport = Join-Path $recordDirectory ("package-zip-$PackageName.json")
        & $python -B -X utf8 (Join-Path $projectRoot 'package-zip.py') --package-dir $releaseDirectory --output $archive --report $zipReport
        if ($LASTEXITCODE -ne 0) { throw 'Release ZIP compression or content verification failed; partial output and evidence are retained.' }
        $packaged = $true
        Get-FileHash -LiteralPath $archive -Algorithm SHA256
    }
    $completed = $true
}
finally {
    . (Join-Path $projectRoot 'project.ps1')
    $report = [pscustomobject]@{ action = $Action; completed = $completed; startedUtc = $started.ToString('o'); endedUtc = (Get-Date).ToUniversalTime().ToString('o'); cargo = $toolchain; targetDirectory = $env:CARGO_TARGET_DIR; jobs = $Jobs; steps = @($steps.ToArray()); packagingPerformed = $packaged; releaseCrtPolicy = 'Only build --release appends -C target-feature=+crt-static; check/test flags and caches are unchanged, and both rustflags environment variables are restored.'; originalBaselineVerification = 'Historical migration evidence; not applicable to this standalone Windows workspace.' }
    Write-ProjectJson $report (Join-Path $recordDirectory ("build-$($Action.ToLowerInvariant()).json"))
    Pop-Location
}
