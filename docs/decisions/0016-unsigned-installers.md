# 0016: Unsigned installers, the app alone

**Status:** accepted (agent decision, 2026-11-23). **Issues:** #162; signing in #159.

## Context

M5 needs "installers produced by CI", and §4 sets a 50 MB budget for installed size. Signing, notarisation and publishing need the owner (§8.1, asked in #159). Everything else can be built now.

Measured on Linux, in a release build with `lto = "fat"`:

| | `blueprint` (app) | `bp` (CLI) | Both |
|---|---|---|---|
| As built | 42.2 MB | 16.5 MB | 58.7 MB |
| Stripped | 37.4 MB | 15.6 MB | 53.0 MB |

## Options

1. **Install the app alone, stripped (chosen).** It installs 37.4 MB. §5.3 defines `cli` as the headless front end "used by tests and benchmarks", and the product a reviewer installs is the app.
2. **Install both.** That is 53 MB stripped, over budget. Fitting would mean shrinking both: optimising for size instead of speed, which speed budgets forbid, or merging the CLI into the app binary as a subcommand. Merging remains possible later if the owner wants `bp` installed.
3. **Strip in the release profile.** That would also strip the binaries the benchmarks and the Windows UI timings run, losing symbols in their backtraces for no gain. Instead the `package` job strips its own copy. On Windows, debug information is already in a separate `.pdb`, which is not packaged.

## Decision

- **Tool:** cargo-packager 0.11.8 (MIT/Apache-2.0), used as a CI tool only and never linked. Its configuration is `Packager.toml` at the root.
- **Packages:**
  - **Linux:** a `.deb`. Its dependencies are the libraries the app links and loads, plus `libc6 (>= 2.39)`, because a binary built on Ubuntu 24.04 needs glibc 2.39 (`pidfd_spawnp`). So Debian 12 and Ubuntu 22.04 are not supported by these packages.
  - **No AppImage:** its runtime is statically linked with libfuse (LGPL-2.1), a licence outside §8.1, so shipping one is the owner's decision (asked on #159).
  - **Windows:** an NSIS installer, installed per user, with zlib compression. The default LZMA would embed a decompressor under a licence outside §8.1, and NSIS itself is zlib-licensed. WiX is MS-RL, so there is no `.msi`.
  - **macOS:** a `.dmg`, once macOS CI returns (#133).
- **Notices:** the installers redistribute about 650 crates, whose MIT and Apache-2.0 licences ask for their notices to travel with the binary. CI generates `THIRD-PARTY-NOTICES.txt` with cargo-about 0.9.3 (MIT/Apache-2.0, a CI tool), accepting only the §8.1 licences (`packaging/about.toml`), so a crate under any other licence fails the job. The notices and both project licences are installed with the app.
- **Icon:** `packaging/icon.png`, drawn by a script, with three connected boxes on a blueprint grid. Linux uses it. On Windows, the installer and the `.exe` still have default icons, which needs an `.ico` and a build-script resource.
- **Installed size:** the job measures it from the real package and fails over 50 MB. See `docs/benchmarks.md`.
- **Distribution:** packages are workflow artifacts, never releases, until #159 is answered.

## Measured

- **Linux `.deb`:** installs 37.9 MB (`/usr/bin/blueprint`, a desktop entry, the icon, the licences and 434 KB of notices).
- **Windows NSIS:** the installer is 8.6 MB and installs 26.5 MB (CI, PR #168).

## Revisit when

- The owner answers #159 (signing, notarisation, auto-update).
- macOS CI returns.
- The app grows past about 45 MB installed. Then decide between size work and the budget.
- The owner wants `bp` installed: merge it into the app binary.
