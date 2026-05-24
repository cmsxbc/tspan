# Backup and Restore

## Overview

TSPAN uses SQLite in WAL (Write-Ahead Logging) mode. In this mode, writes are first appended to a separate WAL file (`data.db-wal`) before being merged back into the main database file (`data.db`). A shared-memory index (`data.db-shm`) is also maintained for fast concurrent reads.

```
data.db      → main database file
data.db-wal  → pre-write journal (un-checkpointed changes)
data.db-shm  → shared-memory index for WAL
```

**Important:** These three files are a logical unit. Do not delete or copy them individually, or the database may become corrupted.

---

## Backup

### Method 1: Online Backup API (Recommended)

The server provides an online backup endpoint that uses SQLite's native backup API (`sqlite3_backup_init`). This produces a clean, single-file database without requiring a server restart.

```bash
# Download backup via Basic Auth
curl -u admin:PASSWORD \
  http://localhost:8080/api/admin/backup \
  -o tspan-backup-$(date +%Y%m%d).db
```

Characteristics:

| Feature | Description |
|---------|-------------|
| Server restart required | No |
| Output format | Single `.db` file (non-WAL mode) |
| Consistency | Point-in-time snapshot at backup start |
| Impact on runtime | Minimal; brief shared lock only |

### Method 2: Automated CronJob Backup

A Kubernetes CronJob can be configured to download backups periodically:

```yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: tspan-backup
spec:
  schedule: "0 2 * * *"
  jobTemplate:
    spec:
      template:
        spec:
          containers:
            - name: backup
              image: busybox:stable
              command:
                - /bin/sh
                - -c
                - |
                  TOKEN=$(wget -qO- --header="Authorization: Basic $(echo -n \"$WEB_USER:$WEB_PASS\" | base64)" \
                    http://tspan-server.tspan-system.svc.cluster.local/api/admin/tokens | head -1 | cut -d' ' -f1)
                  wget -q --header="Authorization: Bearer $TOKEN" \
                    -O /backup/tspan-backup-$(date +%Y%m%d-%H%M%S).db \
                    http://tspan-server.tspan-system.svc.cluster.local/api/admin/backup
              volumeMounts:
                - name: backup
                  mountPath: /backup
          volumes:
            - name: backup
              persistentVolumeClaim:
                claimName: tspan-backup
          restartPolicy: OnFailure
```

> **Security note:** The example above embeds credentials in the manifest for illustration. In production, use Kubernetes Secrets.

---

## Restore

**Restoring requires stopping the server** (or ensuring no process holds the database connection), because replacing the database file while it is open will cause file descriptor issues.

### Standalone (Binary or Docker)

```bash
# 1. Stop the server
pkill tspan-server          # or docker stop <container>

# 2. Replace the database file
mv tspan-backup-YYYYMMDD.db data.db

# 3. Remove stale WAL/SHM files (the restored file is not in WAL mode;
#    the server will re-enable WAL automatically on next start)
rm -f data.db-wal data.db-shm

# 4. Start the server
./tspan-server              # or docker start <container>
```

On startup, the server automatically runs:

```sql
PRAGMA journal_mode=WAL;
```

This re-enables WAL mode and recreates the `-wal` / `-shm` files as needed.

### Kubernetes

```bash
# 1. Scale down to zero (disconnect all database handles)
kubectl scale deployment tspan-server -n <namespace> --replicas=0

# 2. Replace the database file in the persistent volume.
#    One way is to run a temporary pod with the same PVC mounted:
kubectl run restore --rm -it \
  --image=busybox \
  --overrides='{
    "spec": {
      "volumes": [{
        "name": "data",
        "persistentVolumeClaim": {"claimName": "tspan-data"}
      }],
      "containers": [{
        "name": "restore",
        "image": "busybox",
        "volumeMounts": [{"name": "data", "mountPath": "/data"}],
        "stdin": true,
        "tty": true
      }]
    }
  }' \
  -- /bin/sh

# Inside the temporary pod:
# cp /path/to/backup/tspan-backup-YYYYMMDD.db /data/data.db
# rm -f /data/data.db-wal /data/data.db-shm
# exit

# 3. Scale back up
kubectl scale deployment tspan-server -n <namespace> --replicas=1
```

---

## FAQ

### Why do `-wal` and `-shm` files persist while the server is running?

This is normal WAL behavior. The server holds a long-lived connection; new writes append to the WAL file. SQLite performs automatic checkpoints in the background, but the WAL file is only fully truncated when the last connection closes. When the server stops gracefully, a final checkpoint usually cleans them up.

### Why do `-wal` and `-shm` disappear after running the CLI import command?

CLI commands (e.g., `tspan-server import`) open a short-lived connection, perform work, and exit. On connection close, SQLite performs a full checkpoint that writes all WAL contents back to the main database and removes the auxiliary files.

### Can I just `cp data.db` without the WAL/SHM files?

No. If you copy only `data.db`, you will miss un-checkpointed transactions that still live in `data.db-wal`. Always use the online backup API, or atomically copy all three files while no process is writing.

### Is the backup file itself WAL-enabled?

No. The online backup API produces a plain SQLite database file. WAL mode is re-enabled automatically when the server starts and calls `PRAGMA journal_mode=WAL`.
