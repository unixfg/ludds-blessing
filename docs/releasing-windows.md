# Windows 0.2.0 beta release runbook

The `Windows beta candidate` workflow is the authoritative packaging path for version `0.2.0`. It only builds and retains a candidate. The separate `Promote Windows beta` workflow publishes that exact retained artifact after acceptance; it never rebuilds the binaries.

## Prerequisites

1. Use a clean commit on the public repository’s default branch.
2. Keep every workspace, frontend, and Tauri manifest version identical. `verify-release.ps1` checks the complete set.
3. Confirm that no real save, local fixture directory, Starsector asset, signing key, or environment file is tracked.
4. Complete the Windows in-game acceptance pass with permissioned fixtures. Modded editing must not be described as verified until a permissioned real modded fixture passes.
5. Review the conservative dependency inventory and source/build SPDX SBOM for missing or unacceptable license declarations. Neither is a signed binary-composition attestation.
6. Confirm that the repository’s Actions policy permits only full-length commit SHA references. Set the repository variable `ENABLE_DEPENDENCY_REVIEW` to `true` to enable pull-request dependency review when that feature is available.
7. Confirm that GitHub private vulnerability reporting is enabled and that `SECURITY.md` points reporters to it.

## Build and review

1. Dispatch **Windows beta candidate** from the default branch with a tag matching `v<package-version>-<prerelease>`.
2. Download the retained Actions artifact.
3. Record the successful candidate workflow run ID. Verify that its artifact contains the offline NSIS installer, portable executable, zip archive, license, product/dependency notices, provenance record, SPDX SBOM, and SHA-256 manifests.
4. Verify hashes independently, install on clean Windows 10 and 11 x64 test systems, and repeat the transaction/recovery and in-game smoke tests.
5. Retain the exact artifact used for acceptance; do not rebuild between acceptance and publication.

After approval, dispatch **Promote Windows beta** with the accepted run ID and the identical tag. The promotion workflow verifies the source workflow, successful conclusion, default-branch commit, provenance, complete artifact shape, internal and public checksums, legal/guide/release-note copies, and tag target before creating the prerelease. Protect its `release` environment with required reviewers when the repository plan supports environments.

Promotion in the public source repository produces a public prerelease. GitHub
also exposes source archives for the tagged revision. Confirm the release
contains the accepted binaries, checksum manifest, copyright notice, GPL
license, security policy, dependency notices, provenance, and SBOM before
sharing it.

## Local packaging

With the Tauri Windows prerequisites, Node.js 24.19.0, pnpm 11.19.0, the pinned Rust toolchain, and cargo-audit 0.22.2 installed:

```powershell
pnpm install --frozen-lockfile
pnpm audit --prod --audit-level high
pnpm test
cargo fmt --all -- --check
cargo audit
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
pnpm tauri build --bundles nsis
$stage = .\scripts\release\prepare-windows.ps1
```

Generate `SBOM.spdx.json` into `$stage` with Syft or the same pinned `anchore/sbom-action` used by CI. It catalogs the checked-out source/build graph and staged payload, not only runtime binary contents. Add descriptive provenance identifying the clean source commit and candidate tag, then run:

```powershell
.\scripts\release\new-release-provenance.ps1 -StagingDirectory $stage -Tag v0.2.0-beta.1 -SourceCommit (git rev-parse HEAD) -Repository local/local -WorkflowRunId local
.\scripts\release\finalize-windows.ps1 -StagingDirectory $stage
.\scripts\release\verify-windows-artifacts.ps1 -ArtifactRoot (Split-Path -Parent $stage) -Tag v0.2.0-beta.1 -SourceCommit (git rev-parse HEAD) -Repository local/local -WorkflowRunId local
```

The release scripts refuse to overwrite an existing staging directory,
provenance file, archive, or checksum manifest.

## Signing and later platforms

Version `0.2.0` is deliberately unsigned, so the release notes and beta guide must retain the SmartScreen warning. Do not add a signing secret to the repository. macOS and Linux artifacts remain blocked on native transaction tests and in-game smoke tests; macOS additionally requires signing and notarization before public distribution.
