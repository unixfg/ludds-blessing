[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Tag,

    [switch]$AllowDirty
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

$package = Get-Content -LiteralPath (Join-Path $repoRoot 'package.json') -Raw | ConvertFrom-Json
$tauri = Get-Content -LiteralPath (Join-Path $repoRoot 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json
function Get-TomlSectionVersion {
    param(
        [Parameter(Mandatory = $true)][string]$RelativePath,
        [Parameter(Mandatory = $true)][string]$Section
    )

    $text = Get-Content -LiteralPath (Join-Path $repoRoot $RelativePath) -Raw
    $escapedSection = [regex]::Escape($Section)
    $match = [regex]::Match(
        $text,
        "(?ms)^\[$escapedSection\]\s*.*?^version\s*=\s*`"([^`"]+)`""
    )
    if (-not $match.Success) {
        throw "Could not read [$Section] version from $RelativePath."
    }
    return [string]$match.Groups[1].Value
}

$versions = [ordered]@{
    'package.json' = [string]$package.version
    'src-tauri/tauri.conf.json' = [string]$tauri.version
    'Cargo.toml [workspace.package]' = Get-TomlSectionVersion -RelativePath 'Cargo.toml' -Section 'workspace.package'
    'crates/save-core/Cargo.toml' = Get-TomlSectionVersion -RelativePath 'crates\save-core\Cargo.toml' -Section 'package'
    'src-tauri/Cargo.toml' = Get-TomlSectionVersion -RelativePath 'src-tauri\Cargo.toml' -Section 'package'
}

$uniqueVersions = @($versions.Values | Select-Object -Unique)
if ($uniqueVersions.Count -ne 1) {
    $details = $versions.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }
    throw "Version mismatch:`n$($details -join "`n")"
}

$version = [string]$package.version
$expectedTag = '^v' + [regex]::Escape($version) + '(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$'
if ($Tag -notmatch $expectedTag) {
    throw "Tag '$Tag' does not identify version $version. Expected v$version or a v$version-* prerelease tag."
}
if ([string]$package.engines.node -notmatch '^\d+\.\d+\.\d+$' -or
    [string]$package.engines.pnpm -notmatch '^\d+\.\d+\.\d+$') {
    throw 'package.json must pin complete Node.js and pnpm release numbers.'
}
if ([string]$package.packageManager -ne "pnpm@$($package.engines.pnpm)") {
    throw 'packageManager and engines.pnpm must identify the same pinned pnpm release.'
}

if ($tauri.bundle.createUpdaterArtifacts -ne $false) {
    throw 'Updater artifacts must remain disabled for the local-only beta.'
}
if ($tauri.bundle.licenseFile -ne '../LICENSE.md') {
    throw 'Tauri bundles must include the project license.'
}
if ($tauri.bundle.windows.webviewInstallMode.type -ne 'offlineInstaller') {
    throw 'The Windows installer must bundle the offline WebView2 installer; network bootstrap modes are forbidden.'
}
if ($tauri.bundle.windows.webviewInstallMode.silent -ne $true) {
    throw 'The bundled WebView2 offline installer must run silently.'
}

$csp = [string]$tauri.app.security.csp
if ($csp -match "'unsafe-(?:inline|eval)'") {
    throw 'The production content-security policy must not permit unsafe inline styles/scripts or eval.'
}
$cspWithoutIpcOrigin = $csp.Replace('http://ipc.localhost', '')
if ($cspWithoutIpcOrigin -match '(?i)https?://') {
    throw 'The production content-security policy must not authorize remote HTTP origins.'
}

$toolchainText = Get-Content -LiteralPath (Join-Path $repoRoot 'rust-toolchain.toml') -Raw
$toolchainMatch = [regex]::Match($toolchainText, '(?m)^channel\s*=\s*"(\d+\.\d+\.\d+)"\s*$')
if (-not $toolchainMatch.Success) {
    throw 'rust-toolchain.toml must pin a complete Rust release number rather than a floating channel.'
}

$required = @(
    'Cargo.lock',
    'pnpm-lock.yaml',
    'COPYRIGHT.md',
    'LICENSE.md',
    'SECURITY.md',
    'THIRD_PARTY_NOTICES.md',
    'docs\action-pins.md',
    'docs\beta-user-guide.md',
    "docs\release-notes\$version-beta.md"
)
foreach ($relativePath in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $repoRoot $relativePath) -PathType Leaf)) {
        throw "Required release file is missing: $relativePath"
    }
}

$guideHeading = (Get-Content -LiteralPath (Join-Path $repoRoot 'docs\beta-user-guide.md') -TotalCount 1)
if ($guideHeading -ne "# Ludd’s Blessing $version community beta") {
    throw "The beta guide heading does not identify version $version."
}
$notesHeading = (Get-Content -LiteralPath (Join-Path $repoRoot "docs\release-notes\$version-beta.md") -TotalCount 1)
if ($notesHeading -ne "# Ludd’s Blessing $version community beta") {
    throw "The release notes heading does not identify version $version."
}

$actionPinsText = Get-Content -LiteralPath (Join-Path $repoRoot 'docs\action-pins.md') -Raw
$documentedPinMatches = [regex]::Matches(
    $actionPinsText,
    '(?m)^\| `([^`]+)` \| `([^`]+)` \| `([0-9a-fA-F]{40})` \|'
)
$documentedPins = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
foreach ($pinMatch in $documentedPinMatches) {
    [void]$documentedPins.Add("$($pinMatch.Groups[1].Value)@$($pinMatch.Groups[3].Value)")
}
if ($documentedPins.Count -eq 0) {
    throw 'docs/action-pins.md contains no parseable reviewed action pins.'
}

$workflowPins = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$workflowNodeVersions = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$workflowPnpmVersions = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$workflowFiles = Get-ChildItem -LiteralPath (Join-Path $repoRoot '.github\workflows') -File |
    Where-Object { $_.Extension -in @('.yml', '.yaml') }
foreach ($workflowFile in $workflowFiles) {
    $workflowText = Get-Content -LiteralPath $workflowFile.FullName -Raw
    foreach ($match in [regex]::Matches($workflowText, '(?m)^\s*NODE_VERSION:\s*([^\s#]+)')) {
        [void]$workflowNodeVersions.Add([string]$match.Groups[1].Value)
    }
    foreach ($match in [regex]::Matches($workflowText, '(?m)^\s*PNPM_VERSION:\s*([^\s#]+)')) {
        [void]$workflowPnpmVersions.Add([string]$match.Groups[1].Value)
    }
    $usesMatches = [regex]::Matches($workflowText, '(?m)^\s*(?:-\s*)?uses:\s*([^\s#]+)(?:\s+#\s*(.*))?$')
    foreach ($usesMatch in $usesMatches) {
        $reference = [string]$usesMatch.Groups[1].Value
        if ($reference.StartsWith('./')) {
            continue
        }
        if ($reference -notmatch '@[0-9a-fA-F]{40}$') {
            throw "GitHub Action references must use full commit SHAs: $($workflowFile.Name): $reference"
        }
        $comment = [string]$usesMatch.Groups[2].Value
        if ($comment -notmatch '(?i)\bv?\d+\.\d+(?:\.\d+)?\b') {
            throw "Pinned GitHub Actions require a human-readable version comment: $($workflowFile.Name): $reference"
        }
        $parts = $reference.Split('@', 2)
        $pin = "$($parts[0])@$($parts[1])"
        if (-not $documentedPins.Contains($pin)) {
            throw "GitHub Action pin is not recorded in docs/action-pins.md: $($workflowFile.Name): $reference"
        }
        [void]$workflowPins.Add($pin)
    }
}
foreach ($documentedPin in $documentedPins) {
    if (-not $workflowPins.Contains($documentedPin)) {
        throw "docs/action-pins.md records an action pin that no workflow uses: $documentedPin"
    }
}
if ($workflowNodeVersions.Count -ne 1 -or
    -not $workflowNodeVersions.Contains([string]$package.engines.node)) {
    throw 'Workflow NODE_VERSION values must match the exact package.json Node.js pin.'
}
if ($workflowPnpmVersions.Count -ne 1 -or
    -not $workflowPnpmVersions.Contains([string]$package.engines.pnpm)) {
    throw 'Workflow PNPM_VERSION values must match the exact package.json pnpm pin.'
}

$tracked = & git -C $repoRoot ls-files
if ($LASTEXITCODE -ne 0) {
    throw 'Unable to inspect tracked files.'
}

$forbidden = $tracked | Where-Object {
    $_ -match '(^|/)(campaign|descriptor)\.xml(?:\.bak)?$' -or
    $_ -match '(^|/)(testdata-local|fixtures-local)/' -or
    $_ -match '(?i)\.(pfx|p12|key|pem|jks|keystore)$' -or
    $_ -match '(^|/)\.(netrc|pypirc|envrc)$' -or
    $_ -match '(^|/)\.cargo/credentials(?:\.toml)?$' -or
    ($_ -match '(^|/)\.env(?:\..+)?$' -and $_ -notmatch '(^|/)\.env\.example$')
}
if ($forbidden) {
    throw "Private save fixtures, credentials, or signing material are tracked:`n$($forbidden -join "`n")"
}

if (-not $AllowDirty) {
    if (@($tracked).Count -eq 0) {
        throw 'Release builds require a committed repository; no tracked files were found.'
    }

    $dirty = & git -C $repoRoot status --porcelain --untracked-files=normal
    if ($LASTEXITCODE -ne 0) {
        throw 'Unable to verify repository status.'
    }
    if ($dirty) {
        throw "Release builds require a clean checkout:`n$($dirty -join "`n")"
    }
}

if ($AllowDirty) {
    Write-Warning 'Repository cleanliness was not enforced because -AllowDirty was supplied.'
}
Write-Host "Release metadata is consistent for $Tag (Rust $($toolchainMatch.Groups[1].Value))."
