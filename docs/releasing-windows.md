# Windows releases

Stable Windows releases are automatic. Pushing a tag that exactly matches the
version in `package.json` builds one standalone executable and immediately
publishes it as the latest GitHub Release.

## Publish a version

1. Update the version in the project manifests and add
   `docs/release-notes/<version>.md`.
2. Commit the changes to `main` and let CI finish.
3. Create and push the stable version tag:

   ```bash
   git tag -a v0.2.2 -m "Ludd's Blessing v0.2.2"
   git push origin v0.2.2
   ```

The **Windows release** workflow then installs the locked dependencies, builds
the tagged source on GitHub's Windows runner, and publishes
`LuddsBlessing_<version>_windows-x64.exe`. There is no candidate handoff,
manual smoke test, approval environment, promotion workflow, or prerelease
step.

If the automated build fails, fix the problem and publish the next version.
Do not move a tag that already has a release.

## Distribution details

The executable is unsigned, so Windows SmartScreen may warn before launch. It
requires Microsoft WebView2 Evergreen Runtime, which is normally already
installed on supported Windows 10 and 11 systems. There is no installer or
updater.

GitHub automatically adds source archives to the release page. The workflow
uploads only the executable.
