# Multi-User Support Assessment

## 结论

**当前方案不支持同一 host 上多个用户完全隔离的记录。**

虽然数据库表中有 `client_id` 字段，但鉴权层没有将 token 与 client_id 绑定，所有 API 查询也没有按 client_id 过滤。任何持有有效 token 的用户都可以访问和操作所有数据。

---

## 当前方案的问题分析

### 1. Token 与 Client 未绑定

`api_tokens` 表结构：
```sql
CREATE TABLE api_tokens (
    token       TEXT PRIMARY KEY,
    description TEXT,
    created_at  INTEGER NOT NULL
);
```

**缺失**：没有 `client_id` 字段。无法知道一个 token 属于哪个用户。

### 2. 数据查询没有按 client_id 过滤

`stats.rs` 中的统计查询：
```sql
SELECT COALESCE(SUM(duration_seconds), 0), COUNT(*), MIN(start_time)
FROM records WHERE status = 'completed'
```

**缺失**：没有 `AND client_id = ?` 过滤。用户 A 的 stats 会包含用户 B 的数据。

### 3. Session 管理没有权限隔离

`api_end_session`、`api_discard_session`、`api_get_orphaned` 都通过 `id` 直接操作记录，不检查该记录是否属于当前 token 对应的用户。

### 4. Web 界面没有用户概念

Web 界面的 Basic Auth 用户名/密码是全局的。所有用户看到的都是同样的聚合数据。

### 5. tspanrun 的 client_id 默认是 hostname

```bash
CLIENT_ID="${TSPANRUN_CLIENT:-$(hostname)}"
```

同一 host 上的所有用户默认使用相同的 `client_id`，进一步加剧了混淆。

---

## 推荐方案：按 Client 分库隔离

### 设计思路

与其在一个数据库里用 `client_id` 过滤（容易遗漏、查询复杂），不如**每个 client 一个独立的 SQLite 数据库文件**。这样：

- **物理隔离**：用户 A 的数据库文件和用户 B 完全分开
- **代码改动小**：大部分查询逻辑不需要改，只需增加"路由到哪个数据库"的逻辑
- **权限自然隔离**：token A 只能打开 userA.db，天然看不到 userB 的数据
- **备份简单**：每个用户的数据库可以独立备份

### 数据库文件布局

```
data/
├── userA.db
├── userA.db-shm
├── userA.db-wal
├── userB.db
├── userB.db-shm
└── userB.db-wal
```

### 需要修改的模块

| 模块 | 修改内容 |
|------|---------|
| `db.rs` | `ApiToken` 增加 `client_id`；`create_pool` 改为按 `client_id` 创建/打开数据库；管理所有数据库连接缓存 |
| `auth.rs` | `verify_api_token_sync` 返回 `(bool, client_id)`；`check_api_auth` 返回 `client_id` |
| `server.rs` | 所有 handler 根据 `client_id` 获取对应的数据库连接 |
| `main.rs` | `token-generate` 子命令增加 `--client-id` 参数；创建数据库时自动创建目录 |
| `tspanrun` | `client_id` 参数变为可选；如果不传，由 server 根据 token 自动推断 |

### 数据迁移

现有 `data.db` 可以保留为兼容模式，或者通过工具将现有数据按 `client_id` 拆分到多个数据库文件中。

---

## 替代方案对比

| 方案 | 隔离级别 | 代码复杂度 | 数据迁移难度 | 备注 |
|------|---------|-----------|-------------|------|
| **按 client 分库（推荐）** | 物理隔离 | 中 | 低 | 最干净，天然权限隔离 |
| 单库 + row-level filter | 逻辑隔离 | 高 | 低 | 容易遗漏过滤条件 |
| 多实例（每个用户一个 server） | 进程隔离 | 低 | 无 | 端口管理麻烦，资源浪费 |
