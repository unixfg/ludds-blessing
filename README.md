# Ludd’s Blessing

Ludd’s Blessing is a local-first desktop editor for Starsector save games. The
first supported write format is the uncompressed `0.98a-RC8` / save-format
`0.6` pair. Other XML saves are opened read-only when possible.

The application never loads Starsector or mod Java classes and never rewrites
the campaign XML wholesale. Supported edits are converted into checked byte
spans, previewed, backed up, reparsed, and committed as a guarded two-file
transaction.

## Development

Prerequisites:

- Node.js 24.19.0 and pnpm 11.19.0 (pinned in `package.json`)
- Rust 1.97.1 with `rustfmt` and `clippy` (installed automatically from
  `rust-toolchain.toml`)
- Tauri 2 platform prerequisites

```text
pnpm install
pnpm test
pnpm build
cargo test --workspace
pnpm tauri dev
```

Regenerate the frontend IPC contract after changing a Rust command model:

```text
cargo test -p ludds-blessing --test bindings export_ipc_bindings
pnpm typecheck
```

The generated files in `src-tauri/bindings/` are committed and consumed by the
React frontend. CI rejects binding drift.

## Architecture

- `crates/save-core`: bounded XML graph indexing, RC8 semantics, checked
  byte-span patches, progression, validation, backups, transactions, and
  startup recovery.
- `src-tauri`: typed IPC, opaque sessions/reviews, save discovery, local
  game/mod catalogs, native filesystem policy, and diagnostics.
- `src`: React save library, character/inventory/reputation/officer editors,
  colony holdings, semantic review, backups/recovery, and settings.

All save writes are source-hash-bound and single-use. The core preserves
unknown bytes, keeps Starsector's own `.bak` files untouched, and stores editor
backups outside the game tree. Save Copy uses a separately journaled hidden
staging directory before publishing the new slot.

Do not place real saves or game assets inside the repository. Use the ignored
`testdata-local/` directory for private regression fixtures.

The supplied local RC8 fixture is exercised only by an ignored, explicitly
opt-in read-only developer test.

## Current editing boundary

- Existing player cargo, colony Storage, and colony Local Resources quantities
  can be adjusted when uniquely recognized in the selected installation.
- Colony Storage can receive new catalog-backed commodities, weapons, fighter
  LPCs, and individual ship/weapon/fighter blueprints. Local Resources can
  receive recognized economic commodities. New stacks are inserted with the
  exact RC8 shape and remain subject to semantic review and transactional
  validation.
- Quantities must stay above zero and at or below the serialized maximum.
  Weapons, fighter wings, and special items require whole quantities.

The editor does not install, generate, or require a Starsector mod.

## Game settings profiles

The Settings page can save reusable local profiles and apply five
installation-wide `settings.json` values: player maximum level, skill points
per level, story points per level, officer maximum level, and the officer
elite-skill limit. Settings writes accept only an installation that discovery
has verified; a caller cannot supply an arbitrary settings path.

Applying a profile is bound to the exact loaded file revision and first creates
an app-owned backup outside the Starsector installation. Starsector must be
closed before applying and restarted afterward. These settings affect game
rules and future progression; applying them does not retroactively rebalance an
existing character. This is direct local configuration editing, not a mod
mechanism.

The editor's RC8 XP simulator is intentionally fail-closed by progression
track. Player XP and target-level edits require the vanilla player level,
skill-point, story-point, and max-level bonus-XP rules. Officer XP and
target-level edits require the vanilla officer level and XP multiplier. The
editor verifies the two related multipliers even though profiles do not edit
them; changing only the officer elite-skill cap does not disable XP editing.
Saves without a unique verified installation association cannot use XP
simulation. Explicit skill, unspent skill-point, and story-point edits remain
available. Reopen an already open save after applying a settings profile.

Manual save roots can be reviewed and forgotten from Settings. Automatic roots
remain internal, and superseded save sessions and reviews are bounded and
discarded rather than accumulating for the lifetime of the app.

## Distribution

Ludd’s Blessing is free software licensed under the GNU General Public License,
version 3 or (at your option) any later version. See `COPYRIGHT.md` and
`LICENSE.md`. Pushing a stable version tag automatically builds and publishes
one standalone Windows executable with no installer. The same GitHub release
page provides source archives for the corresponding tag.

Suspected vulnerabilities can be reported privately through
[GitHub's private vulnerability reporting form](https://github.com/unixfg/ludds-blessing/security/advisories/new).

Starsector is created by Fractal Softworks; Ludd’s Blessing is an independent
community tool and does not include Starsector or mod assets.
