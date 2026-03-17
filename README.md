# devc

TUI dashboard for managing local dev services.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/doxicjs/devc/main/install.sh | bash
```

## Usage

Place a `devc.toml` in your project root and run:

```bash
devc
```

### Keybindings

| Key         | Action                          |
| ----------- | ------------------------------- |
| `Enter`     | Toggle selected service         |
| `Space`     | Open service in browser         |
| `Tab`       | Switch between Services / Tools |
| `↑↓` / `jk` | Navigate                        |
| `a`         | Start all services              |
| `x`         | Stop all services               |
| `q`         | Quit                            |

Services and tools also have their own shortcut keys defined in `devc.toml`.

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

[[services]]
name = "Web"
key = "w"
port = 5173
command = "pnpm dev"
working_dir = "web"
service_type = "frontend"

[[links]]
name = "Dashboard"
key = "d"
url = "http://localhost:3000/admin"

[[copies]]
name = "API Key"
key = "c"
text = "your-api-key"
```
