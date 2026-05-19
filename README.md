# What You Are Doing (WYD)

A C/S architecture activity tracker. Record how much time you spend on various tasks by wrapping commands with a bash client.

## Quick Start

### Build

```bash
cargo build --release
```

### Initialize & Import History

```bash
# Import historical records from reference/records/
./target/release/wyd-server import ./reference/records/

# Generate an API token
./target/release/wyd-server token-generate "my-laptop"
```

### Start Server

```bash
./target/release/wyd-server --bind 0.0.0.0:8080 --web-password yourpassword
```

The server will auto-generate an initial API token if none exists. Copy it from the console output.

### Use `wydrun` Wrapper

```bash
export WYDRUN_SERVER="http://localhost:8080"
export WYDRUN_TOKEN="wyd_xxxxx"

# Track any command
./wydrun vim file.txt
./wydrun python train.py
WYDRUN_ALIAS="模型训练" ./wydrun python train.py --epochs 100
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
docker build -t wyd-server:latest .

# Build with a local crates.io mirror (recommended for slow network)
docker build \
  --build-arg CARGO_REGISTRY="https://mirrors.tuna.tsinghua.edu.cn/git/crates.io-index.git" \
  -t wyd-server:latest .

# Deploy to k8s
kubectl apply -f k8s/

# Port-forward for local access
kubectl port-forward -n wyd-system svc/wyd-server 8080:80
```

## Architecture

- **Server**: Rust (axum) + SQLite (WAL mode) + native SVG generation
- **Client**: Bash wrapper (`wydrun`) using `curl`
- **Storage**: Single SQLite file, zero external dependencies for charts
- **Backup**: Online backup via SQLite backup API (`/api/admin/backup`)
