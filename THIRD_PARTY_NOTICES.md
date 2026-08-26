# Third-party notices

The release pipeline generates a conservative notice inventory from the locked
Rust source/build graph and JavaScript production graph, plus an SPDX
source/build SBOM, for each binary artifact. These records support license and
supply-chain review; they are not a claim that every package is present in the
runtime binary or a cryptographic attestation of binary composition.

The Windows installer embeds Microsoft’s separately licensed WebView2
Evergreen Standalone Installer and is packaged with NSIS through Tauri. The
installed WebView2 runtime is serviced independently by Microsoft and may use
Microsoft’s own update mechanisms. Ludd’s Blessing itself contains no updater
and makes no installation-time runtime download.

Ludd’s Blessing does not bundle Starsector code, data, art, fonts, sound,
logos, or screenshots.

The application itself is distributed under GPL-3.0-or-later as described in
`LICENSE.md`. That license does not replace or restrict a third-party
component’s own license.
