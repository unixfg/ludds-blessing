# Third-party notices

The release pipeline generates a conservative notice inventory from the locked
Rust source/build graph and JavaScript production graph, plus an SPDX
source/build SBOM, for each binary artifact. These records support license and
supply-chain review; they are not a claim that every package is present in the
runtime binary or a cryptographic attestation of binary composition.

The standalone Windows executable uses Microsoft’s separately installed
WebView2 Evergreen Runtime. WebView2 is not bundled with Ludd’s Blessing and is
serviced independently by Microsoft. Ludd’s Blessing itself contains no
installer, updater, or runtime download.

Ludd’s Blessing does not bundle Starsector code, data, art, fonts, sound,
logos, or screenshots.

The application itself is distributed under GPL-3.0-or-later as described in
`COPYRIGHT.md` and `LICENSE.md`. That license does not replace or restrict a
third-party component’s own license.
