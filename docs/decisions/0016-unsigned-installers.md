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
  - Linux: a `.deb` (with the libraries the app links and loads as dependencies) and an AppImage.
  - Windows: an NSIS installer, installed per user. NSIS is zlib-licensed. WiX is MS-RL, which is not on the §8.1 list, so there is no `.msi`.
  - macOS: a `.dmg`, once macOS CI returns (#133).
- **Icon:** `packaging/icon.png`, drawn by a script, with three connected boxes on a blueprint grid.
- **Installed size:** the job measures it from the real package and fails over 50 MB. See `docs/benchmarks.md`.
- **Distribution:** packages are workflow artifacts, never releases, until #159 is answered.

## Measured

- **Linux `.deb`:** installs 37.4 MB (`/usr/bin/blueprint`, a desktop entry and the icon). The package file is 12.7 MB.
- **Windows and the AppImage:** measured by the first CI run, and recorded on #162.

## Revisit when

- The owner answers #159 (signing, notarisation, auto-update).
- macOS CI returns.
- The app grows past about 45 MB installed. Then decide between size work and the budget.
- The owner wants `bp` installed: merge it into the app binary.
