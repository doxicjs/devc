# devc - Dev Control

TUI dashboard for managing local dev services.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/doxicjs/devc/main/install.sh | bash
```

## Usage

Place a `devc.toml` in your project root and run:

```bash
devc                  # uses ./devc.toml
devc path/to/config   # custom config path
devc -v               # show version
devc -u               # update to latest
```

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
| `a`              | Start all services                      |
| `x`              | Stop all services                       |
| `q`              | Quit                                    |

Services, commands, and tools also have their own shortcut keys defined in `devc.toml`.

### Default Behaviors

- **Config file** — reads `./devc.toml` from the current directory
- **Project root** — defaults to `./` (the directory containing `devc.toml`)
- **Port monitoring** — when `port` is set, devc checks it every ~2s on IPv4 and IPv6 loopback and shows a status icon; include the port flag in your command if the service needs it
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

### Configuration

```toml
[general]
project_root = "./my-project"

[[services]]
name = "API"
key = "a"
command = "docker compose up"
working_dir = "api"
service_type = "backend"
url = "http://localhost:3000/"
depends_on = []

[[services]]
name = "Web"
key = "w"
port = 5173
command = "pnpm dev"
working_dir = "web"
service_type = "frontend"
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
| `service_type` | yes      | Type label (e.g. `backend`, `frontend`)              |
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
service_type = "backend"

[[services]]
name = "Web"                      # same name as in devc.toml — overrides
key = "w"
command = "pnpm dev --inspect"
working_dir = "web"
service_type = "frontend"
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
