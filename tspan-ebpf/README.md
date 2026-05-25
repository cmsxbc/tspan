# tspan-ebpf

eBPF-based process execution tracker for TSPAN.

## Overview

`tspan-ebpf` is a per-host daemon that uses eBPF to capture all `execve` attempts (success or failure) and exports them to a remote `tspan-server` backend.

## Architecture

```
Kernel eBPF        Ring Buffer        Userspace Daemon        HTTP
  trace_enter_execve ──►              load & attach
  trace_exit_execve  ──►  rb  ─────►  poll loop  ─────────►  /api/sessions/start
  sched_process_exit ──►              filter & track                /end
                                                         ──►  /api/exec-events
```

## Event Flow

1. **sys_enter_execve / sys_enter_execveat**: Stores temporary exec context (filename, args, PID) in a BPF hash map.
2. **sys_exit_execve / sys_exit_execveat**: 
   - On success (`ret == 0`): emits `ExecSuccess` event, inserts PID into `active_pids` map.
   - On failure (`ret != 0`): emits `ExecFailed` event.
3. **sched_process_exit**: If PID is in `active_pids`, emits `ProcessExit` event.

## Userspace Handling

- **ExecSuccess** → `POST /api/sessions/start` → track PID locally → on **ProcessExit** → `POST /api/sessions/:id/end`
- **ExecFailed** → `POST /api/exec-events` (stored in `exec_events` table)
- Failed exports are buffered to a local JSONL file and replayed on startup.

## Build

```bash
cd tspan-ebpf
cargo build --release
```

Requires:
- Rust stable toolchain
- `clang` with BPF target support
- Linux kernel with BTF (5.8+ recommended)

> **Kernel upgrade note**: `ebpf/vmlinux.h` is auto-generated from the running kernel's BTF. After a kernel upgrade, remove `ebpf/vmlinux.h` and rebuild so the eBPF program uses the new kernel type definitions:
> ```bash
> rm ebpf/vmlinux.h && cargo build --release
> ```

## Run

Requires root or `CAP_BPF` + `CAP_PERFMON` + `CAP_SYS_ADMIN`.

```bash
sudo ./target/release/tspan-ebpf \
  --server http://tspan-server:8080 \
  --token tspan_xxxxx \
  --client-id $(hostname)
```

### Environment Variables

| Variable | Description |
|----------|-------------|
| `TSPAN_EBPF_SERVER` | tspan-server URL |
| `TSPAN_EBPF_TOKEN` | API Bearer token |
| `TSPAN_EBPF_CLIENT` | client_id for records |
| `TSPAN_EBPF_ALLOW_UIDS` | Comma-separated UID allowlist |
| `TSPAN_EBPF_DENY_COMM` | Regex pattern to deny commands |

## Filtering

- `--allow-uids`: Only track processes launched by these UIDs.
- `--deny-comm`: Skip commands matching this regex (e.g. `kworker.*`).

## Retry Buffer

If the tspan-server is unreachable, failed exports are appended to a local JSONL file (default `/var/lib/tspan-ebpf/retry.jsonl`). On daemon startup, buffered events are replayed before accepting new eBPF events.
