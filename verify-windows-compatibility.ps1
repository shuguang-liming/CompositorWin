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
