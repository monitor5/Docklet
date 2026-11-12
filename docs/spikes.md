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

Method: `.github/workflows/spike-wsl.yml` imports the released docklet rootfs
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
