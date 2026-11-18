# Building Blueprint

Blueprint builds with the Rust toolchain pinned in `rust-toolchain.toml`; `rustup` installs it on first use. Run `cargo test --workspace` to build and test everything.

## Linux

The named Linux targets are Ubuntu (current LTS) and Fedora (current release), on Wayland and X11 (REQUIREMENTS §5.1, #37). CI runs the tests on both: Ubuntu 24.04 natively, Fedora 45 in a container.

| What | Why | Ubuntu 24.04 | Fedora 45 |
|---|---|---|---|
| Rust installer | the pinned toolchain | `rustup` (or the script from rustup.rs) | `rustup`, then `rustup-init` |
| C compiler | tree-sitter grammars | `build-essential` | `gcc` |
| Git | tests build fixture repositories | `git` | `git` |

So, on a fresh machine:

```sh
# Ubuntu
sudo apt-get install -y build-essential git rustup
# Fedora
sudo dnf install -y gcc git rustup && rustup-init -y --default-toolchain none
```

### From M3: the desktop app

The app uses GPUI (DR 0003), which links the windowing, keyboard, font and Vulkan libraries. These are the packages the M0 spike needed; the M3 PR that adds GPUI to the workspace confirms them in CI on both distributions.

| Library | Ubuntu 24.04 | Fedora 45 |
|---|---|---|
| xkbcommon | `libxkbcommon-dev`, `libxkbcommon-x11-dev` | `libxkbcommon-devel`, `libxkbcommon-x11-devel` |
| Wayland | `libwayland-dev` | `wayland-devel` |
| X11 and XCB | `libx11-xcb-dev`, `libxcb1-dev` | `libX11-devel`, `libxcb-devel` |
| Fontconfig | `libfontconfig-dev` | `fontconfig-devel` |
| Vulkan (runtime) | `libvulkan1`, `mesa-vulkan-drivers` | `vulkan-loader`, `mesa-vulkan-drivers` |

## Windows and macOS

Install Rust with rustup. Windows also needs the MSVC build tools (the "Desktop development with C++" workload). macOS needs the Xcode command-line tools (`xcode-select --install`).
