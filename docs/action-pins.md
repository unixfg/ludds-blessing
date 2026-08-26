# Reviewed GitHub Action pins

External actions are referenced by full commit SHA. The adjacent workflow
comment is the human-readable release that was reviewed. Dependabot may propose
updates, but a maintainer must verify the new tag and commit in the action’s
official repository before merging.

Pins reviewed on 2026-08-26:

| Action | Release | Full commit SHA | Official verification |
| --- | --- | --- | --- |
| `actions/checkout` | `v7.0.1` | `3d3c42e5aac5ba805825da76410c181273ba90b1` | [release commit](https://github.com/actions/checkout/commit/3d3c42e5aac5ba805825da76410c181273ba90b1) |
| `actions/setup-node` | `v7.0.0` | `820762786026740c76f36085b0efc47a31fe5020` | [release commit](https://github.com/actions/setup-node/commit/820762786026740c76f36085b0efc47a31fe5020) |
| `actions/upload-artifact` | `v7.0.1` | `043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` | [release commit](https://github.com/actions/upload-artifact/commit/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a) |
| `actions/download-artifact` | `v8.0.1` | `3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c` | [release commit](https://github.com/actions/download-artifact/commit/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c) |
| `actions/dependency-review-action` | `v5.0.0` | `a1d282b36b6f3519aa1f3fc636f609c47dddb294` | [release commit](https://github.com/actions/dependency-review-action/commit/a1d282b36b6f3519aa1f3fc636f609c47dddb294) |
| `pnpm/action-setup` | `v6.0.10` | `0977fd99725f1db4007ccb2928dbb4e90d06cc86` | [release commit](https://github.com/pnpm/action-setup/commit/0977fd99725f1db4007ccb2928dbb4e90d06cc86) |
| `Swatinem/rust-cache` | `v2.9.2` | `6323deb102c322ba6fcbdcafc7e3dddab59af2b6` | [release commit](https://github.com/Swatinem/rust-cache/commit/6323deb102c322ba6fcbdcafc7e3dddab59af2b6) |
| `anchore/sbom-action` | `v0.24.0` | `e22c389904149dbc22b58101806040fa8d37a610` | [release commit](https://github.com/anchore/sbom-action/commit/e22c389904149dbc22b58101806040fa8d37a610) |
| `softprops/action-gh-release` | `v3.0.2` | `3d0d9888cb7fd7b750713d6e236d1fcb99157228` | [release commit](https://github.com/softprops/action-gh-release/commit/3d0d9888cb7fd7b750713d6e236d1fcb99157228) |

The Rust compiler and components are not installed through an action. They are
pinned directly in `rust-toolchain.toml` so local development, CI, and release
builds resolve the same toolchain.
