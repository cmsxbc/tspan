# What You Are Doing (TSPAN)

A C/S architecture activity tracker. Record how much time you spend on various tasks by wrapping commands with a bash client.

## Quick Start

### Build

```bash
# Server and backup client
cargo build --release

# Standalone terminal admin client
cargo build --release -p tspan-tui
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

### Terminal Admin Interface

Run the interactive TUI against a tspan server. Supplying the password through the environment keeps it out of shell history and process arguments:

```bash
export TSPAN_TUI_PASSWORD='your-admin-password'
./target/release/tspan-tui \
  --server https://tspan.example.com \
  --username admin

# Start on one client, use local time, and show 50 records per page
./target/release/tspan-tui \
  --server https://tspan.example.com \
  --client-id workstation \
  --timezone America/New_York \
  --page-size 50

# Debug API traffic and save raw server responses
./target/release/tspan-tui --verbose \
  --server https://tspan.example.com \
  --verbose-log tspan-tui-api.log
```

`tspan-tui` is a separate executable and can be copied to an administrator's machine without `tspan-server`. It has no runtime or build dependency on the server crate, SQLite, or the database file. `TSPAN_TUI_SERVER` and `TSPAN_TUI_USERNAME` can also provide the server URL and username. The TUI authenticates with HTTP Basic Auth and uses only `/api/*` endpoints. Use HTTPS when connecting across a network so the credentials are encrypted in transit.

`-v` / `--verbose` writes every API method and URL, HTTP status, and unmodified response body to `tspan-tui-api.log`. Use `--verbose-log PATH` (or `TSPAN_TUI_VERBOSE_LOG`) to choose another file; specifying a path also enables verbose logging. Logging never writes API traffic to the active terminal, so it cannot interfere with the full-screen interface. Authorization headers are never recorded. On Unix, the log is created with mode `0600`. Raw responses can contain API tokens, tracked commands, and other sensitive data, so protect and delete debug logs appropriately.

The TUI accepts older `/api/records` responses that do not include `status`. It infers completed or active status from the record timestamps and displays a yellow `LEGACY API` compatibility warning so administrators know the server should be upgraded.

The TUI provides summary and grouped statistics, paginated record browsing, active-session management, and API token generation/revocation. Press `?` for complete keyboard help. Common keys are:

| Key | Action |
|-----|--------|
| `1`–`5` / `Tab` | Switch views |
| `↑` / `↓` or `j` / `k` | Move or scroll |
| `[` / `]` | Change the client filter |
| `r` | Refresh (data also refreshes every 10 seconds) |
| `d` | Delete, discard, or revoke the selected item after confirmation |
| `q` / `Ctrl-C` | Quit |

## API Endpoints

Client ingestion endpoints use `Authorization: Bearer <token>`. Statistics and admin endpoints use HTTP Basic Auth; ending a session accepts either authentication method.

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
- **Admin TUI**: Standalone Rust API client (`tspan-tui`)
- **Storage**: Single SQLite file, zero external dependencies for charts
- **Backup**: Online backup via SQLite backup API (`/api/admin/backup`)
