[CmdletBinding()]
param(
    [ValidateNotNullOrEmpty()]
    [string]$OutputRoot = 'artifacts',

    [string]$BuildTargetRoot
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$package = Get-Content -LiteralPath (Join-Path $repoRoot 'package.json') -Raw | ConvertFrom-Json
$version = [string]$package.version
$baseName = "LuddsBlessing_${version}_windows-x64"

$resolvedOutputRoot = if ([System.IO.Path]::IsPathRooted($OutputRoot)) {
    [System.IO.Path]::GetFullPath($OutputRoot)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
}
$stagingDirectory = Join-Path $resolvedOutputRoot $baseName

if (Test-Path -LiteralPath $stagingDirectory) {
    throw "Staging directory already exists; refusing to overwrite it: $stagingDirectory"
}

$buildRoots = if ([string]::IsNullOrWhiteSpace($BuildTargetRoot)) {
    @(
        (Join-Path $repoRoot 'target'),
        (Join-Path $repoRoot 'src-tauri\target')
    )
} elseif ([System.IO.Path]::IsPathRooted($BuildTargetRoot)) {
    @([System.IO.Path]::GetFullPath($BuildTargetRoot))
} else {
    @([System.IO.Path]::GetFullPath((Join-Path $repoRoot $BuildTargetRoot)))
}

$portableCandidates = @(
    $buildRoots |
        ForEach-Object { Join-Path $_ 'release\ludds-blessing.exe' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }
)
if ($portableCandidates.Count -ne 1) {
    throw "Expected exactly one portable executable, found $($portableCandidates.Count)."
}

$nsisDirectories = @(
    $buildRoots |
        ForEach-Object { Join-Path $_ 'release\bundle\nsis' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Container }
)
$installerCandidates = @(
    $nsisDirectories |
        ForEach-Object { Get-ChildItem -LiteralPath $_ -Filter '*.exe' -File } |
        Where-Object {
            $_.Name -match '(?i)(setup|installer)' -and
            $_.Name -match [regex]::Escape($version)
        }
)
if ($installerCandidates.Count -ne 1) {
    throw "Expected exactly one NSIS installer, found $($installerCandidates.Count)."
}

New-Item -ItemType Directory -Path $stagingDirectory | Out-Null
Copy-Item -LiteralPath $portableCandidates[0] -Destination (Join-Path $stagingDirectory "$baseName-portable.exe")
Copy-Item -LiteralPath $installerCandidates[0].FullName -Destination (Join-Path $stagingDirectory "$baseName-installer.exe")
Copy-Item -LiteralPath (Join-Path $repoRoot 'THIRD_PARTY_NOTICES.md') -Destination (Join-Path $stagingDirectory 'PRODUCT_NOTICES.md')
Copy-Item -LiteralPath (Join-Path $repoRoot 'COPYRIGHT.md') -Destination (Join-Path $stagingDirectory 'COPYRIGHT.md')
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE.md') -Destination (Join-Path $stagingDirectory 'LICENSE.md')
Copy-Item -LiteralPath (Join-Path $repoRoot 'SECURITY.md') -Destination (Join-Path $stagingDirectory 'SECURITY.md')
Copy-Item -LiteralPath (Join-Path $repoRoot 'docs\beta-user-guide.md') -Destination (Join-Path $stagingDirectory 'README.md')
$releaseNotes = Join-Path $repoRoot "docs\release-notes\$version-beta.md"
if (-not (Test-Path -LiteralPath $releaseNotes -PathType Leaf)) {
    throw "Release notes are missing: $releaseNotes"
}
Copy-Item -LiteralPath $releaseNotes -Destination (Join-Path $stagingDirectory 'RELEASE_NOTES.md')

& (Join-Path $PSScriptRoot 'generate-dependency-notices.ps1') -OutputPath (Join-Path $stagingDirectory 'DEPENDENCY_NOTICES.md')

Write-Output $stagingDirectory
