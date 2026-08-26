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
| `actions/dependency-review-action` | `v5.0.0` | `a1d282b36b6f3519aa1f3fc636f609c47dddb294` | [release commit](https://github.com/actions/dependency-review-action/commit/a1d282b36b6f3519aa1f3fc636f609c47dddb294) |
| `pnpm/action-setup` | `v6.0.10` | `0977fd99725f1db4007ccb2928dbb4e90d06cc86` | [release commit](https://github.com/pnpm/action-setup/commit/0977fd99725f1db4007ccb2928dbb4e90d06cc86) |
| `Swatinem/rust-cache` | `v2.9.2` | `6323deb102c322ba6fcbdcafc7e3dddab59af2b6` | [release commit](https://github.com/Swatinem/rust-cache/commit/6323deb102c322ba6fcbdcafc7e3dddab59af2b6) |

The Rust compiler and components are not installed through an action. They are
pinned directly in `rust-toolchain.toml` so local development, CI, and release
builds resolve the same toolchain.
