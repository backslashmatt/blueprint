# 0018: Signed update manifests

**Status:** accepted, 2026-11-26. **Issue:** #179. **Builds on:** [0017](0017-owner-release-decisions.md): updates come from GitHub Releases.

## Context

The app will update itself from this repository's GitHub Releases. Anyone who can change what a release serves, such as a compromised account, token or download host, must not be able to push code to users. Platform signing does not cover this: it is deferred (DR 0017), and it says who built a file, not which file is the current release.

## Options

1. **A manifest signed with a key only CI holds (chosen).** Each release carries `manifest.json` (version, notes, and a URL, SHA-256 and size per platform) and `manifest.json.sig`. The signature is ed25519, over the manifest's exact bytes. The app holds the public key.
2. **Trust HTTPS and GitHub alone.** This is simpler, but anyone with write access to releases could push an update.
3. **A full update framework (TUF).** This handles key rotation and freezing attacks, but it is a large dependency and process for one key and one channel.

## Decision

- **Crate `crates/update`:** headless, with no I/O. It provides `sign`, `verify`, `newer` and `check_asset`.
  - `verify` checks the signature first (`verify_strict`, which rejects malleable signatures), then parses the manifest. It refuses:
    - any manifest over 64 KB;
    - any unknown field;
    - a version that is not semver;
    - an asset URL outside `https://github.com/backslashmatt/blueprint/releases/download/`;
    - a hash that is not 64 hex digits.
- **Only a stable, strictly newer version is an update.**
  - A replayed old manifest cannot downgrade the app.
  - A manifest whose version has a prerelease (`-alpha.1`) or build metadata (`+7`) is refused: semver orders build metadata after the same version, and stable users must not be offered an unstable build signed with the same key.
- **Asset URLs** must be plain paths under the releases prefix: segments of letters, digits, `.`, `_`, `-` and `+`, none empty or `..`. So no escapes, queries, fragments or userinfo.
- **Downloads** must match the manifest's size and SHA-256 before anything is installed.
- **The key:**
  - The secret is a 32-byte seed in hex, in the repository secret `UPDATE_SIGNING_KEY`, which only the owner can add.
  - `cargo run -p xtask -- update-keygen` makes the pair from the operating system's randomness. It prints the seed and the public key, and writes nothing.
  - The public key will be committed as `crates/update/release-key.txt` and compiled into the app.
- **Crates:**
  - `ed25519-dalek` 2.2 with `curve25519-dalek` 4.1 (BSD-3-Clause);
  - `ed25519` and `signature` (MIT/Apache-2.0);
  - `sha2`, `semver` and `hex`, which were already in the lockfile.

## Accepted risks

- **Freeze:** whoever controls the download host can keep serving an old valid manifest, so users simply do not update.
- **Holding back:** they can serve a genuine but older release (0.3.0 when 0.4.0 exists) to users on something older still.
- Nothing in the manifest expires or revokes a release. Signing cannot stop withholding. An `expires` field would only turn it into a warning, at the cost of re-signing on a schedule. Accepted for one channel and one key; revisit with TUF below.

## Measured

- **Build time:** a clean release build of the new crates takes 11.9 s on 4 cores.
- **Binary size:** none yet. The app does not link the crate until the updater slice, which will measure installed size against 50 MB.

## Revisit when

- A second update channel or signer is needed, or the key must be rotated. Then add a key list to the app, or move to TUF.
- The owner's platform signing (DR 0017) arrives. The manifest stays; platform signatures are checked as well.
