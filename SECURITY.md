# Security policy

## Supported releases

Only the newest 0.2.x community beta is supported. Older beta builds may stop
receiving security fixes as soon as a replacement is available. Release assets
are unsigned during the community beta; verify their SHA-256 manifests before
running them.

## Reporting a vulnerability

Please report suspected vulnerabilities through this repository’s
[GitHub private vulnerability reporting form](https://github.com/unixfg/ludds-blessing/security/advisories/new).
This gives maintainers a private discussion and remediation workspace. Do not
open a public issue with exploit details before a fix is available.

If GitHub does not offer the private reporting form, contact a maintainer
through their GitHub profile and ask for a private reporting route without
including sensitive details in the initial message. Repository administrators
must keep private vulnerability reporting enabled while releases are
available.

Include the application version, operating system, affected workflow, expected
and observed behavior, and the smallest safe reproduction you can provide.
The redacted diagnostics export is preferred. Never attach a real save,
Starsector asset, mod asset, credential, signing material, or personally
identifying path unless every owner has approved the private disclosure and it
is essential to reproduce the issue.

Reports involving save replacement, backup or recovery integrity, forged IPC
selectors, XML resource exhaustion, catalog authorization, path traversal,
symlink handling, privilege boundaries, release artifact substitution, or an
unexpected network request are treated as security-sensitive.

The maintainer will acknowledge a complete report when practical, assess its
impact, and coordinate disclosure after a corrected build is available.
Please do not test against files, computers, or accounts you do not own or have
explicit permission to use.

## Release integrity

Official candidates are produced by the repository’s release workflow. A
candidate is reviewed and smoke-tested before the exact retained Actions
artifact is promoted; promotion does not rebuild it. Workflows pin external
actions to full commit SHAs, and every public binary is covered by a SHA-256
manifest and a source/build SPDX SBOM.
