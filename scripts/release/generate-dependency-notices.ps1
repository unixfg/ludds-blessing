[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

Push-Location $repoRoot
try {
    $cargoRaw = & cargo metadata --locked --offline --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata failed while generating dependency notices.'
    }
    $cargo = ($cargoRaw -join "`n") | ConvertFrom-Json

    $nodeRaw = & pnpm licenses list --prod --json --long
    if ($LASTEXITCODE -ne 0) {
        throw 'pnpm licenses failed while generating dependency notices.'
    }
    $nodeLicenses = ($nodeRaw -join "`n") | ConvertFrom-Json
}
finally {
    Pop-Location
}

function ConvertTo-MarkdownCell {
    param([AllowNull()][object]$Value)

    $text = [string]$Value
    if ([string]::IsNullOrWhiteSpace($text)) {
        return 'Not declared'
    }
    return ($text -replace '\|', '\|' -replace '[\r\n]+', ' ')
}

$workspaceIds = @{}
foreach ($id in $cargo.workspace_members) {
    $workspaceIds[[string]$id] = $true
}

$cargoPackages = @(
    $cargo.packages |
        Where-Object { -not $workspaceIds.ContainsKey([string]$_.id) } |
        ForEach-Object {
            [pscustomobject]@{
                Name = [string]$_.name
                Version = [string]$_.version
                License = [string]$_.license
                Homepage = if ($_.homepage) { [string]$_.homepage } else { [string]$_.repository }
                Roots = @((Split-Path -Parent ([string]$_.manifest_path)))
            }
        } |
        Sort-Object Name, Version |
        Group-Object Name, Version |
        ForEach-Object { $_.Group[0] }
)

$nodePackages = @()
foreach ($licenseGroup in $nodeLicenses.PSObject.Properties) {
    foreach ($dependency in @($licenseGroup.Value)) {
        foreach ($dependencyVersion in @($dependency.versions)) {
            $nodePackages += [pscustomobject]@{
                Name = [string]$dependency.name
                Version = [string]$dependencyVersion
                License = if ($dependency.license) { [string]$dependency.license } else { [string]$licenseGroup.Name }
                Homepage = [string]$dependency.homepage
                Roots = @($dependency.paths | ForEach-Object { [string]$_ })
            }
        }
    }
}
$nodePackages = @(
    $nodePackages |
        Sort-Object Name, Version |
        Group-Object Name, Version |
        ForEach-Object { $_.Group[0] }
)

$licenseTexts = @{}
function Add-DependencyLicenseText {
    param(
        [Parameter(Mandatory = $true)][string]$Ecosystem,
        [Parameter(Mandatory = $true)][object]$Dependency
    )

    foreach ($root in @($Dependency.Roots)) {
        if (-not (Test-Path -LiteralPath $root -PathType Container)) {
            continue
        }

        $noticeFiles = @(
            Get-ChildItem -LiteralPath $root -File |
                Where-Object { $_.Name -match '^(?i:LICENSE|LICENCE|COPYING|NOTICE)(?:[-._].*)?$' } |
                Sort-Object Name
        )
        foreach ($noticeFile in $noticeFiles) {
            if ($noticeFile.Length -gt 2MB) {
                throw "Dependency notice file is unexpectedly large: $($Dependency.Name) $($Dependency.Version) / $($noticeFile.Name)"
            }

            $content = Get-Content -LiteralPath $noticeFile.FullName -Raw
            if ([string]::IsNullOrWhiteSpace($content)) {
                continue
            }

            $hasher = [System.Security.Cryptography.SHA256]::Create()
            try {
                $hashBytes = $hasher.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($content))
            }
            finally {
                $hasher.Dispose()
            }
            $hash = -join ($hashBytes | ForEach-Object { $_.ToString('x2') })
            $attribution = "$Ecosystem $($Dependency.Name) $($Dependency.Version) ($($noticeFile.Name))"

            if (-not $licenseTexts.ContainsKey($hash)) {
                $licenseTexts[$hash] = [pscustomobject]@{
                    Hash = $hash
                    Attributions = [System.Collections.Generic.List[string]]::new()
                    Text = $content.TrimEnd()
                }
            }
            if (-not $licenseTexts[$hash].Attributions.Contains($attribution)) {
                $licenseTexts[$hash].Attributions.Add($attribution)
            }
        }
    }
}

foreach ($dependency in $cargoPackages) {
    Add-DependencyLicenseText -Ecosystem 'Rust' -Dependency $dependency
}
foreach ($dependency in $nodePackages) {
    Add-DependencyLicenseText -Ecosystem 'JavaScript' -Dependency $dependency
}
if ($licenseTexts.Count -eq 0) {
    throw 'No dependency license or notice files were found; refusing to create an incomplete release notice.'
}

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('# Dependency notices')
$lines.Add('')
$lines.Add('This conservative inventory was generated from the locked Rust source/build graph and JavaScript production graph at release time. The Rust list may include build- or test-only crates. License identifiers are declarations supplied by each dependency; the accompanying SPDX file catalogs the checked-out source/build graph and is not a binary-composition attestation.')
$lines.Add('')
$lines.Add("Ludd’s Blessing contains no Starsector code, data, art, fonts, sound, logos, or screenshots.")
$lines.Add('')
$lines.Add("The Windows installer embeds Microsoft’s separately licensed WebView2 Evergreen Standalone Installer and is packaged with NSIS through Tauri. These installer components are not enumerated as Cargo or JavaScript packages below.")
$lines.Add('')
$lines.Add('## Rust dependencies')
$lines.Add('')
$lines.Add('| Package | Version | Declared license | Project |')
$lines.Add('| --- | --- | --- | --- |')
foreach ($dependency in $cargoPackages) {
    $lines.Add("| $(ConvertTo-MarkdownCell $dependency.Name) | $(ConvertTo-MarkdownCell $dependency.Version) | $(ConvertTo-MarkdownCell $dependency.License) | $(ConvertTo-MarkdownCell $dependency.Homepage) |")
}
$lines.Add('')
$lines.Add('## JavaScript production dependencies')
$lines.Add('')
$lines.Add('| Package | Version | Declared license | Project |')
$lines.Add('| --- | --- | --- | --- |')
foreach ($dependency in $nodePackages) {
    $lines.Add("| $(ConvertTo-MarkdownCell $dependency.Name) | $(ConvertTo-MarkdownCell $dependency.Version) | $(ConvertTo-MarkdownCell $dependency.License) | $(ConvertTo-MarkdownCell $dependency.Homepage) |")
}
$lines.Add('')
$lines.Add('## Bundled license and notice texts')
$lines.Add('')
$lines.Add('Identical texts are included once. The attribution line lists every locked package represented by that text.')
$lines.Add('')
foreach ($licenseText in @($licenseTexts.Values | Sort-Object Hash)) {
    $lines.Add("### $($licenseText.Hash.Substring(0, 12))")
    $lines.Add('')
    $lines.Add('Applies to: ' + (($licenseText.Attributions | Sort-Object) -join '; '))
    $lines.Add('')
    $lines.Add('~~~~~~~~text')
    $lines.Add($licenseText.Text)
    $lines.Add('~~~~~~~~')
    $lines.Add('')
}

$parent = Split-Path -Parent $OutputPath
if ($parent -and -not (Test-Path -LiteralPath $parent)) {
    New-Item -ItemType Directory -Path $parent | Out-Null
}
$lines | Set-Content -LiteralPath $OutputPath -Encoding utf8
