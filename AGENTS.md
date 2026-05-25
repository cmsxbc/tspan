# AGENTS.md — What You Are Doing (TSPAN)

> This file is intended for AI coding agents. It describes the project structure, build process, conventions, and architectural decisions based on the actual source code.

## Project Overview

**What You Are Doing (TSPAN)** is a client/server activity tracker. Users wrap shell commands with a bash client (`tspanrun`), which notifies a Rust server of session start/end events. The server stores data in SQLite and serves a web dashboard with statistics, SVG activity calendars, and admin tools.

- **Server**: Rust (axum) + SQLite (WAL mode) + native SVG generation
- **Client**: Bash wrapper (`tspanrun`) using `curl`
- **eBPF Agent**: `tspan-ebpf` — per-host daemon capturing `execve` via eBPF and exporting to server
- **Web UI**: Single-page dashboard with embedded vanilla JavaScript (no frontend framework)
- **Charts**: Pure SVG generated server-side (no external charting library)

## Technology Stack

| Layer | Technology |
|-------|-----------|
| Language | Rust (Edition 2021) |
| Web Framework | axum 0.7 |
| Async Runtime | tokio (full features) |
| eBPF | aya 0.13 + clang BPF target |
| Database | SQLite via `rusqlite` (bundled, WAL mode, backup API) |
| Auth | Bearer tokens for API; HTTP Basic Auth for Web UI |
| Password Hashing | bcrypt (optional; plaintext fallback supported) |
| Serialization | serde + serde_json |
| CLI Parsing | clap v4 |
| Configuration | Command-line args + env vars (`WEB_PASSWORD`, `TSPANRUN_*`) |
| Logging | tracing + tracing-subscriber with env-filter |

## Project Structure

```
.
├── Cargo.toml              # Rust manifest
├── Cargo.lock              # Pinned dependencies
├── src/
│   ├── main.rs             # CLI entry point, subcommands, server bootstrap
│   ├── server.rs           # axum router, request handlers, embedded HTML/JS
│   ├── db.rs               # SQLite schema, connection pool, queries
│   ├── auth.rs             # API token verification, Basic Auth
│   ├── stats.rs            # Statistics computation (totals, distributions, streaks, heatmaps, trends)
│   ├── svg_calendar.rs     # Native SVG calendar generation (GitHub-style contribution graph)
│   ├── markdown.rs         # Markdown report with base64-encoded SVGs
│   └── importer.rs         # Historical record importer from text files
├── tspanrun                  # Bash client wrapper script
├── tspan-ebpf/             # eBPF agent (independent crate)
│   ├── ebpf/
│   │   ├── main.bpf.c      # eBPF C program (3 tracepoints + ring buffer)
│   │   └── vmlinux.h       # BTF-generated kernel headers
│   └── src/
│       ├── main.rs         # Daemon entry
│       ├── ebpf.rs         # eBPF load/attach/poll
│       ├── exporter.rs     # HTTP client → tspan-server
│       ├── tracker.rs      # PID → session_id tracking
│       ├── filter.rs       # UID/command filtering
│       └── buffer.rs       # Offline retry queue
├── scripts/
│   └── tspan-rofi-drun       # rofi modi script for desktop app tracking
├── k8s/                    # Kubernetes manifests
├── docs/
│   └── multi-user-assessment.md  # Analysis of multi-user isolation (Chinese)
├── reference/              # Legacy data format reference and historical records
├── dev.sh                  # Development build & run helper
├── watch.sh                # Auto-rebuild with cargo-watch
├── Dockerfile              # Multi-stage build (rust builder → distroless runtime)
└── data.db*                # Local SQLite database (WAL mode)
```

## Build and Run Commands

### Development

```bash
# Debug build and run with defaults (password: changeme)
./dev.sh

# With custom password or bind address
WEB_PASSWORD=secret ./dev.sh --bind 127.0.0.1:3000

# Auto-rebuild on source changes (requires cargo-watch)
./watch.sh
```

### Production Build

```bash
cargo build --release
# Binary: ./target/release/tspan-server
```

### Database Initialization

The database schema is auto-initialized on first run (`db::init_db`). It creates:
- `clients` table
- `api_tokens` table
- `records` table (with indexes on client_id, start_time, end_time)

SQLite runs in WAL mode (`PRAGMA journal_mode=WAL`).

### Token Management

```bash
# Generate an API token
./target/release/tspan-server token-generate "agent-1"
./target/release/tspan-server token-generate --client-id agent-2 "agent-2"

# List / revoke tokens
./target/release/tspan-server token-list
./target/release/tspan-server token-revoke <token>
```

If no tokens exist when the server starts, it auto-generates one and prints it to stdout.

### Import Historical Data

```bash
./target/release/tspan-server import ./reference/records/ --client-id imported
```

The importer expects `.txt` files where:
- Filename = start datetime in `%Y%m%d%H%M%S` format
- Line 1 = command (ignored by importer)
- Line 2 = duration in seconds

### Using the Client Wrapper

```bash
export TSPANRUN_SERVER="http://localhost:8080"
export TSPANRUN_TOKEN="tspan_xxxxx"

./tspanrun vim file.txt
TSPANRUN_ALIAS="模型训练" ./tspanrun python train.py
```

## Runtime Architecture

### Server Modes

The binary operates in two modes based on CLI arguments:

1. **Server mode** (default, no subcommand): Starts the HTTP server.
2. **Admin mode** (subcommands: `import`, `token-generate`, `token-list`, `token-revoke`): Direct DB operations.

### HTTP Routes

| Route | Auth | Description |
|-------|------|-------------|
| `/` | Basic Auth | Web dashboard (embedded HTML+JS) |
| `/admin` | Basic Auth | Orphaned sessions + token management |
| `/api/sessions/start` | Bearer | Start a tracked session |
| `/api/sessions/:id/end` | Bearer or Basic | End a session |
| `/api/sessions/:id/discard` | Basic | Discard an orphaned session |
| `/api/sessions/orphaned` | Basic | List active (orphaned) sessions |
| `/api/stats` | Basic | Core statistics JSON |
| `/api/stats/*` | Basic | Various analytics endpoints (by-client, by-alias, by-command, distribution, weekday/weekend, streaks, monthly-trend, hourly-heatmap) |
| `/api/stats/summary.md` | Basic | Markdown report with SVG calendars |
| `/api/svg` | Basic | SVG calendar data JSON |
| `/api/records` | Basic | Paginated record list |
| `/api/exec-events` | Bearer | Log a failed exec attempt to `exec_events` table |
| `/api/clients`, `/api/aliases` | Basic | Filter dropdown data |
| `/api/admin/import` | Basic | Trigger import via API |
| `/api/admin/backup` | Basic | Download consistent DB snapshot |
| `/api/admin/tokens` | Basic | List/generate API tokens |
| `/api/admin/tokens/:token` | Basic | Revoke a token |

### Authentication

- **API endpoints** (`/api/*`): Require `Authorization: Bearer <token>` header. Tokens are stored in the `api_tokens` table and bound to a `client_id`.
- **Web endpoints** (`/`, `/admin`): Require HTTP Basic Auth. Password can be plaintext or bcrypt-hashed (detected by `$2` prefix).
- The `resolve_auth` helper in `server.rs` tries Bearer first, then falls back to Basic Auth, treating Basic Auth users as admins (`is_admin = true`).

### Database Access Pattern

SQLite is accessed through a single shared connection wrapped in `Arc<Mutex<Connection>>` (`DbPool`). Handlers lock the mutex for each DB operation. This is simple but means the server is effectively single-writer for DB operations.

## Module Responsibilities

### `main.rs`
- CLI definition with `clap`
- Subcommand dispatch
- Server bootstrap: token auto-generation, TCP listener, axum serve

### `server.rs`
- `AppState` struct (`DbPool` + `AuthConfig`)
- axum router definition (`create_router`)
- All HTTP handler implementations
- **Embedded frontend**: The entire web dashboard (`INDEX_HTML`) is a single `const &str` containing HTML, CSS, and vanilla JS that fetches JSON from `/api/*` endpoints.

### `db.rs`
- `DbPool` type alias and `create_pool`
- Schema initialization (`init_db`): creates `clients`, `api_tokens`, `records`, `exec_events`
- Session lifecycle: `start_session`, `end_session`, `discard_session`
- Exec event logging: `log_exec_event` (for eBPF-captured failed execs)
- Admin variants: `end_session_admin`, `discard_session_admin`, `get_orphaned_sessions_admin`
- Token management: `verify_api_token`, `list_api_tokens`, `add_api_token`, `delete_api_token`
- Record queries: `list_records_page`, `distinct_client_ids`
- Import: `import_record`

### `auth.rs`
- `AuthConfig` struct
- `extract_bearer_token` from headers
- `check_api_auth` (async) and `check_web_auth` (async)
- `verify_basic_auth` with bcrypt or plaintext comparison

### `stats.rs`
- Core statistics: `compute_stats` (totals, past N periods, interval stats)
- Grouped stats: by client, alias, command
- Analytics: session distribution, weekday vs weekend, streaks, monthly trend, hourly heatmap
- Helper: `human_readable_time` (formats seconds to "X d HH h MM m SS s")
- Daily data: `get_daily_data` for SVG generation

### `svg_calendar.rs`
- `generate_svg_calendar`: GitHub-contribution-graph-style SVG calendar
- `generate_all_years_svgs`: Per-year SVG breakdown
- Uses dot-matrix digits for year-view counts
- Color levels: `#f6f8fa` (0s), `#9be9a8` (<30m), `#f9d71c` (30-60m), `#e5534b` (>60m)

### `markdown.rs`
- `generate_markdown_report`: Produces a Markdown document with base64-encoded SVG images suitable for rendering in Markdown viewers.

### `importer.rs`
- `import_from_directory`: Scans a directory for `.txt` files, parses filename as datetime and line 2 as duration, inserts completed records.

## Code Style Guidelines

- **Formatting**: Standard Rust `rustfmt`. No custom rustfmt config detected.
- **Error Handling**: Mix of `anyhow::Result` for high-level flow and `rusqlite::Result` for DB operations. HTTP handlers map DB errors to `StatusCode::INTERNAL_SERVER_ERROR` and log them with `tracing::error!`.
- **Naming**: `snake_case` throughout. Async handlers prefixed with `api_` or `web_`.
- **SQL**: Inline SQL strings using `rusqlite::params!` or `params_from_iter`. `COALESCE` used aggressively for null handling.
- **Comments**: Sparse; rely on descriptive function names. Major logic blocks in `svg_calendar.rs` have phase comments.
- **Unsafe**: None detected.

## Testing

**There are currently no automated tests in this project** (no `tests/` directory, no `#[cfg(test)]` modules). All validation is manual via:

1. Building and running the server
2. Using `tspanrun` to track commands
3. Checking the web dashboard
4. Importing historical data and verifying stats

When making changes, verify by:
- `cargo build` (or `cargo build --release`)
- Running `./dev.sh` and testing the affected endpoint via browser or curl
- Checking `tracing` logs for errors

## Deployment

### Docker

```bash
docker build -t tspan-server:latest .
```

Supports a `CARGO_REGISTRY` build arg for mirrors (e.g., Tsinghua).

### Kubernetes

```bash
kubectl apply -f k8s/
```

Manifests:
- `namespace.yaml`: `tspan-system`
- `pvc.yaml`: 1Gi persistent volume for SQLite
- `deployment.yaml`: Single replica, distroless image, probes on `/`
- `service.yaml`: ClusterIP on port 80 → 8080
- `ingress.yaml`: nginx ingress for `tspan.local`
- `cronjob-backup.yaml`: Daily at 02:00, downloads DB backup via `/api/admin/backup`

**Security note**: The K8s manifests currently embed `WEB_PASSWORD` as plaintext. A production deployment should use Kubernetes Secrets.

## Security Considerations

- **Single shared DB connection with Mutex**: Not horizontally scalable. The K8s deployment uses `replicas: 1` for this reason.
- **API tokens are stored in plaintext** in the `api_tokens` table (the `token` column itself is the secret).
- **Web password** can be plaintext or bcrypt-hashed. The `WEB_PASSWORD` env var is read at startup.
- **No HTTPS enforcement**: The server does not redirect HTTP to HTTPS. Use a reverse proxy (nginx, Traefik) for TLS termination.
- **Multi-user isolation**: As documented in `docs/multi-user-assessment.md`, the current architecture does **not** fully isolate data by client. Any valid token or Basic Auth user can view all aggregated stats. The `client_id` field exists for grouping but not for access control.

## Development Conventions

- Run `./dev.sh` for quick iteration; it kills existing processes, builds debug, and starts the server.
- Use `WEB_PASSWORD` env var to avoid typing the password flag.
- The server auto-creates `data.db` in the working directory. It is safe to delete for a fresh start (but you will lose data).
- When adding new stats endpoints, follow the pattern in `server.rs`: add route → handler → delegate to `stats::compute_*` → return `Json<T>`.
- When modifying the web UI, edit the `INDEX_HTML` string literal in `server.rs`. The frontend is vanilla JS with no build step.
- SVG calendar parameters (`INNER`, `BORDER`, `PADDING`, `STRIDE`) are kept in sync with the legacy `reference/calendarimg.sh` implementation.
- **Commit early and often**: After each coherent change (bug fix, feature chunk, or refactor), commit immediately with a descriptive message. Do not batch multiple unrelated changes into a single commit. This keeps the history clean and makes rollbacks easier.

## Useful Reference

- `README.md`: Quick start guide, API endpoint table, rofi integration
- `docs/multi-user-assessment.md`: Detailed analysis of multi-user isolation limitations and a recommended per-client-database architecture
- `reference/records/`: Example historical data format for import testing
- `reference/summary.sh` / `reference/do.sh`: Legacy shell-based tracker (predecessor to this Rust implementation)
