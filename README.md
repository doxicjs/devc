# devc - Dev Control

TUI dashboard for managing local dev services.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/doxicjs/devc/main/install.sh | bash
```

## Usage

Place a `devc.toml` in your project root and run:

```bash
devc                  # uses ./devc.toml (attaches if one is already running)
devc path/to/config   # custom config path
devc -h               # full command reference
devc -v               # show version
devc -u               # update to latest
```

devc can also be driven from outside the TUI — `devc status`, `devc start Web`, `devc stop Web` — see [Driving devc from scripts and agents](#driving-devc-from-scripts-and-agents).

### Tabs

- **Services** — long-running processes with start/stop toggle and port monitoring
- **Commands** — one-time commands that run to completion and report exit status
- **Tools** — quick links (open in browser) and copy-to-clipboard items

### Keybindings

| Key              | Action                                  |
| ---------------- | --------------------------------------- |
| `Tab` / `BackTab`| Switch between Services / Commands / Tools |
| `↑↓` / `jk`     | Navigate                                |
| `Enter`          | Activate selected item                  |
| `Space`          | Open service URL in browser             |
| `x`              | Stop all services (Services tab)        |
| `q`              | Quit (detaches, if this is an attached view) |

Services, commands, and tools also have their own shortcut keys defined in `devc.toml`. The keys `q`, `j`, `k`, and `space` are consumed by the UI on every tab, so don't bind them. `x` is only reserved on the Services tab — you can use it as a command or tool binding.

### Default Behaviors

- **Config file** — reads `./devc.toml` from the current directory
- **Project root** — defaults to `./` (the directory containing `devc.toml`)
- **Port monitoring** — when `port` is set, devc checks it every ~2s on IPv4 and IPv6 loopback and shows a status icon; include the port flag in your command if the service needs it
- **Single instance** — one devc supervises a given `devc.toml`; a second `devc` in the same project attaches to it as another view rather than starting a rival supervisor
- **Duplicate protection** — starting a service that's already up is a no-op. If its `port` is held by a process devc didn't start, the service shows as `external` with that pid and devc refuses to spawn a second copy or to kill the foreign one
- **Service URL** — if `url` is not set but `port` is, `Space` opens `http://localhost:<port>/`
- **Dependencies** — services listed in `depends_on` are started automatically before the dependent service
- **Stop signal** — services receive `SIGTERM` first, then `SIGKILL` after 3s if still running
- **Log buffer** — last 500 lines of output are kept per service/command
- **ANSI colors** — log panels render ANSI escape sequences (16 standard colors, 256 indexed, 24-bit RGB, bold, dim, italic, underline, strikethrough, and more)
- **Status messages** — flash for 3 seconds then disappear
- **Startup tab** — opens on the Services tab
- **Sections** — all sections are optional including `services`; unknown fields are rejected with a clear error
- **Local overrides** — if a sibling `devc.local.toml` exists, it's merged on top of `devc.toml` at startup (see below)
- **Live config reload** — `devc.toml` and `devc.local.toml` are polled (~100ms via mtime). Edits reload automatically without restarting devc; running services are never killed. A `[reload]` (yellow) badge appears on a running service or command whose config changed — stop+start to apply. A `[removed]` (red) badge appears on a running entry that was removed from config — once stopped, it auto-disappears. Stopped commands are fully reset (logs cleared, status icon gone) when their config changes. Tools (links, copies) rebuild silently. Parse errors flash an error and keep the previous config active.

### Driving devc from scripts and agents

A running devc listens on a per-project Unix socket. The same binary doubles as the client, so anything that can run a command — a script, a Makefile, a coding agent — can ask devc what's up and turn things on and off.

```bash
devc ls                      # what can I start? (works with no devc running)
devc status                  # what's up, who owns it, on which pid
devc status Web --json       # machine-readable, one service
devc start Web               # start if not already up
devc start Web --wait        # ...and block until the port actually answers
devc stop Web                # stop, if devc started it
devc restart Web --wait
devc run Migrate             # run a [[commands]] entry
devc logs Web -n 50          # tail buffered output
```

All of these target `./devc.toml` unless you pass `--config PATH`.

#### Start is idempotent

`devc start Web` on a service that's already running is a **no-op that exits 0** and tells you the pid. This is the point: a caller that has lost track of what it started can't create a second copy of your dev server by asking again.

```console
$ devc start Web
started (pid 48213)
$ devc start Web
already running (pid 48213)
```

The check is a live TCP probe taken immediately before spawning, not a cached flag, so there's no window where two rapid starts both get through.

#### Servers devc didn't start

If a service's `port` is answering but devc has no process for it — someone ran `pnpm dev` in a shell and forgot — devc reports it as `external` and names the pid:

```console
$ devc status
SERVICE              STATUS     PORT     PID
Web                  external   8899     33211

$ devc start Web
port 8899 held by external pid 33211      # exit 0 — it's up, nothing to do

$ devc stop Web
devc: port 8899 held by external pid 33211 — devc didn't start it, so it won't stop it
                                          # exit 4 — devc won't kill what it didn't spawn
```

This detection needs a `port` on the service. Without one there's nothing to probe, so set `port` on anything you want protected from duplicate starts.

#### Exit codes

| Code | Meaning |
| ---- | ------- |
| `0`  | Success — **including** "already running". The requested state holds. |
| `1`  | Usage error, or the action failed |
| `2`  | No service or command by that name (the error lists the valid ones) |
| `3`  | No devc running for this config |
| `4`  | Refused — e.g. stopping a service devc didn't start |

Because "already running" is a success, the common agent pattern is just:

```bash
devc start Web --wait || exit 1    # ensure it's up; fine to call repeatedly
```

#### Only one devc per project

The socket doubles as a lock. Running `devc` in a project that already has one attaches to it as a second view rather than starting a second supervisor — two supervisors would each spawn their own copy of every service.

An attached view shares the primary's cursor and scroll position, like `tmux attach`. Pressing `q` detaches; it does not quit the primary or stop anything. A socket left behind by a devc that crashed is detected (the recorded pid is gone, or it fails to answer a handshake) and reclaimed automatically.

The socket lives in a `0700` directory under `$XDG_RUNTIME_DIR`, `$TMPDIR`, or `/tmp`, so it's reachable only by you. Note that anyone who can write to it can run any command in your `devc.toml` — the same trust boundary as your shell.

### Upgrading

See [MIGRATION.md](MIGRATION.md) for breaking changes between releases.

### Configuration

```toml
[general]
project_root = "./my-project"

[[services]]
name = "API"
key = "a"
command = "docker compose up"
working_dir = "api"
url = "http://localhost:3000/"
depends_on = []

[[services]]
name = "Web"
key = "w"
port = 5173
command = "pnpm dev"
working_dir = "web"
depends_on = ["API"]

[[commands]]
name = "Migrate"
key = "m"
command = "pnpm db:migrate"
working_dir = "api"

[[links]]
name = "Dashboard"
key = "d"
url = "http://localhost:3000/admin"

[[copies]]
name = "API Key"
key = "c"
text = "your-api-key"
```

#### Services

| Field          | Required | Description                                          |
| -------------- | -------- | ---------------------------------------------------- |
| `name`         | yes      | Display name                                         |
| `key`          | yes      | Single-character shortcut to toggle the service       |
| `command`      | yes      | Shell command to start the service                   |
| `working_dir`  | yes      | Working directory (relative to `project_root`)       |
| `port`         | no       | Port to monitor (1–65535); shown in service list      |
| `url`          | no       | URL to open with `Space` (defaults to `localhost:port`) |
| `depends_on`   | no       | Array of service names to start first                |

#### Commands

| Field         | Required | Description                                    |
| ------------- | -------- | ---------------------------------------------- |
| `name`        | yes      | Display name                                   |
| `key`         | yes      | Single-character shortcut to run the command    |
| `command`     | yes      | Shell command to execute                       |
| `working_dir` | yes      | Working directory (relative to `project_root`) |

#### Links

| Field  | Required | Description                             |
| ------ | -------- | --------------------------------------- |
| `name` | yes      | Display name                            |
| `key`  | yes      | Single-character shortcut to open       |
| `url`  | yes      | URL to open in browser                  |

#### Copies

| Field  | Required | Description                             |
| ------ | -------- | --------------------------------------- |
| `name` | yes      | Display name                            |
| `key`  | yes      | Single-character shortcut to copy       |
| `text` | yes      | Text to copy to clipboard               |

### Local Overrides

Drop a `devc.local.toml` next to your `devc.toml` to add personal services, commands, or tools without touching the shared config. At startup devc merges it on top of the main config — new entries are appended, and entries whose `name` matches a shared entry replace it in place.

```toml
# devc.local.toml
[[services]]
name = "Scratch"
key = "s"
command = "pnpm dev:scratch"
working_dir = "scratch"

[[services]]
name = "Web"                      # same name as in devc.toml — overrides
key = "w"
command = "pnpm dev --inspect"
working_dir = "web"
port = 5173

[[links]]
name = "Local Admin"
key = "l"
url = "http://localhost:9000/admin"
```

Add it to your project's `.gitignore`:

```
devc.local.toml
```

**Rules:**

- Filename is derived from the main config: `devc.toml` → `devc.local.toml`, `foo.config.toml` → `foo.config.local.toml`
- Every section in `devc.local.toml` is optional, including `[[services]]`
- `services`, `commands`, `links`, `copies` merge **by `name`** — same name replaces in place; new name appends
- `[general]` merges field-by-field (only fields set in local override main)
- Missing local file is silent; malformed local TOML fails loud at startup
- Gotcha: if you rename an entry in the shared `devc.toml`, any local override keyed on the old `name` will silently become an additive orphan entry — rename it in your local file too

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
