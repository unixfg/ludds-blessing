[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$StagingDirectory
)

$ErrorActionPreference = 'Stop'
$staging = (Resolve-Path -LiteralPath $StagingDirectory).Path
$artifactRoot = Split-Path -Parent $staging
$baseName = Split-Path -Leaf $staging

$requiredPatterns = @(
    "$baseName.exe",
    'PRODUCT_NOTICES.md',
    'DEPENDENCY_NOTICES.md',
    'COPYRIGHT.md',
    'LICENSE.md',
    'README.md',
    'RELEASE_NOTES.md',
    'PROVENANCE.json',
    'SBOM.spdx.json'
)
foreach ($pattern in $requiredPatterns) {
    $matches = @(Get-ChildItem -LiteralPath $staging -Filter $pattern -File)
    if ($matches.Count -ne 1) {
        throw "Expected exactly one '$pattern' file in $staging, found $($matches.Count)."
    }
}

$innerManifest = Join-Path $staging 'SHA256SUMS.txt'
if (Test-Path -LiteralPath $innerManifest) {
    throw "Checksum manifest already exists; refusing to overwrite it: $innerManifest"
}

$stagedFiles = @(Get-ChildItem -LiteralPath $staging -File | Sort-Object Name)
$innerLines = foreach ($file in $stagedFiles) {
    $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $($file.Name)"
}
$innerLines | Set-Content -LiteralPath $innerManifest -Encoding ascii

$archivePath = Join-Path $artifactRoot "$baseName.zip"
if (Test-Path -LiteralPath $archivePath) {
    throw "Release archive already exists; refusing to overwrite it: $archivePath"
}
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $archivePath -CompressionLevel Optimal

$outerManifest = Join-Path $artifactRoot "$baseName-SHA256SUMS.txt"
if (Test-Path -LiteralPath $outerManifest) {
    throw "Release checksum manifest already exists; refusing to overwrite it: $outerManifest"
}

$candidateFiles = @(
    Get-ChildItem -LiteralPath $staging -File
    Get-Item -LiteralPath $archivePath
) | Sort-Object Name
$outerLines = foreach ($file in $candidateFiles) {
    $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $($file.Name)"
}
$outerLines | Set-Content -LiteralPath $outerManifest -Encoding ascii

Write-Host "Finalized release archive: $archivePath"
Write-Host "Finalized release checksums: $outerManifest"
