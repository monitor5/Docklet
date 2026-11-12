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

Method: `.github/workflows/spike-wsl.yml`. Each step is a checkpoint.

Result: pending.

## Spike 2: message-mode named pipe and the Docker CLI half-close

Question: does a tokio message-mode named pipe let `docker.exe` (go-winio)
half-close its side, so that `echo hi | docker run -i --rm alpine cat` prints
`hi` and exits instead of hanging?

Method: `docklet-proxy` (crates/docklet-proxy) relaying to `socat` in the
`docklet` distro, compared against `--byte` mode.

Result: pending.
