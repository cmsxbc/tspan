# What You Are Doing (TSPAN)

A C/S architecture activity tracker. Record how much time you spend on various tasks by wrapping commands with a bash client.

## Quick Start

### Build

```bash
cargo build --release
```

### Initialize & Import History

```bash
# Import historical records from reference/records/
./target/release/tspan-server import ./reference/records/

# Generate an API token (for a single agent)
./target/release/tspan-server token-generate "agent-1"

# Generate a token for a specific client/agent
./target/release/tspan-server token-generate --client-id agent-2 "agent-2"
```

### Start Server

```bash
./target/release/tspan-server --bind 0.0.0.0:8080 --web-password yourpassword
```

The server will auto-generate an initial API token if none exists. Copy it from the console output.

### Use `tspanrun` Wrapper

```bash
export TSPANRUN_SERVER="http://localhost:8080"
export TSPANRUN_TOKEN="tspan_xxxxx"

# Track any command
./tspanrun vim file.txt
./tspanrun python train.py
TSPANRUN_ALIAS="模型训练" ./tspanrun python train.py --epochs 100
```

### Web Interface

- **Stats**: http://localhost:8080/ (HTTP Basic Auth)
- **Admin**: http://localhost:8080/admin (orphaned sessions + API token management)

## API Endpoints

All `/api/*` endpoints require `Authorization: Bearer <token>`.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/sessions/start` | POST | Start a session |
| `/api/sessions/:id/end` | POST | End a session |
| `/api/sessions/:id/discard` | POST | Discard an orphaned session |
| `/api/sessions/orphaned` | GET | List orphaned sessions |
| `/api/stats` | GET | Statistics JSON |
| `/api/stats/summary.md` | GET | Markdown report with SVG calendars |
| `/api/admin/import` | POST | Import historical data |
| `/api/admin/backup` | GET | Download a consistent DB snapshot |
| `/api/admin/tokens` | GET/POST | List / generate API tokens |
| `/api/admin/tokens/:token` | DELETE | Revoke a token |

## Kubernetes Deployment

```bash
# Build image
docker build -t tspan-server:latest .

# Build with a local crates.io mirror (recommended for slow network)
docker build \
  --build-arg CARGO_REGISTRY="https://mirrors.tuna.tsinghua.edu.cn/git/crates.io-index.git" \
  -t tspan-server:latest .

# Deploy to k8s
kubectl apply -f k8s/

# Port-forward for local access
kubectl port-forward -n tspan-system svc/tspan-server 8080:80
```

## Desktop Environment Integration (rofi)

If you use **rofi** to launch desktop applications, a custom modi script is provided to automatically wrap all launches with `tspanrun` — no need to modify individual `.desktop` files.

```bash
# In your WM config (i3/sway/dwm/etc.), replace:
#   rofi -show drun
# With:
rofi -modi drun:$HOME/what-you-are-doing/scripts/tspan-rofi-drun -show drun
```

The script scans `.desktop` files, displays them in rofi (with icons), and executes the selected application through `tspanrun` with the app name set as `alias`.

### Prerequisites

```bash
export TSPANRUN_SERVER="http://localhost:8080"
export TSPANRUN_TOKEN="tspan_xxxxx"
chmod +x scripts/tspan-rofi-drun
```

## Architecture

- **Server**: Rust (axum) + SQLite (WAL mode) + native SVG generation
- **Client**: Bash wrapper (`tspanrun`) using `curl`
- **Storage**: Single SQLite file, zero external dependencies for charts
- **Backup**: Online backup via SQLite backup API (`/api/admin/backup`)
