# Docklet

A lightweight Windows tray app that installs and manages Docker Engine in a
dedicated WSL2 distro, with Kubernetes you can switch on and off. No Docker
Desktop required.

> Status: early development (milestone M0). Nothing here is ready for daily use yet.

## How it works

- **Engine**: a dedicated WSL2 distro named `docklet` (Debian 13 + docker-ce, systemd).
  Your other distros are never touched; removing it is `wsl --unregister docklet`.
- **Connection**: a named pipe `\\.\pipe\docklet_engine`, restricted to the current
  user, relayed into the distro's `/var/run/docker.sock`. The GUI and the Windows
  `docker` / `docker compose` CLIs use the same pipe.
- **Kubernetes**: a single-node k3d cluster that is started and stopped on demand.
- **App**: Tauri 2 (Rust core + WebView2). Closing the window destroys the WebView
  and leaves only the tray process.

## Requirements

Windows 11 22H2 or later (x64) with WSL 2.4.4 or later.

## Development

Prerequisites: Rust (the pinned toolchain installs itself through `rust-toolchain.toml`),
the MSVC C++ build tools, Node 24 or later, and pnpm.

```sh
pnpm install
pnpm dev        # run the app
pnpm lint       # Biome
pnpm test       # Vitest
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm build      # NSIS installer in target/release/bundle/nsis
```

## Layout

| Path                    | Contents                                                        |
| ----------------------- | --------------------------------------------------------------- |
| `apps/desktop`          | React UI and the Tauri shell (`src-tauri`)                      |
| `crates/docklet-core`   | Platform-neutral logic: state machine, `wsl.exe` output parsing |
| `crates/docklet-wsl`    | `wsl.exe` runner, registry lookup, keepalive                    |
| `crates/docklet-proxy`  | Named pipe server and relay                                     |
| `crates/docklet-engine` | Docker Engine client wrapper                                    |
| `distro`                | Root filesystem definition of the `docklet` distro              |
| `docs/spikes.md`        | Results of technical spikes                                     |

## License

Apache-2.0
