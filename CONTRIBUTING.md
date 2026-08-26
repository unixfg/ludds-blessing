# Contributing to Ludd’s Blessing

Thank you for helping improve Ludd’s Blessing. Contributions are accepted
under the repository’s GPL-3.0-or-later license.

## Protect private and third-party data

Never commit or publicly attach a real save, Starsector asset, mod asset,
credential, signing key, or personally identifying filesystem path. Build the
smallest synthetic reproduction you can, and use the application’s redacted
diagnostics export when reporting behavior.

Report suspected vulnerabilities through the private process in
`SECURITY.md`, not through a public issue.

## Development checks

Install the pinned Node.js, pnpm, and Rust versions listed in `README.md`, then
run:

```text
pnpm install --frozen-lockfile
pnpm typecheck
pnpm test
pnpm build
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

After changing a Rust IPC model, regenerate and verify the committed frontend
bindings:

```text
cargo test -p ludds-blessing --test bindings export_ipc_bindings
pnpm typecheck
```

## Pull requests

Keep changes focused, explain user-visible and save-integrity effects, and add
tests for changed behavior. A pull request must leave generated bindings and
both lockfiles consistent. By submitting a contribution, you agree to license
it under GPL-3.0-or-later.
