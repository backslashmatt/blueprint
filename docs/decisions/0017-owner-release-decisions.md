# 0017: macOS CI returns; release decisions

**Status:** accepted, 2026-11-26. **Supersedes:** [0004](0004-macos-ci-deferred.md) (macOS CI deferred). **Decided by:** the owner, in their own words on #133, #159 and #37.

## Context

M5 needs installers and auto-update on all three platforms. Some of that needs the owner (REQUIREMENTS §8.1): money, signing identities, publishing, and licences outside the allowed list. The questions were put on #159, #133 and #37.

## Decisions

| Question | Owner's answer | What it means |
|---|---|---|
| macOS CI (#133) | "We can add Mac os ci now" | `macos-latest` returns to CI: tests, the app steps, the M4 exit test and a `.dmg`. |
| Windows signing (#159, 1) | "Option a sounds good. Anything paid should be moved to the end of development though to defer any costs" | Azure Trusted Signing, set up last. Until then, the Windows installer is unsigned. |
| macOS signing and notarisation (#159, 2) | "Unsigned for now" | The `.dmg` is unsigned. Gatekeeper asks the user to allow it (`docs/install.md`). |
| Auto-update channel (#159, 3) | "Option a is fine" | Updates come from GitHub Releases on this repository. The update feed is signed with an ed25519 key held as a repository secret, separate from platform signing. Publishing releases for that channel is allowed. |
| AppImage (#159, 4) | "I'll go with your recommendation" | No AppImage. Its runtime's LGPL-2.1 licence stays outside §8.1. |
| Linux targets (#37) | "Confirm all" | Ubuntu (current LTS) and Fedora (current release) are the named targets. Each gets a native package: `.deb` and `.rpm`. The §5.1 wording itself is PR #76, for the owner to merge. |

## Order of work

1. **macOS CI** (#133).
2. **`.rpm` for Fedora** (#37). cargo-packager has no rpm format, so another tool is needed under §8.1's licences.
3. **Auto-update from GitHub Releases** (#159, 3). The feed key needs a repository secret, which only the owner can add.
4. **Paid signing, last** (#159, 1). Azure Trusted Signing for Windows, when the rest of development is done.

## Revisit when

- The owner sets up Azure Trusted Signing, or asks for macOS signing after all.
- A Linux distribution beyond Ubuntu and Fedora becomes a target.
