# 0019: The update check's network access

**Status:** accepted, 2026-11-26. **Issue:** #179. **Builds on:** [0017](0017-owner-release-decisions.md) (updates from GitHub Releases) and [0018](0018-signed-update-manifest.md) (signed manifests).

## Context

Auto-update is the app's only network access. REQUIREMENTS §1.5 rules out accounts, servers and telemetry, and §2.5 says source code never leaves the machine. Downloading a manifest and an installer from this repository's releases breaks neither, provided nothing else is sent and the user can turn it off.

## Options (HTTP and TLS)

1. **ureq 3 with rustls and ring, certificates checked by `rustls-platform-verifier` (chosen).** Blocking and small. It uses the operating system's trust store on every platform: the native verifier on macOS and Windows, and the system certificates on Linux. No CA bundle is compiled in.
2. **reqwest.** It pulls in an async runtime (tokio and hyper) for two GET requests.
3. **native-tls.** It links OpenSSL dynamically on Linux, which adds a package dependency and a licence to track.
4. **`webpki-roots` for the CA list.** Its data is CDLA-Permissive-2.0, which is outside §8.1.

## Decision

- **Code:** `crates/update` gains `check` and `download`, written against a small `Fetch` trait, so they are tested with a fake GitHub. The HTTPS client is `net::Https`, behind the crate feature `net`, which only the app enables.
- **`check`:**
  - reads `releases/latest/download/manifest.json` and its `.sig`;
  - verifies them (DR 0018);
  - answers `NoRelease` on a 404, `UpToDate`, or `Available` with this platform's installer.
- **`download`:** reads at most the manifest's size for the asset, and returns the bytes only if their SHA-256 matches.
- **Limits:**
  - HTTPS only, at most 5 redirects, 300 s per request;
  - bodies capped at 64 KB for the manifest, 1 KB for the signature, and the stated size for an installer.
- **What is sent:** the GET requests and a `User-Agent: Blueprint/<version>` header. No identifiers, no repository data, nothing about the user.
- **Crates and licences** (all §8.1):
  - `ureq` and `ureq-proto`: MIT/Apache-2.0;
  - `rustls`: Apache-2.0/ISC/MIT;
  - `ring`: Apache-2.0 AND ISC;
  - `rustls-webpki` and `untrusted`: ISC;
  - `rustls-platform-verifier` and `rustls-native-certs`: MIT/Apache-2.0;
  - the platform crates (`security-framework`, `windows-sys`): MIT/Apache-2.0.

  cargo-about checks them on every OS once the app links them.
- **In the app (next slice):**
  - checks at most once a day, off the UI thread;
  - says so when an update is available, and installs only when the user asks;
  - a setting turns the check off, and the docs say what is sent.

## Measured

- **Binary size:** the `check` example with `net`, stripped, is 2.20 MB, against 0.48 MB for the same crate without the network stack. That is about **1.7 MB** more on the app. Linux packages would install about 39.6 MB of 50 MB.
- **Live check:** run against GitHub from a development container, with nothing released, it answers `NoRelease`.

## Revisit when

- The installed size nears the budget. Moving TLS to the platform's own stack would save some of the 1.7 MB.
- A proxy or enterprise network needs settings beyond the system's trust store.
