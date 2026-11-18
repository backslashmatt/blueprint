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

### The desktop app

The app (`crates/app`, binary `blueprint`) uses GPUI (DR 0003), which links the windowing, keyboard, font and Vulkan libraries. CI installs exactly these packages on both distributions, builds the app and opens its window (#88).

| Library | Ubuntu 24.04 | Fedora 45 |
|---|---|---|
| xkbcommon | `libxkbcommon-dev`, `libxkbcommon-x11-dev` | `libxkbcommon-devel`, `libxkbcommon-x11-devel` |
| Wayland | `libwayland-dev` | `wayland-devel` |
| X11 and XCB | `libx11-xcb-dev`, `libxcb1-dev` | `libX11-devel`, `libxcb-devel` |
| Fontconfig | `libfontconfig-dev` | `fontconfig-devel` |
| Vulkan (runtime) | `libvulkan1`, `mesa-vulkan-drivers` | `vulkan-loader`, `mesa-vulkan-drivers` |

To run the app without a desktop, as CI does, also install Xvfb and openbox (`xvfb openbox` on Ubuntu, `xorg-x11-server-Xvfb openbox` on Fedora) and use `spikes/gpui/xvfb-run.sh target/debug/blueprint`. GPUI's X11 backend starts drawing only once a window manager reports the window visible.

## Windows and macOS

Install Rust with rustup. Windows also needs the MSVC build tools (the "Desktop development with C++" workload). macOS needs the Xcode command-line tools (`xcode-select --install`).
