---
name: docklet
description: Manage Docker Engine and Kubernetes on Windows through Docklet (a dedicated WSL2 distro, no Docker Desktop). Use when the user wants to start or stop the Docker engine, check its status, run docker or docker compose against it, or switch the local Kubernetes cluster on or off on a machine where the `docklet` CLI is installed.
---

# Docklet

Docklet runs Docker Engine inside a dedicated WSL2 distro named `docklet` and
exposes it to Windows through the named pipe `\\.\pipe\docklet_engine`. It has
a tray GUI and a CLI, `docklet.exe`, that share the same core. Use the CLI: it
needs no window, never prompts, and answers in JSON when asked.

## The flow

```
docklet up --json               # install if needed, start, set up the Docker CLI
docklet docker ps               # any docker command, against the Docklet engine
docklet docker compose up -d
```

`docklet up` is safe to repeat: it does only what is still missing and reports
what it did in `data.changed`. After it succeeds, `docklet docker ...` works.
There is no need to find `docker.exe` or to pass `--context`.

When a command fails, read `error.code`, and run `error.hint` if there is one.
When that does not help, run `docklet doctor --json`.

An engine nobody uses goes to sleep after a while (10 minutes by default) and
wakes up by itself with the next Docker command. That is normal and needs no
action; see "Idle stop" below.

## Rules

- Run `docklet help --json` once to see the commands this installed version
  supports. Do not assume a command exists if it is not listed.
- Never run `wsl --unregister`, `wsl --terminate` or `wsl --shutdown` yourself.
  Other WSL distros on the machine belong to the user. Use `docklet` commands.
- Do not edit `%USERPROFILE%\.wslconfig`.
- Ask before anything that deletes data (removing the distro, volumes, images
  or the Kubernetes cluster). `docklet help --json` marks such commands with
  `deletes_data: true`; they refuse to run without `--yes`
  (`confirmation_required`). Docker commands that delete data, such as
  `docklet docker volume rm` or `docklet docker system prune`, need the same
  care.
- Do not run the hint of `wsl_missing` or `wsl_outdated` yourself. Installing
  or updating WSL needs an administrator and may need a reboot: tell the user.
- Give commands marked `long_running: true` a timeout of minutes, not seconds.

## JSON output

Every command accepts `--json`, anywhere on the command line. Stdout then
carries exactly one JSON object on one line; progress and warnings go to
stderr.

```
{"ok":true,"command":"status","data":{...}}
{"ok":false,"command":"start","error":{"code":"not_installed","message":"...","hint":"docklet install"}}
```

- `ok` is true exactly when the exit code is 0.
- `error.code` is stable; branch on it, not on `message`.
- `error.hint` is the next command to run. It is absent when there is no
  obvious one.
- A failed `doctor` or `skills` command also has `data`, with what it found.
- `command` is `null` when the command line names no known command.

Exit codes: 0 success, 1 the command failed, 2 the command line is wrong and
nothing was done. Without `--json` the result is plain text on stdout and
failures are one line on stderr.

`docklet docker` is the exception: what follows `docker` belongs to Docker, so
its stdin, stdout, stderr and exit code are Docker's. `--json` only applies
when it comes before the word `docker`, and then only to failures that happen
before Docker runs (`not_running`, `docker_cli_missing`).

## Commands

| Command | Purpose | `data` with `--json` |
| --- | --- | --- |
| `docklet up [--base NAME]` | Install if needed, start, set up the Docker CLI and context | `state`, `docker_host`, `pipe`, `docker_cli`, `docker_context`, `changed` |
| `docklet docker <args...>` | Run `docker.exe` against the Docklet engine. Wakes a sleeping engine; never starts a stopped one | Docker's own output |
| `docklet status` | Engine state | `state`, `reason`, `ready`, `hint`, `proxy_running`, `idle_stop`, `pipe`, `docker_host`, `distro`, `rootfs_version`, `wsl_version`, `docklet_version`, `mirrored_networking`, `docker_context`, `docker_cli` |
| `docklet wait [--timeout SECONDS]` | Block until the engine is usable (default 120 s): running, or sleeping. Starts nothing | `state`, `docker_host`, `pipe`, `waited_seconds` |
| `docklet doctor` | Check the setup; exit 1 if a check fails | `healthy`, `summary`, `checks` |
| `docklet start` | Start the engine, or wake it, and wait until Docker is ready | `state`, `docker_host`, `pipe` |
| `docklet stop` | Stop the engine; frees its memory. It stays off until started | `proxy_running` |
| `docklet config get [<key>]` | Show the settings, or one | `key`, `value`, `minutes`; without a key, `settings` with one such entry each |
| `docklet config set <key> <value>` | Change a setting: `idle-stop` takes a number of minutes or `off`. Applies from the next `docklet start` | `key`, `value`, `minutes`, `applies_from`, `restart_needed` |
| `docklet install [--base NAME]` | Download and install the engine distro (`up` does this too) | `distro`, `base`, `rootfs_file` |
| `docklet bases` | Linux bases available for the engine distro | `default`, `bases` |
| `docklet cli setup` | Install the Windows `docker` CLI and Compose if missing, create the `docklet` Docker context (`up` does this too) | `docker_host`, `docker_cli`, `docker_context`, `changed` |
| `docklet uninstall --yes` | Delete the engine distro and all its data. Ask the user first | `distro` |
| `docklet skills status` | Show whether this skill is registered and current | `agents` |
| `docklet skills install` | Register or update this skill for AI agents | `agents` |
| `docklet skills uninstall` | Remove this skill | `agents` |
| `docklet version` | Print the Docklet version | `version` |
| `docklet help` | List the commands; with `--json`, the catalogue with arguments, `deletes_data`, `long_running` and the error codes | `commands`, `options`, `exit_codes`, `error_codes` |

`status` reports `state` as one of `wsl_missing`, `not_installed`, `stopped`,
`sleeping`, `starting`, `running`, `stopping` or `error`. `ready` is true when
Docker commands will work: in `running`, and in `sleeping`, where the first
command takes a few seconds longer because it wakes the engine. `idle_stop`
says whether the engine will go to sleep when unused. Without `--json` the
first word of the output is the same state, written with a hyphen
(`not-installed`). `up` and `wait` report `state` as `running` or `sleeping`.

`doctor` returns one entry per check: `id`, `title`, `status` (`ok`, `warn` or
`fail`), `detail`, and `fix` (a command) when there is one. The checks are
`wsl`, `distro_installed`, `distro_running`, `dockerd`, `pipe_proxy`,
`docker_cli`, `compose_plugin`, `docker_context`, `mirrored_networking` and
`disk_space`. They are in the order problems have to be solved, so apply the
first `fix` and run `doctor` again. A `fail` means Docker does not work until
it is fixed, and that includes an engine that was stopped; a sleeping engine
passes, because Docker works with it. A `warn` does not stop Docker from
working. When `doctor` exits 0, `docklet docker` works.

## Idle stop

A running engine holds more than a gigabyte of memory even without
containers. So the engine is stopped when, for the idle time, no Docker client
was connected and no container was running, and it is started again by the
next Docker command, whichever client sends it. `docklet status` then says
`sleeping`.

- `sleeping` is not a problem and needs no `docklet start`. Run the Docker
  command; allow it some seconds more than usual.
- A running container keeps the engine up. So does an open connection, such
  as `docklet docker logs -f` or `docklet docker events`.
- `docklet stop` is different: the engine then stays off, and
  `docklet docker` fails with `not_running`, until `docklet start` or
  `docklet up`.
- `docklet config get idle-stop` shows the idle time in minutes, or `off`.
  `docklet config set idle-stop 30` changes it and
  `docklet config set idle-stop off` keeps the engine running until it is
  stopped. A change applies from the next `docklet start`; when
  `restart_needed` is true the engine that is running keeps the old setting
  until `docklet stop`. Do not change the setting unless the user asks.

## Error codes

| `error.code` | Meaning | What to do |
| --- | --- | --- |
| `usage` | The command line is wrong (exit 2) | Check `docklet help --json` |
| `confirmation_required` | The command deletes data and `--yes` was missing (exit 2) | Ask the user, then repeat with `--yes` |
| `wsl_missing` | WSL is not installed | Tell the user; it needs an administrator and a reboot |
| `wsl_outdated` | WSL is older than Docklet supports | Tell the user to run `wsl --update` |
| `wsl_error` | `wsl.exe` failed or did not answer in time | `docklet doctor --json`; retry later |
| `not_installed` | The `docklet` distro is not installed | `docklet up` (or `docklet install`) |
| `already_installed` | `install` was run on an installed distro | Nothing to install; `docklet start` |
| `unknown_base` | `--base` names no published base | `docklet bases` |
| `not_running` | The engine is stopped (not merely sleeping), or its pipe proxy is not running | `docklet start` |
| `timeout` | The engine did not become usable in time | Run `error.hint`, or `docklet doctor --json` |
| `docker_cli_missing` | No `docker.exe` was found | `docklet cli setup` (or `docklet up`) |
| `checks_failed` | `doctor` found a failing check | Apply the `fix` of the first failing check in `data.checks` |
| `failed` | Anything else | Read `error.message`; `docklet doctor --json` |

## Using Docker

`docklet docker <args...>` runs the `docker.exe` on PATH, or the copy Docklet
installed, against the Docklet engine. Arguments, piped input and the exit code
pass through unchanged:

```
docklet docker run --rm hello-world
docklet docker compose up -d
echo hi | docklet docker run -i --rm alpine cat
```

On a stopped engine it fails with `not_running` instead of starting it; run
`docklet up` or `docklet start` first. A sleeping engine it wakes. Calling
`docker --context docklet ...` directly also works once `docklet up` or
`docklet cli setup` has created the context, and wakes a sleeping engine the
same way.

Published ports are reachable on `http://localhost:<port>` from Windows.
`docklet status`, `docklet start` and `docklet up` print a warning on stderr
when `.wslconfig` sets `networkingMode=mirrored`: published ports may then be
unreachable from Windows. Do not change the file; tell the user.

Limitation: bind mounts of Windows paths (`-v C:\project:/app`) do not work yet.
Use named volumes, or paths inside the distro.
