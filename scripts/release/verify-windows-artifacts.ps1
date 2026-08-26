[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ArtifactRoot,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$Tag,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{40}(?:[0-9a-fA-F]{24})?$')][string]$SourceCommit,
    [Parameter(Mandatory = $true)][ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')][string]$Repository,
    [Parameter(Mandatory = $true)][ValidatePattern('^(?:[1-9][0-9]*|local)$')][string]$WorkflowRunId
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$root = (Resolve-Path -LiteralPath $ArtifactRoot).Path
$package = Get-Content -LiteralPath (Join-Path $repoRoot 'package.json') -Raw | ConvertFrom-Json
$version = [string]$package.version
$expectedTag = '^v' + [regex]::Escape($version) + '(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$'
if ($Tag -notmatch $expectedTag) {
    throw "Tag '$Tag' does not identify version $version."
}

$baseName = "LuddsBlessing_${version}_windows-x64"
$staging = Join-Path $root $baseName
$archivePath = Join-Path $root "$baseName.zip"
$outerManifest = Join-Path $root "$baseName-SHA256SUMS.txt"
if (-not (Test-Path -LiteralPath $staging -PathType Container)) {
    throw "Candidate staging directory is missing: $staging"
}

function Get-LowerSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Read-And-VerifyManifest {
    param(
        [Parameter(Mandatory = $true)][string]$ManifestPath,
        [Parameter(Mandatory = $true)][string]$FilesDirectory,
        [Parameter(Mandatory = $true)][string[]]$ExpectedNames
    )

    if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf)) {
        throw "Checksum manifest is missing: $ManifestPath"
    }
    $entries = @{}
    foreach ($line in @(Get-Content -LiteralPath $ManifestPath)) {
        if ($line -notmatch '^([0-9a-fA-F]{64})  ([^\\/]+)$') {
            throw "Malformed checksum line in $ManifestPath`: $line"
        }
        $name = [string]$Matches[2]
        if ($entries.ContainsKey($name)) {
            throw "Duplicate checksum entry in $ManifestPath`: $name"
        }
        $entries[$name] = [string]$Matches[1].ToLowerInvariant()
    }

    $actualNames = @($entries.Keys | Sort-Object)
    $wantedNames = @($ExpectedNames | Sort-Object)
    if (($actualNames -join "`n") -ne ($wantedNames -join "`n")) {
        throw "Unexpected checksum contents in $ManifestPath. Expected:`n$($wantedNames -join "`n")`nActual:`n$($actualNames -join "`n")"
    }

    foreach ($name in $wantedNames) {
        $candidate = if ($name -eq "$baseName.zip") {
            Join-Path $root $name
        } else {
            Join-Path $FilesDirectory $name
        }
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            throw "Checksummed file is missing: $candidate"
        }
        $actualHash = Get-LowerSha256 -Path $candidate
        if ($actualHash -ne $entries[$name]) {
            throw "SHA-256 mismatch for $candidate."
        }
    }
}

$installerName = "$baseName-installer.exe"
$portableName = "$baseName-portable.exe"
$stagedPayloadNames = @(
    'DEPENDENCY_NOTICES.md',
    $installerName,
    'LICENSE.md',
    $portableName,
    'PRODUCT_NOTICES.md',
    'PROVENANCE.json',
    'README.md',
    'RELEASE_NOTES.md',
    'SBOM.spdx.json',
    'SECURITY.md'
)
$stagedNames = @(Get-ChildItem -LiteralPath $staging -File | Select-Object -ExpandProperty Name | Sort-Object)
$expectedStagedNames = @(($stagedPayloadNames + @('SHA256SUMS.txt')) | Sort-Object)
if (($stagedNames -join "`n") -ne ($expectedStagedNames -join "`n")) {
    throw "Candidate staging directory has unexpected files. Expected:`n$($expectedStagedNames -join "`n")`nActual:`n$($stagedNames -join "`n")"
}

$topLevelNames = @(Get-ChildItem -LiteralPath $root -Force | Select-Object -ExpandProperty Name | Sort-Object)
$expectedTopLevelNames = @(@($baseName, "$baseName-SHA256SUMS.txt", "$baseName.zip") | Sort-Object)
if (($topLevelNames -join "`n") -ne ($expectedTopLevelNames -join "`n")) {
    throw "Candidate artifact has unexpected top-level entries. Expected:`n$($expectedTopLevelNames -join "`n")`nActual:`n$($topLevelNames -join "`n")"
}

Read-And-VerifyManifest -ManifestPath (Join-Path $staging 'SHA256SUMS.txt') -FilesDirectory $staging -ExpectedNames $stagedPayloadNames
Read-And-VerifyManifest `
    -ManifestPath $outerManifest `
    -FilesDirectory $staging `
    -ExpectedNames @($expectedStagedNames + "$baseName.zip")

$provenance = Get-Content -LiteralPath (Join-Path $staging 'PROVENANCE.json') -Raw | ConvertFrom-Json
$toolchainText = Get-Content -LiteralPath (Join-Path $repoRoot 'rust-toolchain.toml') -Raw
$toolchainMatch = [regex]::Match($toolchainText, '(?m)^channel\s*=\s*"(\d+\.\d+\.\d+)"\s*$')
if (-not $toolchainMatch.Success) {
    throw 'rust-toolchain.toml does not contain a pinned release toolchain.'
}
$expectedProvenance = [ordered]@{
    schemaVersion = '1'
    product = "Ludd’s Blessing"
    version = $version
    tag = $Tag
    sourceCommit = $SourceCommit.ToLowerInvariant()
    repository = $Repository
    workflowRunId = $WorkflowRunId
    rustToolchain = [string]$toolchainMatch.Groups[1].Value
}
foreach ($entry in $expectedProvenance.GetEnumerator()) {
    if ([string]$provenance.($entry.Key) -ne [string]$entry.Value) {
        throw "Provenance mismatch for $($entry.Key): expected '$($entry.Value)', found '$($provenance.($entry.Key))'."
    }
}
if ([string]$provenance.nodeVersion -ne [string]$package.engines.node) {
    throw "Provenance Node.js version does not match package.json engines.node."
}
if ([string]$provenance.pnpmVersion -ne [string]$package.engines.pnpm) {
    throw "Provenance pnpm version does not match package.json engines.pnpm."
}
if ([string]::IsNullOrWhiteSpace([string]$provenance.runnerImage) -or
    [string]::IsNullOrWhiteSpace([string]$provenance.buildHost)) {
    throw 'Provenance does not identify the build host and runner image.'
}
if ([string]$provenance.provenanceKind -ne 'descriptive-build-metadata' -or
    [bool]$provenance.binaryCompositionAttestation) {
    throw 'Provenance must identify itself as descriptive, unsigned build metadata.'
}
if ([DateTimeOffset]::MinValue -eq [DateTimeOffset]::Parse([string]$provenance.createdAtUtc)) {
    throw 'Provenance timestamp is invalid.'
}

$sourceCopies = [ordered]@{
    'LICENSE.md' = 'LICENSE.md'
    'PRODUCT_NOTICES.md' = 'THIRD_PARTY_NOTICES.md'
    'README.md' = 'docs\beta-user-guide.md'
    'RELEASE_NOTES.md' = "docs\release-notes\$version-beta.md"
    'SECURITY.md' = 'SECURITY.md'
}
foreach ($copy in $sourceCopies.GetEnumerator()) {
    $stagedHash = Get-LowerSha256 -Path (Join-Path $staging $copy.Key)
    $sourceHash = Get-LowerSha256 -Path (Join-Path $repoRoot $copy.Value)
    if ($stagedHash -ne $sourceHash) {
        throw "Staged $($copy.Key) does not match $($copy.Value) at the candidate source commit."
    }
}

foreach ($binaryName in @($installerName, $portableName)) {
    $binaryPath = Join-Path $staging $binaryName
    $binary = Get-Item -LiteralPath $binaryPath
    if ($binary.Length -lt 1MB) {
        throw "Staged Windows binary is implausibly small: $binaryName"
    }
    $stream = [System.IO.File]::OpenRead($binaryPath)
    try {
        if ($stream.ReadByte() -ne 0x4d -or $stream.ReadByte() -ne 0x5a) {
            throw "Staged Windows binary does not have a PE executable header: $binaryName"
        }
    }
    finally {
        $stream.Dispose()
    }
}

$dependencyNotices = Get-Content -LiteralPath (Join-Path $staging 'DEPENDENCY_NOTICES.md') -Raw
foreach ($heading in @(
    '# Dependency notices',
    '## Rust dependencies',
    '## JavaScript production dependencies',
    '## Bundled license and notice texts'
)) {
    if (-not $dependencyNotices.Contains($heading)) {
        throw "Dependency notices are incomplete; missing heading: $heading"
    }
}

try {
    $sbom = Get-Content -LiteralPath (Join-Path $staging 'SBOM.spdx.json') -Raw | ConvertFrom-Json
}
catch {
    throw "SPDX SBOM is not valid JSON: $($_.Exception.Message)"
}
if ([string]$sbom.spdxVersion -notmatch '^SPDX-2\.[0-9]+$') {
    throw "SBOM does not identify a supported SPDX 2.x document."
}
$sbomPackages = @($sbom.packages)
if ($sbomPackages.Count -eq 0) {
    throw 'SBOM contains no packages; the locked source graph was not cataloged.'
}
if ([string]::IsNullOrWhiteSpace([string]$sbom.name) -or
    [string]$sbom.documentNamespace -notmatch '^https?://') {
    throw 'SBOM document identity is missing or malformed.'
}
if (@($sbom.creationInfo.creators).Count -eq 0 -or
    [DateTimeOffset]::MinValue -eq [DateTimeOffset]::Parse([string]$sbom.creationInfo.created)) {
    throw 'SBOM creation metadata is missing or malformed.'
}
$packageIds = @($sbomPackages | ForEach-Object { [string]$_.SPDXID })
if ($packageIds | Where-Object { $_ -notmatch '^SPDXRef-' }) {
    throw 'SBOM contains a package without a valid SPDX element identifier.'
}
if (@($packageIds | Select-Object -Unique).Count -ne $packageIds.Count) {
    throw 'SBOM contains duplicate package SPDX identifiers.'
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
try {
    $archiveEntries = @($archive.Entries | Where-Object { -not $_.FullName.EndsWith('/') })
    $archiveNames = @($archiveEntries | Select-Object -ExpandProperty FullName | Sort-Object)
    if (($archiveNames -join "`n") -ne ($expectedStagedNames -join "`n")) {
        throw "Candidate zip contents do not match the staged payload."
    }
    foreach ($entry in $archiveEntries) {
        if ($entry.FullName -match '[\\/]' -or $entry.FullName -match '^\.') {
            throw "Candidate zip contains a nested or hidden path: $($entry.FullName)"
        }
        $stream = $entry.Open()
        $hasher = [System.Security.Cryptography.SHA256]::Create()
        try {
            $hashBytes = $hasher.ComputeHash($stream)
            $archiveHash = -join ($hashBytes | ForEach-Object { $_.ToString('x2') })
        }
        finally {
            $hasher.Dispose()
            $stream.Dispose()
        }
        $stagedHash = Get-LowerSha256 -Path (Join-Path $staging $entry.FullName)
        if ($archiveHash -ne $stagedHash) {
            throw "Candidate zip entry differs from the staged file: $($entry.FullName)"
        }
    }
}
finally {
    $archive.Dispose()
}

Write-Host "Candidate artifact is internally consistent for $Tag at $SourceCommit."
