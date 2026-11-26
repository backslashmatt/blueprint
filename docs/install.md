# Installing Blueprint

CI builds installers on every run, as workflow artifacts (`installers-Linux`, `installers-Windows` and `installers-macOS`) of the `package` job (#162). They are **unsigned**: the owner chose to set up paid signing last ([decision 0017](decisions/0017-owner-release-decisions.md)). There are no releases yet; updates will come from GitHub Releases.

| Platform | Package | Install |
|---|---|---|
| Ubuntu and Debian (x64), glibc 2.39 or newer (Ubuntu 24.04, Debian 13) | `blueprint_<version>_amd64.deb` | `sudo apt install ./blueprint_<version>_amd64.deb` |
| Fedora (x64), glibc 2.39 or newer (Fedora 40 and later; CI installs it on Fedora 45) | `blueprint-<version>-1.x86_64.rpm` | `sudo dnf install ./blueprint-<version>-1.x86_64.rpm` |
| Windows (x64) | `blueprint_<version>_x64-setup.exe` | Run it. It installs for the current user, with no administrator rights. Windows SmartScreen warns about an unsigned installer; choose *More info*, then *Run anyway*. |
| macOS (Apple silicon) | `Blueprint_<version>_aarch64.dmg` | Open it, accept the licence, and drag Blueprint to Applications. It is not notarised, so macOS blocks the first launch: open System Settings › Privacy & Security and choose *Open Anyway* next to Blueprint. If macOS says the app is damaged, run `xattr -dr com.apple.quarantine /Applications/Blueprint.app` once. |

There is no AppImage: its runtime embeds libfuse, whose LGPL licence is outside the allowed list, and the owner chose to leave it out (#159).

The installers contain the app, `blueprint`, with its licences (`LICENSE-MIT`, `LICENSE-APACHE`) and the third-party notices of the crates it is built from (`THIRD-PARTY-NOTICES.txt`, in `/usr/lib/blueprint` on Linux and in the install directory on Windows). Nothing else is installed. The `bp` command line is built from source (see [building.md](building.md)): it is the headless front end for tests and benchmarks, and including it would take the install over the 50 MB budget ([decision 0016](decisions/0016-unsigned-installers.md)).

On Linux the `.deb` depends on `libxcb1`, `libxkbcommon0`, `libxkbcommon-x11-0`, `libwayland-client0`, `libvulkan1` and `libfontconfig1`, and the `.rpm` on `libxcb`, `libxkbcommon`, `libxkbcommon-x11`, `libwayland-client`, `vulkan-loader` and `fontconfig`, which most desktops already have. The `.rpm` is built with cargo-generate-rpm (MIT), as cargo-packager has no rpm format; its contents and dependencies are in `crates/app/Cargo.toml`.
