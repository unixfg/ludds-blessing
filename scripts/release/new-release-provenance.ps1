[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$StagingDirectory,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$Tag,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{40}(?:[0-9a-fA-F]{24})?$')][string]$SourceCommit,
    [Parameter(Mandatory = $true)][ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')][string]$Repository,
    [Parameter(Mandatory = $true)][ValidatePattern('^(?:[1-9][0-9]*|local)$')][string]$WorkflowRunId
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$staging = (Resolve-Path -LiteralPath $StagingDirectory).Path
$package = Get-Content -LiteralPath (Join-Path $repoRoot 'package.json') -Raw | ConvertFrom-Json
$version = [string]$package.version
$expectedTag = '^v' + [regex]::Escape($version) + '(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$'
if ($Tag -notmatch $expectedTag) {
    throw "Tag '$Tag' does not identify version $version."
}

$toolchainText = Get-Content -LiteralPath (Join-Path $repoRoot 'rust-toolchain.toml') -Raw
$toolchainMatch = [regex]::Match($toolchainText, '(?m)^channel\s*=\s*"(\d+\.\d+\.\d+)"\s*$')
if (-not $toolchainMatch.Success) {
    throw 'rust-toolchain.toml does not contain a pinned release toolchain.'
}

$nodeVersion = ((& node --version) -join '').Trim().TrimStart('v')
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($nodeVersion)) {
    throw 'Could not record the Node.js build version.'
}
$pnpmVersion = ((& pnpm --version) -join '').Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($pnpmVersion)) {
    throw 'Could not record the pnpm build version.'
}
$runnerImage = if (-not [string]::IsNullOrWhiteSpace($env:ImageOS) -and -not [string]::IsNullOrWhiteSpace($env:ImageVersion)) {
    "$($env:ImageOS)@$($env:ImageVersion)"
} else {
    'unreported-local-host'
}

$outputPath = Join-Path $staging 'PROVENANCE.json'
if (Test-Path -LiteralPath $outputPath) {
    throw "Provenance already exists; refusing to overwrite it: $outputPath"
}

$provenance = [ordered]@{
    schemaVersion = 1
    product = "Ludd’s Blessing"
    version = $version
    tag = $Tag
    sourceCommit = $SourceCommit.ToLowerInvariant()
    repository = $Repository
    workflowRunId = $WorkflowRunId
    rustToolchain = [string]$toolchainMatch.Groups[1].Value
    nodeVersion = $nodeVersion
    pnpmVersion = $pnpmVersion
    runnerImage = $runnerImage
    buildHost = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
    provenanceKind = 'descriptive-build-metadata'
    binaryCompositionAttestation = $false
    createdAtUtc = [DateTimeOffset]::UtcNow.ToString('o')
}
$provenance | ConvertTo-Json | Set-Content -LiteralPath $outputPath -Encoding utf8
Write-Output $outputPath
