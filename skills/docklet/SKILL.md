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

| Command                    | Purpose                                           |
| -------------------------- | ------------------------------------------------- |
| `docklet --help`           | List the commands available in this version       |
| `docklet version`          | Print the Docklet version                         |
| `docklet skills status`    | Show whether this skill is registered and current |
| `docklet skills install`   | Register or update this skill for AI agents       |
| `docklet skills uninstall` | Remove this skill                                 |

## Using Docker once the engine is running

Docker commands go through the `docklet` context:

```
docker --context docklet ps
docker --context docklet compose up -d
```

Published ports are reachable on `http://localhost:<port>` from Windows.
