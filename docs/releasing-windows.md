# Windows 0.2.0 beta release runbook

The `Windows beta release` workflow is the authoritative packaging path for version `0.2.0`. A numbered beta tag automatically builds the exact tagged commit, retains the candidate for 35 days, and then pauses before publication at the protected `release` environment. After approval, the workflow uploads every asset to an unpublished draft, verifies the uploaded names and sizes, and publishes that same candidate; it never rebuilds the binaries.

Manual dispatch remains available for building an untagged review candidate. A manually dispatched run never publishes. After acceptance, the separate `Promote Windows beta` workflow can publish that retained candidate by workflow run ID and tag.

## Prerequisites

1. Use a clean commit on the public repository's default branch with all required checks passing.
2. Keep every workspace, frontend, and Tauri manifest version identical. `verify-release.ps1` checks the complete set.
3. Confirm that no real save, local fixture directory, Starsector asset, signing key, or environment file is tracked.
4. Complete the Windows in-game acceptance pass with permissioned fixtures. Modded editing must not be described as verified until a permissioned real modded fixture passes.
5. Review the conservative dependency inventory and source/build SPDX SBOM for missing or unacceptable license declarations. Neither is a signed binary-composition attestation.
6. Confirm that the repository's Actions policy permits only full-length commit SHA references. Set the repository variable `ENABLE_DEPENDENCY_REVIEW` to `true` to enable pull-request dependency review when that feature is available.
7. Protect the GitHub `release` environment with a required reviewer. If deployment refs are restricted, allow numbered beta tags for the automatic path and `main` for the manual fallback. Keep the repository's default `GITHUB_TOKEN` permission read-only; the publication job requests its narrowly scoped write permission itself.
8. Keep the beta-tag ruleset active so a release tag can be created once but cannot later be moved or deleted.

## Automatic tagged release

1. Confirm that the intended release commit is on the default branch and its checks are green.
2. Create an annotated tag matching `v<package-version>-beta.<positive-number>`, for example:

   ```bash
   git tag -a v0.2.0-beta.1 -m "Ludd's Blessing 0.2.0 beta 1"
   git push origin v0.2.0-beta.1
   ```

3. The **Windows beta release** workflow automatically builds and verifies the candidate.
4. While publication waits for approval, download the retained Actions artifact. Verify its hashes independently, install it on clean Windows 10 and 11 x64 test systems, and repeat the transaction/recovery and in-game smoke tests.
5. Approve the `release` environment only after accepting that exact artifact.
6. Confirm that the resulting GitHub prerelease contains the accepted binaries, checksum manifest, copyright notice, GPL license, dependency notices, provenance, and SBOM.

Do not move a release tag after review or pre-create its GitHub Release. If the build fails before artifact upload, rerun the failed build. If publication fails after a successful build, rerun only the failed jobs so the exact retained artifact is reused. An incomplete upload remains an unpublished draft that the workflow can safely repair. Use a new beta number after changing source code.

## Manual fallback

Dispatch **Windows beta release** from the default branch with a tag matching `v<package-version>-beta.<positive-number>`. The manual path builds and verifies a candidate but deliberately skips publication.

Download and accept the retained artifact as above, then dispatch **Promote Windows beta** with the successful candidate run ID and the identical tag. The promotion workflow verifies the source workflow, successful conclusion, default-branch commit, provenance, complete artifact shape, internal and public checksums, legal/guide/release-note copies, and tag target before creating the prerelease.

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

The release scripts refuse to overwrite an existing staging directory, provenance file, archive, or checksum manifest.

## Signing and later platforms

Version `0.2.0` is deliberately unsigned, so the release notes and beta guide must retain the SmartScreen warning. Do not add a signing secret to the repository. macOS and Linux artifacts remain blocked on native transaction tests and in-game smoke tests; macOS additionally requires signing and notarization before public distribution.
