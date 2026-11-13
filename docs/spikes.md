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

Method: `.github/workflows/spike-pipe.yml` runs `docklet-proxy` on a hosted
runner in message mode and, as a control, in byte mode. Every command has a
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
- Known gap: between accepting a connection and creating the next pipe
  instance a client can get "pipe busy". go-winio retries, .NET `Get-Acl` does
  not. Pre-creating a spare instance would close it.
