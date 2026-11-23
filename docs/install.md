# Installing Blueprint

CI builds installers on every run, as workflow artifacts (`installers-Linux` and `installers-Windows`) of the `package` job (#162). They are **unsigned**, and there are no releases yet: signing, notarisation and the update channel wait on the owner (#159).

| Platform | Package | Install |
|---|---|---|
| Linux (x64), glibc 2.39 or newer (Ubuntu 24.04, Fedora 40, Debian 13) | `blueprint_<version>_amd64.deb` | `sudo apt install ./blueprint_<version>_amd64.deb` |
| Windows (x64) | `blueprint_<version>_x64-setup.exe` | Run it. It installs for the current user, with no administrator rights. Windows SmartScreen warns about an unsigned installer; choose *More info*, then *Run anyway*. |
| macOS | none yet | Waits on macOS CI (#133) |

There is no AppImage yet: its runtime embeds libfuse, whose LGPL licence needs the owner's decision (#159). Fedora has no package yet either.

The installers contain the app, `blueprint`, with its licences (`LICENSE-MIT`, `LICENSE-APACHE`) and the third-party notices of the crates it is built from (`THIRD-PARTY-NOTICES.txt`, in `/usr/lib/blueprint` on Linux and in the install directory on Windows). Nothing else is installed. The `bp` command line is built from source (see [building.md](building.md)): it is the headless front end for tests and benchmarks, and including it would take the install over the 50 MB budget ([decision 0016](decisions/0016-unsigned-installers.md)).

On Linux the `.deb` depends on `libxcb1`, `libxkbcommon0`, `libxkbcommon-x11-0`, `libwayland-client0`, `libvulkan1` and `libfontconfig1`, which most desktops already have.
