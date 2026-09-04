# Ludd’s Blessing 0.2.2

This is an unsigned Windows 10/11 x64 release. Windows SmartScreen may show an “unrecognized app” warning because the executable is not code-signed. Verify the SHA-256 digest shown by GitHub for the executable before running it. The tagged source archives on the same release page contain the source, license, and build scripts.

Ludd’s Blessing is an independent community tool and is not affiliated with or endorsed by Fractal Softworks. It does not bundle Starsector files or assets.

## Safety boundaries

- All save processing stays on the local computer. The app has no telemetry, updater, or network feature.
- The standalone executable requires Microsoft’s WebView2 Evergreen Runtime to already be installed and does not fetch it. WebView2 is normally present on current Windows 10 and 11 systems; [Microsoft provides installation guidance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution) for systems where it is missing. Microsoft may service that runtime independently; Ludd’s Blessing itself has no updater.
- The editor does not install, generate, or require a Starsector mod.
- Only uncompressed Starsector `0.98a-RC8` saves using format `0.6` can be changed. Other detected saves are read-only.
- Starsector may remain running while you edit a save that is not currently loaded. Every save the current log session indicates may still be active remains blocked from in-place apply, restore, protected-save unlock, and recovery operations.
- Keep exactly one normally launched Starsector instance open. Multiple, batch-launched, unpaired, or otherwise unverifiable game processes are blocked; do not launch and close a second instance while the first remains running.
- Apply and Restore recheck game activity at the write boundary. Do not load, switch to, or save the target in Starsector while the write is running. If the app cannot confirm that the target is inactive, it fails closed rather than replacing the save.
- Keep the game’s own save backups. Ludd’s Blessing stores its backups outside the Starsector save directory and does not prune them automatically.
- Iron Mode and autosaves remain locked until explicitly acknowledged and immediately backed up for that session.
- Mod support is conservative. Unknown data is preserved, but support for every mod’s semantics is not promised.

## Choosing a download

- `LuddsBlessing_<version>_windows-x64.exe` is the complete application. It runs without installation and creates backups and settings in the normal per-user application-data directory; moving or deleting the executable does not remove that data.
- There is no installer or updater; this is the only application download.
- If an earlier installer-based beta is present, uninstall that copy and remove its Start-menu shortcut before using the standalone executable so there is only one version to launch. Preserve the per-user application-data directory containing settings and backups.

## Finding saves

- **Refresh** re-runs bounded discovery. On Windows, the app verifies detected
  Starsector installations and reads each installation's configured save path
  from `vmparams`; this includes valid custom absolute save folders.
- **Open editor** and **Open preview** automatically repeat discovery before
  loading the selected save, so a manual refresh is not required to pick up a
  campaign that changed while the library was open.
- Discovery also checks the Windows Known Documents folder (including folder
  redirection), the traditional `Documents\Starsector` location, common
  OneDrive Documents locations, and narrowly matched legacy VirtualStore
  locations. It never recursively searches a drive or user profile.
- A missing, disconnected, or unreadable candidate does not prevent healthy
  roots from appearing.
- Older descriptors that predate `gameVersion` or `slotCreationTimestamp` are
  shown with the metadata they contain instead of being mislabeled unreadable.
  They remain strictly read-only unless they meet the RC8/format-0.6 write gate.
- Current and otherwise usable saves sort ahead of unreadable archive entries;
  missing dates never outrank real save dates.
- If a save is still absent, use **Choose folder** and select the Starsector
  installation, a `saves` folder, an individual `save_*` folder, or either XML
  file in that save.
- Manually registered folders appear under **Settings → Remembered save
  folders**, where unavailable entries can be reviewed or forgotten. Automatic
  platform and installation roots remain internal and cannot be accidentally
  removed from that list.
- Standalone archives remain editable when structurally supported, but trusted
  skill, portrait, item, and ship catalogs are loaded only when the save root is
  uniquely associated with a verified installation.
- For safety, legacy VirtualStore saves remain blocked from writes while any
  Starsector instance is running because the editor cannot prove which
  virtualized folder that older process is actively using.

## Game Settings Profiles

- Open **Settings** to load game rules from a verified local Starsector
  installation. The settings writer never accepts an arbitrary file path.
- A profile contains exactly five supported integer values: player maximum
  level, skill points per level, story points per level, officer maximum level,
  and the officer elite-skill limit. The app shows the allowed ranges and
  requires the elite-skill limit not to exceed the officer maximum level.
- Built-in and user-created profiles are local presets. Saving, updating, or
  deleting a profile does not change Starsector until **Create backup & apply
  settings** is selected.
- Close Starsector before applying a profile and restart it afterward. The app
  verifies that the live file still has the exact revision that was loaded,
  creates a separate app-owned backup, replaces the file, and reparses the
  result before reporting success.
- These are installation-wide rules for future progression. Existing
  characters are not automatically rebalanced. The feature directly edits the
  verified local configuration and does not install, generate, or require a
  Starsector mod.
- XP and target-level simulation fails closed only for the affected progression
  track. Player edits require vanilla player level, point-award, story-award,
  and max-level bonus-XP rules; officer edits require vanilla officer level and
  XP-multiplier rules. The editor verifies the two multipliers even though
  profiles do not edit them. Changing only the officer elite-skill cap does not
  disable XP editing. Unassociated saves cannot use XP simulation. Direct skill
  and unspent-point edits remain available. Reopen a save in Ludd's Blessing
  after applying a different settings profile.

## Inventory and colony holdings

- **Inventory** edits quantities on existing player-fleet cargo stacks.
- **Colonies** places the Storage and Local Resources tabs directly beneath
  each colony name. Existing recognized quantities may be adjusted in both.
- **Add item** creates a reviewed Storage stack for a catalog-backed commodity,
  weapon, fighter LPC, or individual ship/weapon/fighter blueprint. **Add
  commodity** does the same for an economic commodity in Local Resources. The
  validated catalog stays open after each addition; select **Done adding** after
  staging every intended stack.
- Local Resources edits affect the current stockpile only. They do not create
  a month-end charge or refund, and normal production, replenishment, or
  shortage consumption may change the value after loading.
- Quantities must remain greater than zero and cannot exceed the stack's saved
  maximum. Weapons, fighter wings, and special items use whole quantities.
- Resource quantities are stored by Starsector as single-precision values. The
  editor shortens harmless float noise such as `721.32007` to `721.32` while an
  unchanged field is at rest; focus the field to inspect or edit its exact saved
  value. Presentation formatting alone never stages or writes a change.
- Unknown or ambiguous mod items remain visible by ID but read-only. The app
  rechecks the local item catalog when a review is prepared and applied.
- Review labels derived cargo-space changes separately. Exceeding saved cargo
  capacity produces a warning that must be acknowledged; capacity itself is
  never changed.

## Officer skills

- **Make all Unlearned**, **Make all Learned**, and **Make all Elite** stage a
  rank for every editable skill on the selected officer. Read-only or unknown
  mod skills remain untouched, and Elite uses each skill's highest supported
  rank.
- The officer roster remains pinned while the ability list scrolls. In compact
  layouts it becomes a pinned horizontal roster, so switching officers does not
  require returning to the top of the page.
- Bulk skill changes follow the normal semantic review and do not silently
  spend or refund the officer's unspent points.

## Verify a download in PowerShell

```powershell
Get-FileHash -Algorithm SHA256 -LiteralPath .\LuddsBlessing_0.2.2_windows-x64.exe
```

Compare the displayed hash with the `sha256:` digest GitHub shows beside the executable on the release page. A mismatch means the file must not be run.

## Checking an edited save

Before trusting an edited copy, load it in Starsector `0.98a-RC8`, inspect the changed values and cargo-space totals, earn additional XP when progression was changed, save again in-game, and reopen that new save in the editor. Report a failure with the redacted diagnostics export; do not attach a save publicly unless every owner of its contents has approved sharing it. Report a suspected vulnerability privately through [GitHub's reporting form](https://github.com/unixfg/ludds-blessing/security/advisories/new), not in a public issue.
