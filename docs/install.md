# Installing Blueprint

CI builds installers on every run, as workflow artifacts (`installers-Linux` and `installers-Windows`) of the `package` job (#162). They are **unsigned**, and there are no releases yet: signing, notarisation and the update channel wait on the owner (#159).

| Platform | Package | Install |
|---|---|---|
| Linux (x64) | `blueprint_<version>_amd64.deb` | `sudo apt install ./blueprint_<version>_amd64.deb` |
| Linux (x64) | `blueprint_<version>_x86_64.AppImage` | `chmod +x` it, then run it |
| Windows (x64) | `Blueprint_<version>_x64-setup.exe` | Run it. It installs for the current user, with no administrator rights. Windows SmartScreen warns about an unsigned installer; choose *More info*, then *Run anyway*. |
| macOS | none yet | Waits on macOS CI (#133) |

The installers contain the app, `blueprint`, and nothing else. The `bp` command line is built from source (see [building.md](building.md)): it is the headless front end for tests and benchmarks, and including it would take the install over the 50 MB budget ([decision 0016](decisions/0016-unsigned-installers.md)).

On Linux the `.deb` depends on `libxcb1`, `libxkbcommon0`, `libxkbcommon-x11-0`, `libwayland-client0`, `libvulkan1` and `libfontconfig1`, which most desktops already have.
