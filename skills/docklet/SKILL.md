---
name: docklet
description: Manage Docker Engine and Kubernetes on Windows through Docklet (a dedicated WSL2 distro, no Docker Desktop). Use when the user wants to start or stop the Docker engine, check its status, run docker or docker compose against it, or switch the local Kubernetes cluster on or off on a machine where the `docklet` CLI is installed.
---

# Docklet

Docklet runs Docker Engine inside a dedicated WSL2 distro named `docklet` and
exposes it to Windows through the named pipe `\\.\pipe\docklet_engine`. It has
a tray GUI and a CLI, `docklet.exe`, that share the same core. Prefer the CLI:
it needs no window and prints plain text.

## Rules

- Run `docklet --help` first. It lists the commands this installed version
  supports. Do not assume a command exists if it is not listed.
- Never run `wsl --unregister`, `wsl --terminate` or `wsl --shutdown` yourself.
  Other WSL distros on the machine belong to the user. Use `docklet` commands.
- Do not edit `%USERPROFILE%\.wslconfig`.
- Ask before anything that deletes data (removing the distro, volumes, images
  or the Kubernetes cluster).

## Commands

| Command | Purpose |
| --- | --- |
| `docklet --help` | List the commands available in this version |
| `docklet status` | Engine state: `wsl-missing`, `not-installed`, `stopped`, `starting`, `running` or `error: ...` (first word of the output) |
| `docklet bases` | Linux bases available for the engine distro |
| `docklet install [--base NAME]` | Download and install the engine distro (once) |
| `docklet start` | Start the engine and wait until Docker is ready |
| `docklet stop` | Stop the engine; frees its memory |
| `docklet cli setup` | Install the Windows `docker` CLI and Compose if missing, create the `docklet` Docker context, and print the `docker.exe` path to use |
| `docklet uninstall --yes` | Delete the engine distro and all its data. Ask the user first |
| `docklet skills status` | Show whether this skill is registered and current |
| `docklet skills install` | Register or update this skill for AI agents |
| `docklet version` | Print the Docklet version |

Exit codes: 0 success, 1 the command failed (reason on stderr), 2 wrong usage.

Typical flow: `docklet status`; if `not-installed`, `docklet install`; if
`stopped`, `docklet start`; then use Docker.
## Using Docker once the engine is running

Run `docklet cli setup` once. It prints the `docker.exe` to use (the one on
PATH if there is one, otherwise the copy Docklet installed). Docker commands go
through the `docklet` context:

```
docker --context docklet ps
docker --context docklet compose up -d
```

Published ports are reachable on `http://localhost:<port>` from Windows.

Limitation: bind mounts of Windows paths (`-v C:\project:/app`) do not work yet.
Use named volumes, or paths inside the distro.