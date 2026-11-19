# Spikes

Results of the technical spikes that back the decisions in the development plan.

## Environment used for local runs

| Item    | Value                                                                           |
| ------- | ------------------------------------------------------------------------------- |
| Windows | 11 Pro, build 10.0.26300.9711                                                   |
| WSL     | 3.0.5.0, kernel 6.18.58.1-1                                                     |
| Note    | `.wslconfig` on this machine points `kernel=` at a custom NVIDIA JetPack kernel |

## Spike 1: WSL2 + dockerd on a GitHub-hosted runner

Question: can `windows-2025` hosted runners run dockerd inside a WSL2 distro, so
that WSL end-to-end tests can run in CI?

Method: a workflow (since replaced by `.github/workflows/e2e.yml`) imported the released docklet rootfs
with `wsl --import ... --version 2` and checks each stage.

Result (2026-11-12): **yes.** The runner had WSL 3.0.3.0 with kernel
6.18.54.1 on Windows 10.0.26100. The import succeeded, systemd started
dockerd 29.9.3 (overlayfs, cgroup v2) and `docker run --rm hello-world` passed.

Consequence: WSL end-to-end tests run in CI on hosted runners. The plan's
fallback (Docker tests on `ubuntu-latest`, WSL tests only locally) is not needed.

## Spike 2: message-mode named pipe and the Docker CLI half-close

Question: does a tokio message-mode named pipe let `docker.exe` (go-winio)
half-close its side, so that `echo hi | docker run -i --rm alpine cat` prints
`hi` and exits instead of hanging?

Method: a workflow (since removed; the same checks live on in
`.github/workflows/e2e.yml`) ran `docklet-proxy` on a hosted runner in message
mode and, as a control, in byte mode. Every command has a
hard timeout.

Result (2026-11-12): **yes, and message mode is required.**

| Check                                      | Message mode | Byte mode        |
| ------------------------------------------ | ------------ | ---------------- |
| `docker version`                           | pass, ~0.2 s | pass             |
| `docker run --rm hello-world`              | pass, ~2.2 s | pass             |
| `echo hi \| docker run -i --rm alpine cat` | pass, ~0.8 s | hangs (expected) |
| Proxy CPU while idle afterwards            | 0 s in 10 s  | 0 s in 10 s      |

A new connection costs roughly 0.2 s end to end (`docker version`), which is
the price of one `wsl.exe` spawn per connection.

What it took to get there:

1. **Zero-length reads are ambiguous.** go-winio's half-close arrives as one
   zero-byte message, after which reads block again. After a disconnect,
   however, every read completes immediately with zero bytes. The first
   version of the relay treated both as a half-close and retried forever,
   which pinned one CPU core per finished connection and left its `wsl.exe`
   relays running. On the development machine this made the whole system
   unresponsive twice. Rule now: the first zero-length read closes the relay's
   stdin, the second one ends the connection.
2. **`ChildStdin::shutdown` does not close the pipe.** The handle has to be
   dropped for the relay to see EOF.
3. **Neither socat nor nc can be the in-distro relay.** dockerd ends an attach
   stream with a half-close of its own. socat keeps stdout open for its whole
   `-t` timeout, and `-t` cannot be short because the container may keep
   writing long after the client's stdin ended. nc only exits once both
   directions are done. Measured without the proxy (HTTP/1.0 `/_ping`, stdin
   left open): socat with `-t 86400` never delivered end-of-stream, also not
   with `shut-close`, and `wsl.exe` passes end-of-stream on only when the
   Linux process exits. The rootfs therefore ships `docklet-relay`
   (`distro/relay`), which half-closes the socket on stdin EOF and exits on
   socket EOF. The plan had this binary as a later optimisation; it is needed
   from the start.

Rule for running this kind of test: anything that loops over a pipe runs on a
hosted runner with a timeout first, and only then on a developer machine.

## M1 measurements

Engine lifecycle through the CLI (`docklet install`, `start`, `stop`,
`uninstall`), measured 2026-11-14.

| Measurement                                  | Development PC (kernel 6.18.58.1) | Hosted runner |
| -------------------------------------------- | --------------------------------- | ------------- |
| `docklet install` (download, verify, import) | 28 s                              | not timed     |
| `docklet start` until dockerd answers        | 12 s                              | about 15 s    |
| `docklet stop`                               | 0.6 s                             | not timed     |
| Distro lifetime **without** keepalive        | about 16 s                        | about 25 s    |
| Distro still up 40 s after `start`           | yes                               | yes (45 s)    |

- Without a process held from Windows, WSL stops the systemd distro within
  half a minute even though dockerd is running, so the keepalive is required.
- The keepalive must be created without inherited handles. Otherwise it keeps
  the CLI's stdout pipe open and whatever reads `docklet start` never sees EOF.
- Both bases (Debian 13, Ubuntu 26.04) pass the same E2E with dockerd 29.9.3,
  overlayfs and cgroup v2. Nothing in the engine code depends on the base.
- Distro names are read with `wsl --list --quiet`, which prints names only and
  is not localized, instead of the registry lookup the plan proposed.
- Known cosmetic issue: `wsl.exe` prints "Failed to start the systemd user
  session for 'root'" when entering the distro. It does not affect dockerd.

## M2 measurements

Windows `docker` CLI 29.9.3 through `\\.\pipe\docklet_engine`, measured
2026-11-14 on the development PC and in the E2E workflow (both bases).

| Check                                                        | Development PC                 | Hosted runner |
| ------------------------------------------------------------ | ------------------------------ | ------------- |
| `docker --context docklet version` (Server 29.9.3)           | pass, 0.6 s                    | pass, 0.3 s   |
| `docker run --rm hello-world`                                | pass, 0.9 s                    | pass, 1.2 s   |
| `echo hi \| docker run -i --rm alpine cat`                   | pass                           | pass          |
| `docker run -d -p 8080:80 nginx`, then `curl localhost:8080` | 200                            | 200           |
| `docker save python:3.13` (412 MB)                           | 11.3 s, 37 MB/s                | pass          |
| Proxy while idle                                             | 0 CPU s in 5 s, 8 MB, 0 relays | pass          |

- The pipe's DACL is `D:P(A;;FA;;;SY)(A;;FA;;;<current user>)`: only SYSTEM and
  the current user have access. A test from a second Windows account was not
  run because the machine has no second account.
- 37 MB/s for bulk transfer is the cost of going through `wsl.exe` stdio. It is
  fine for API traffic and logs; large `docker save`/`load`/build contexts are
  where a faster transport (hvsocket) would pay off.
- Known gap, narrowed in M3: with a single pipe instance a client could get
  "pipe busy" between the proxy accepting a connection and creating the next
  instance. go-winio retries, .NET `Get-Acl` does not. The proxy now keeps two
  unconnected instances (`crates/docklet-proxy/src/server.rs`) and replaces an
  instance as soon as a client connects to it. A burst larger than the number
  of free instances can still see "pipe busy" for a moment. The runner-only
  test `concurrent_clients_never_get_pipe_busy` covers this; it has not run yet.

## M3 measurements

Release build of the `m3` branch, measured 2026-11-15 on a second PC (AMD
8C/16T, 23 GB RAM, Windows build 26300, WSL 3.0.5) after the E2E workflow
passed. The window was driven by UI automation and was not in the foreground.

| Check                                                    | Result                                                            |
| -------------------------------------------------------- | ----------------------------------------------------------------- |
| Container created with the CLI shows up in the window    | pass: listed with status "Up Less than a second"                  |
| Log view with a container writing about 10,000 lines/s   | pass: follows the tail, buffer stays at 50,000 lines              |
| CPU while following that log, 10 s                       | 0.4 s desktop process, about 1.6 s WebView2, 0.3 s CLI proxy      |
| `msedgewebview2.exe` children after closing the window   | 0; the tray process stays (7 MB private, 32 MB working set)       |
| Relay processes after closing the window with a log open | the log's relay ends, the event stream's relay stays              |
| `docklet-desktop.exe --proxy` serves the Docker pipe     | pass in the E2E job `desktop-proxy` (0 CPU s idle, no relay left) |
| After `docklet stop` with the tray app running           | no distro, proxy or relay left; tray process uses 0 CPU s in 10 s |

Not measured yet:

- [ ] Stopping and starting the engine from the tray menu. The tray was not
      driven; the window's own Start and Stop buttons were not clicked either.
- [ ] The log view with the window in the foreground, where rendering runs at
      the display's frame rate.
- `docklet stop` took about 10 s in this run, against 0.6 s in the M1
  measurement. Three containers had just been removed; the cause was not
  looked into.
- The container list shows the status text from the last Docker event, so
  "Up 13 seconds" stays on screen until something else changes.

## Alpine base without an init system

Question: does the engine distro need systemd, and what does a smaller base
save?

What Docklet needs from a rootfs (the rootfs contract) is only:

- dockerd running after the distro boots, listening on `/var/run/docker.sock`;
- `/usr/local/bin/docklet-relay`;
- a `docker` CLI inside the distro, because the engine probes readiness with
  `docker info` and counts running containers with `docker ps -q`.

How dockerd gets started is up to the base. On the Debian-family bases
systemd does it; nothing else in the engine relies on systemd.

Method (2026-11-15): an Alpine rootfs without an init system was built by hand,
with Docker from Alpine 3.24's packages and a WSL `[boot]` command that starts
dockerd. Both it and the Debian 13 rootfs were measured the same way on the
same PC: a cold WSL VM for each start, no container running. Each number is from **one run on one PC**;
treat them as an order of magnitude, not a benchmark.

| Measurement                      | Debian 13 + systemd      | Alpine, no init system |
| -------------------------------- | ------------------------ | ---------------------- |
| Rootfs download (gzip)           | 169 MiB                  | 59 MiB                 |
| Installed size                   | 598 MB                   | 174 MB                 |
| Cold start until dockerd answers | 14.1 s (`docklet start`) | 2.6 s                  |
| Host memory of the WSL VM, idle  | 1402 MB                  | 1168 MB                |
| Processes in the distro          | 20                       | 11                     |

In the Alpine distro, Docker 29.9.3 passed `docker run hello-world`, the stdin
half-close (`echo hi | docker run -i --rm alpine cat`), a `--memory` limit
(cgroup v2) and a published port reached from Windows on `localhost`.

Consequence: `alpine` is a third base (`docklet install --base alpine`);
`debian-13` stays the default. Its recipe in `distro/Dockerfile` differs from
the hand-built experiment in a few points:

- `distro/apk/docklet-boot` sets `PATH` (without it dockerd cannot find
  `docker-proxy`), enables the cgroup v2 controllers for delegation as moby's
  docker-in-docker script does, makes `/` a shared mount as systemd does on
  the other bases, and logs through busybox `syslogd` to `/var/log/messages`,
  rotated at 2 MiB with two old files kept, instead of a log file that grows
  for as long as the distro runs.
- The `docker` CLI and k3d are installed; the buildx and compose CLI plugins
  are not. They would only serve a `docker` CLI run inside the distro; the
  Windows-side `docker compose` is unaffected.
- Nothing restarts dockerd if it exits; systemd would. `docklet stop` and
  `docklet start` boot the distro again.

The rootfs built by `distro.yml` is 75.5 MiB (gzip), larger than the hand-built
one because it carries the `docker` CLI and k3d. Not measured yet: k3d clusters
on it.

## Idle stop and wake on connect

Question: does stopping an unused engine give the memory back, and what does
the next Docker command pay for it?

GitHub-hosted runner (2026-11-15, E2E run 43709548713, idle time set to 20 s
for the test):

| Base         | `docklet start` | Wake by `docker version` | Wake by `docklet docker version` |
| ------------ | --------------- | ------------------------ | -------------------------------- |
| alpine       | 5.9 s           | 2.9 s                    | 2.9 s                            |
| debian-13    | 19.7 s          | 14.0 s                   | 14.7 s                           |
| ubuntu-26.04 | 15.4 s          | 14.0 s                   | 13.9 s                           |

`docklet-desktop.exe --proxy` (debian-13) fell asleep after about 31 s and woke
in 16.0 s.

Local PC (2026-11-15, release build, one run per base, idle time 20 s):

| Measurement                                | debian-13 | alpine |
| ------------------------------------------ | --------- | ------ |
| `docklet start`                            | 13.6 s    | 2.1 s  |
| Wake by `docker --context docklet version` | 13.1 s    | 3.2 s  |
| `docker ps` on a running engine            | 0.29 s    | 0.28 s |
| `docklet stop`                             | 1.1 s     | 0.6 s  |

- The host's `vmmemWSL` process does not shrink when the distro stops: WSL
  keeps the VM for about a minute (it showed 685 to 966 MB in that window),
  then the process is gone. With the proxy still listening, `vmmemWSL` was at
  0 MB 75 s after the engine fell asleep.
- While sleeping, the only Docklet process was the proxy (8 MB working set,
  2 MB private) and no `wsl.exe` was left.
- The two columns are not like for like. The alpine start began while the VM
  was still up from the import. The alpine wake started from a VM that was
  fully gone; the debian-13 wake came seconds after falling asleep, with the
  VM still up.
- Not measured: a debian-13 wake from a VM that is fully gone, memory of the
  running engine per base in this build, and the desktop window on a sleeping
  engine.

## Native Win32 window instead of WebView2

Question: how much lighter is a window that is not a web page, with the same
screens?

Method (2026-11-15 and 16, one PC, release builds, 125% display scaling): the
Tauri window, two throwaway prototypes of one screen (Win32 with the
`windows` crate, and Slint), then the real native app. `docklet-native.exe`
was measured in its demo mode (made-up data, a log of 10,000 lines per
second) and against a real engine on the alpine base. Working set and
private bytes of the app's own processes; CPU as a share of one core over
5 to 15 s.

| State                                   | Tauri + WebView2                     | Native app                    |
| --------------------------------------- | ------------------------------------ | ----------------------------- |
| Executable                              | 4.1 MB plus WebView2                 | 1.8 MiB                       |
| Window open on a list                   | 357 MB (148 MB private), 7 processes | 28 MB (8 MB private)          |
| Window closed, tray only                | 34 MB (7 MB private)                 | 0 to 1 MB (3 to 9 MB private) |
| Started in the tray, window never shown | not measured                         | 12 MB (2 MB private)          |
| Log at 10,000 lines per second, visible | about 20% (window in the background) | 3.0 to 3.8%                   |
| Same log, window minimised              | not measured                         | about 1%                      |
| Real log, about 20 lines per second     | not measured                         | 0.3 to 6.3%, five samples     |

- The prototypes: Win32 14 MB with the list idle, Slint 37 MB with its
  software renderer and 252 MB with OpenGL while following the log. egui,
  iced and WinUI 3 were ruled out on paper (memory, an open Korean IME
  defect, no tray or accessibility support).
- Private bytes do not return to the 2 MB of a fresh start after the window
  was open once: reading the GPU counters (PDH) keeps about 5 MB for the
  life of the process.
- On the real engine: the lists, usage strip and per-container columns,
  select-all with a filter (hidden rows were kept), bulk removal with its
  confirmation and report, prune with the reclaimed size, disk usage, pull,
  logs, open port, the tray menu and single instance all worked.
- Not verified: typing Hangul through the IME (text was set, not typed), a
  screen reader (only the UI Automation names were read), a click on the
  real tray icon (its notification was sent to the window instead), and the
  sleeping screen on a real engine.

## Idle stop with a watcher on the pipe

Docker Desktop's app, with its own engine stopped, opens a build-history
listener on every Docker context, Docklet's included
(`com.docker.build.exe`: "starting listener for docklet/docklet"). With
"no open connection" as the idle rule that one connection kept the engine
awake indefinitely: 106 s with a 20 s idle time and no container, on a PC
where it had slept the day before while that listener happened to be down.

The rule is now traffic: an engine sleeps when no connection carried data
and no container ran for the idle time, silent connections are closed at
that point, and a program that was cut off and reconnects only to check or
listen is held without waking the engine.

- Runner (alpine): asleep after about 21 s with `docker events` and a
  reconnecting watcher attached; the held reconnect cost no CPU in 30 s;
  `docker version` woke the engine in 5.7 s and the held watcher was then
  served.
- Local PC: asleep 26 s after start with `docker events` attached, which
  ended with "unexpected EOF"; still asleep after 3.5 minutes; woken by
  `docker version` in 2.2 s.
- Not verified: Docker Desktop itself reconnecting to a sleeping engine. Its
  listener did not reconnect during the test, so what its first request is,
  and whether it is held, is unknown.
