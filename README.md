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
