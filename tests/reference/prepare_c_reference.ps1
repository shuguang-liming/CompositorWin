# Development only: preserve the original C arithmetic while replacing Apple-only dispatch blocks with serial scopes.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OriginalRoot
)
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$referenceRoot = (Resolve-Path -LiteralPath $OriginalRoot).Path
$referenceSource = Get-Content -Raw -LiteralPath (Join-Path $referenceRoot 'Compositor/Rendering/DitherPixels.c')
$referenceSource = $referenceSource.Replace('#include <dispatch/dispatch.h>', '')
$referenceSource = [regex]::Replace($referenceSource, '(?s)static void in_bands\(.*?\r?\n}\r?\n', '')
$referenceSource = [regex]::Replace($referenceSource, 'in_bands\((\w+), \^\(size_t (\w+), size_t (\w+)\) \{', {
    param($match)
    '{ size_t ' + $match.Groups[2].Value + '=0, ' + $match.Groups[3].Value + '=' + $match.Groups[1].Value + ';'
})
$referenceSource = $referenceSource.Replace('});', '}')
$referenceSource = $referenceSource.Replace('__block ', '')
$referenceSource = $referenceSource.Replace('failed = 1; return;', 'failed = 1; return 0;')
$buildDirectory = Join-Path $projectRoot 'build'
[System.IO.Directory]::CreateDirectory($buildDirectory) | Out-Null
$referenceOutput = Join-Path $buildDirectory 'c_reference_dither_windows.c'
Set-Content -LiteralPath $referenceOutput -Value $referenceSource -Encoding utf8
