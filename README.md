# Docklet

A lightweight Windows tray app that installs and manages Docker Engine in a
dedicated WSL2 distro, with Kubernetes you can switch on and off. No Docker
Desktop required.

> Status: early development. Milestones M0 to M2 are done (engine lifecycle in
> the CLI, Windows `docker` CLI through the pipe). M3 (the native window and
> tray icon) is in review. Nothing here is ready for daily use yet.

## What works today

- **CLI** (`docklet.exe`): `up`, `status`, `wait`, `doctor`, `bases`, `install`,
  `start`, `stop`, `uninstall --yes`, `docker <args...>`, `config get|set`,
  `cli setup`, `skills status|install|uninstall`, `version`. `docklet up`
  installs, starts and sets up the Docker CLI in one step; `docklet docker ...`
  runs the Windows `docker` CLI against the engine without a path or a context.
  Every command takes `--json` and then prints one JSON object with a stable
  error code and the next command to run; `docklet help --json` lists the
  commands as data.
- **Idle stop**: an engine that carried no Docker traffic and ran no container
  for 10 minutes stops by itself and shows as `sleeping`; the next Docker
  command starts it again. `docklet config set idle-stop <minutes|off>` changes
  or disables this. See [Idle stop](#idle-stop).
- **Native app** (`docklet-native.exe`, Win32, one 1.8 MiB executable): a tray
  icon with Open, Start engine, Stop engine and Quit, and a window with the
  install screen, Containers, Images, Volumes and Networks lists, image pull
  and container logs. A strip shows what the engine uses (memory, CPU, GPU,
  disk) and the Containers list shows CPU, memory and I/O per container. Rows
  have check boxes with select-all in the header: the selection is removed in
  one step after a confirmation, and each tab can remove what is unused and
  report the space it got back. Closing the window leaves only the tray icon
  and gives the memory back.

## What does not work yet

- Bind mounts of Windows paths (`-v C:\project:/app`).
- Kubernetes.
- Registering the CLI on `PATH`.
- An installer. The native app is a single executable that is not packaged
  yet; the earlier installer came from the Tauri bundler, which is gone with
  the web window.
- An updater.

## How it works

- **Engine**: a dedicated WSL2 distro named `docklet` (by default Debian 13 +
  docker-ce, systemd; see [Engine bases](#engine-bases)). Your other distros are
  never touched; removing it is `wsl --unregister docklet`.
- **Connection**: a named pipe `\\.\pipe\docklet_engine`, restricted to the current
  user, relayed into the distro's `/var/run/docker.sock`. The GUI and the Windows
  `docker` / `docker compose` CLIs use the same pipe.
- **Kubernetes**: a single-node k3d cluster that is started and stopped on demand.
- **App**: a native Win32 window and tray icon (`apps/native`) on top of
  `docklet-app`, which holds everything that is not drawing. Measured on one
  PC: about 30 MB with the window open (see [docs/spikes.md](docs/spikes.md)).

## Engine bases

`docklet install --base <name>` picks the Linux base of the engine distro;
`docklet bases` lists them. All of them run the same Docker Engine API, so
containers, images and the Windows `docker` / `docker compose` CLIs behave the
same on each.

| Base                  | What it is                                                                  |
| --------------------- | --------------------------------------------------------------------------- |
| `debian-13` (default) | Debian 13, systemd, docker-ce from Docker's own repository                  |
| `ubuntu-26.04`        | Ubuntu 26.04, otherwise the same as `debian-13`                             |
| `alpine`              | Alpine Linux 3.24, no init system, Docker from Alpine's packages (unpinned) |

Pick `alpine` if you want the engine small and quick: a 76 MiB download instead
of 169 MiB, and about 3 s instead of 14 s until Docker answers after a start or
a wake (see [docs/spikes.md](docs/spikes.md)). The
trade-offs only concern the engine distro itself, not your containers: its
userland is musl and busybox, there is no systemd (a boot script starts dockerd
and nothing restarts it if it exits), Docker's version follows Alpine's
packages rather than Docker's own repository, and the buildx and compose CLI
plugins are not installed inside the distro. The Windows-side `docker compose`
is unaffected.

## Idle stop

The WSL VM is what an engine costs: around 1 GB of host memory even with no
container. So the pipe proxy stops the distro once nothing has used it for a
while, and keeps listening on the pipe.

- **Idle** means no data on any connection to the pipe and no running
  container. A running container keeps the engine up. A connection that is
  open but silent does not: Docker Desktop's build listener, an IDE's Docker
  view, a forgotten `docker events` or the open Docklet window. Those
  connections are closed when the engine goes to sleep.
- A program that was cut off that way and reconnects only to check or listen
  is held without waking the engine and is served at the next wake.
  `docklet config set watchers-keep-awake yes` restores the stricter rule
  that any open connection keeps the engine awake. `docklet doctor` names the
  program that keeps an engine awake.
- **Sleeping** is the state after that. `docklet status` prints `sleeping`,
  with `"ready": true` in JSON. Any Docker client that connects wakes the
  engine; that first command takes as long as an engine start. `status`,
  `doctor` and `wait` do not wake it.
- WSL itself keeps its VM for about another minute after the last distro stops,
  then the memory is returned.
- `docklet stop` and Stop in the app end the proxy too: the engine then stays
  off until it is started.
- `docklet config set idle-stop <minutes|off>` (default 10) applies from the
  next `docklet start`.

## Requirements

Windows 11 22H2 or later (x64) with WSL 2.4.4 or later.

## Development

Prerequisites: Rust (the pinned toolchain installs itself through `rust-toolchain.toml`)
and the MSVC C++ build tools. There is nothing else to install: no Node, no
WebView2.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p docklet-cli -p docklet-native   # target/release/docklet.exe, docklet-native.exe
```

## Layout

| Path                    | Contents                                                                 |
| ----------------------- | ------------------------------------------------------------------------ |
| `apps/native`           | The native Win32 app                                                     |
| `crates/docklet-app`    | Application logic without a UI toolkit: control, lists, selection, usage |
| `crates/docklet-cli`    | The `docklet` command line                                               |
| `crates/docklet-core`   | Platform-neutral logic: state machine, `wsl.exe` output parsing          |
| `crates/docklet-wsl`    | `wsl.exe` runner, registry lookup, keepalive                             |
| `crates/docklet-proxy`  | Named pipe server and relay                                              |
| `crates/docklet-engine` | Docker Engine API client (resources, events, logs, pulls)                |
| `skills/docklet`        | Agent skill (`SKILL.md`), embedded in the CLI                            |
| `distro`                | Root filesystem definition of the `docklet` distro                       |
| `docs/spikes.md`        | Results of technical spikes                                              |

## License

Apache-2.0
