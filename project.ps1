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
