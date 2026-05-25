#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

/*
 * Linux MAX_ARG_STRLEN is 131072 (128KB). We target 65536 (64KB) as a
 * practical upper bound that fits most kernels' eBPF map value limits.
 * Individual args are capped at 4096 bytes.
 */
#define ARG_BUF_SIZE 65536
#define MAX_ARGC 256
#define MAX_ARG_LEN 4096
#define FILENAME_SIZE 128
#define COMM_SIZE 16

#define EVENT_EXEC_SUCCESS 1
#define EVENT_EXEC_FAILED  2
#define EVENT_PROCESS_EXIT 3

/* Ring buffer event: only metadata, args live in arg_buf_map */
struct exec_event_ref {
    u32 type;
    u32 pid;
    u32 tgid;
    u32 uid;
    u64 start_ns;
    char filename[FILENAME_SIZE];
    char comm[COMM_SIZE];
    u32 argc;
    u32 args_len;
    s64 errno;
};

/* Process exit event */
struct exit_event {
    u32 type;
    u32 pid;
    u32 tgid;
    u64 exit_ns;
    u32 exit_code;
};

/* Context saved between enter and exit */
struct exec_ctx {
    u32 tgid;
    u32 uid;
    u64 start_ns;
    char comm[COMM_SIZE];
    char filename[FILENAME_SIZE];
    u32 argc;
    u32 args_len;
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 10240);
    __type(key, u32);
    __type(value, struct exec_ctx);
} exec_ctx_map SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 10240);
    __type(key, u32);
    __type(value, u64);
} active_pids SEC(".maps");

/* Array map (not percpu) to avoid percpu value size limits.
 * Key = CPU id, value = scratch buffer for building argv strings.
 */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1024);
    __type(key, u32);
    __type(value, char[ARG_BUF_SIZE]);
} scratch_buf SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, u32);
    __type(value, char[ARG_BUF_SIZE]);
} arg_buf_map SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024);
} rb SEC(".maps");

static __always_inline int handle_enter_exec(const char *filename,
                                              const char *const *argv)
{
    u32 pid = bpf_get_current_pid_tgid() & 0xFFFFFFFF;
    u32 tgid = bpf_get_current_pid_tgid() >> 32;
    u32 uid = bpf_get_current_uid_gid() & 0xFFFFFFFF;

    struct exec_ctx ec = {};
    ec.tgid = tgid;
    ec.uid = uid;
    ec.start_ns = bpf_ktime_get_ns();
    bpf_get_current_comm(&ec.comm, sizeof(ec.comm));
    bpf_probe_read_user_str(&ec.filename, sizeof(ec.filename), (void *)filename);

    u32 cpu_id = bpf_get_smp_processor_id();
    char *scratch = bpf_map_lookup_elem(&scratch_buf, &cpu_id);
    if (!scratch)
        return 0;

    u32 offset = 0;
    for (int i = 0; i < MAX_ARGC; i++) {
        if (offset + MAX_ARG_LEN > ARG_BUF_SIZE)
            break;
        const char *arg = NULL;
        bpf_probe_read_user(&arg, sizeof(arg), (void *)&argv[i]);
        if (!arg)
            break;
        long n = bpf_probe_read_user_str(&scratch[offset], MAX_ARG_LEN, (void *)arg);
        if (n <= 0)
            break;
        offset += n;
        ec.argc++;
    }
    ec.args_len = offset;

    long ret1 = bpf_map_update_elem(&arg_buf_map, &pid, scratch, BPF_ANY);
    if (ret1 != 0) {
        bpf_printk("tspan-ebpf: arg_buf_map update failed pid=%d ret=%ld\n", pid, ret1);
        return 0;
    }
    bpf_map_update_elem(&exec_ctx_map, &pid, &ec, BPF_ANY);
    bpf_printk("tspan-ebpf: enter pid=%d argc=%d args_len=%d\n", pid, ec.argc, ec.args_len);
    return 0;
}

SEC("tp/syscalls/sys_enter_execve")
int trace_enter_execve(struct trace_event_raw_sys_enter *ctx)
{
    const char *filename = (const char *)ctx->args[0];
    const char *const *argv = (const char *const *)ctx->args[1];
    return handle_enter_exec(filename, argv);
}

SEC("tp/syscalls/sys_enter_execveat")
int trace_enter_execveat(struct trace_event_raw_sys_enter *ctx)
{
    const char *filename = (const char *)ctx->args[1];
    const char *const *argv = (const char *const *)ctx->args[2];
    return handle_enter_exec(filename, argv);
}

static __always_inline int handle_exit_exec(long ret)
{
    u32 pid = bpf_get_current_pid_tgid() & 0xFFFFFFFF;

    struct exec_ctx *ec = bpf_map_lookup_elem(&exec_ctx_map, &pid);
    if (!ec)
        return 0;

    bpf_printk("tspan-ebpf: exit pid=%d args_len=%d\n", pid, ec->args_len);
    struct exec_event_ref *e = bpf_ringbuf_reserve(&rb, sizeof(*e), 0);
    if (!e) {
        bpf_printk("tspan-ebpf: ringbuf reserve failed pid=%d\n", pid);
        goto cleanup;
    }

    e->type = (ret == 0) ? EVENT_EXEC_SUCCESS : EVENT_EXEC_FAILED;
    e->pid = pid;
    e->tgid = ec->tgid;
    e->uid = ec->uid;
    e->start_ns = ec->start_ns;
    __builtin_memcpy(e->filename, ec->filename, sizeof(e->filename));
    __builtin_memcpy(e->comm, ec->comm, sizeof(e->comm));
    e->argc = ec->argc;

    u32 args_len = ec->args_len;
    if (args_len > ARG_BUF_SIZE)
        args_len = ARG_BUF_SIZE;
    e->args_len = args_len;
    e->errno = ret;

    bpf_ringbuf_submit(e, 0);

    if (ret == 0) {
        bpf_map_update_elem(&active_pids, &pid, &ec->start_ns, BPF_ANY);
    }

cleanup:
    bpf_map_delete_elem(&exec_ctx_map, &pid);
    /* arg_buf_map[pid] is deleted by userspace after reading args */
    return 0;
}

SEC("tp/syscalls/sys_exit_execve")
int trace_exit_execve(struct trace_event_raw_sys_exit *ctx)
{
    return handle_exit_exec(ctx->ret);
}

SEC("tp/syscalls/sys_exit_execveat")
int trace_exit_execveat(struct trace_event_raw_sys_exit *ctx)
{
    return handle_exit_exec(ctx->ret);
}

SEC("tp/sched/sched_process_exit")
int trace_process_exit(struct trace_event_raw_sched_process_exit *ctx)
{
    u32 pid = ctx->pid;

    u64 *start_ns = bpf_map_lookup_elem(&active_pids, &pid);
    if (!start_ns)
        return 0;

    struct exit_event *e = bpf_ringbuf_reserve(&rb, sizeof(*e), 0);
    if (e) {
        e->type = EVENT_PROCESS_EXIT;
        e->pid = pid;
        e->tgid = bpf_get_current_pid_tgid() >> 32;
        e->exit_ns = bpf_ktime_get_ns();
        e->exit_code = 0;

        bpf_ringbuf_submit(e, 0);
    }

    bpf_map_delete_elem(&active_pids, &pid);
    return 0;
}

char _license[] SEC("license") = "GPL";
