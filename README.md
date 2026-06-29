# SSH Manager

A simple SSH connection manager (GTK4 + VTE) with an encrypted server store,
grouped hosts, and an embedded terminal.

## Features

- **Encrypted store** — servers are saved to `~/.config/ssh-manager/servers.age`,
  encrypted with [`age`](https://age-encryption.org/) (scrypt passphrase).
- **Master password** on startup, with a doubling lockout after wrong attempts
  (3 tries → 1 min → 2 min → 4 min …), persisted across restarts.
- **Groups** — nested categories via `/`, e.g. `prod/db`, rendered as a tree.
- **Auth** — password (via `sshpass`), private key file (`ssh -i`), or default
  agent/keys.
- **Embedded terminal** with `Ctrl+Shift+C` / `Ctrl+Shift+V` copy-paste and a
  right-click menu.
- Right-click a server to **Edit / Delete**; **Options** and **Help** menus.

## Project structure

```
SimpleSSHManager/
├── Cargo.toml        # crate manifest + dependencies
├── Cargo.lock
├── assets/
│   ├── icon.png      # app icon source
│   └── cloud.sadeem.SimpleSshManager.desktop
└── src/
    ├── main.rs       # app entry, main window, sidebar, menus, terminal tabs
    ├── login.rs      # master-password gate + lockout dialog
    ├── store.rs      # encrypted load/save, Server model, lockout state (+ tests)
    └── center.rs     # X11 window centering (no-op on Wayland)
```

## Runtime requirements

- `ssh` (OpenSSH client)
- `sshpass` — only needed for password auth
- GTK4 + VTE4 runtime libraries

## Build

System libraries (Debian/Ubuntu):

```bash
sudo apt install libgtk-4-dev libvte-2.91-gtk4-dev
```

Build and run:

```bash
cargo build            # debug build
cargo run              # build + launch
cargo build --release  # optimized binary at target/release/simple-ssh-manager
cargo test             # run unit tests (store encryption, lockout, grouping)
```

### Install icon + launcher (optional)

```bash
APP=cloud.sadeem.SimpleSshManager
for s in 16 32 48 64 128 256 512; do
  d="$HOME/.local/share/icons/hicolor/${s}x${s}/apps"; mkdir -p "$d"
  convert assets/icon.png -resize ${s}x${s} "$d/$APP.png"
done
cp assets/$APP.desktop "$HOME/.local/share/applications/"
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor"
```

## Build a .deb package

Uses [`cargo-deb`](https://crates.io/crates/cargo-deb); packaging metadata
(runtime deps, binary, icon, desktop file) lives in `Cargo.toml`.

```bash
cargo install cargo-deb        # once
cargo deb                      # -> target/debian/simple-ssh-manager_0.1.0_amd64.deb
```

Install / uninstall:

```bash
sudo apt install ./target/debian/simple-ssh-manager_*.deb
sudo apt remove simple-ssh-manager
```

## Author

Mohamed Fawzy — <mf.elsaigh@gmail.com> — https://sadeem.cloud
Repo: https://github.com/mf-elsaigh/simple-ssh-manager.git
