#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define MAX_ARGS 2
#define ARG_SIZE 32
#define FILENAME_SIZE 128
#define EVENT_DATA_SIZE 256

/* Event type constants */
#define EVENT_EXEC_SUCCESS 1
#define EVENT_EXEC_FAILED  2
#define EVENT_PROCESS_EXIT 3

/* Unified event sent over ring buffer */
struct event {
    u32 type;
    char data[EVENT_DATA_SIZE];
};

/* Data payloads */
struct exec_success_data {
    u32 pid;
    u32 tgid;
    u32 uid;
    u64 start_ns;
    char comm[16];
    char filename[FILENAME_SIZE];
    u8 argc;
    char args[MAX_ARGS][ARG_SIZE];
};

struct exec_failed_data {
    u32 pid;
    u32 tgid;
    u32 uid;
    u64 start_ns;
    char comm[16];
    char filename[FILENAME_SIZE];
    u8 argc;
    char args[MAX_ARGS][ARG_SIZE];
    s64 errno;
};

struct process_exit_data {
    u32 pid;
    u32 tgid;
    u64 exit_ns;
    u32 exit_code;
};

/* Temporary context between enter/exit */
struct exec_ctx {
    u32 tgid;
    u32 uid;
    u64 start_ns;
    char comm[16];
    char filename[FILENAME_SIZE];
    u8 argc;
    char args[MAX_ARGS][ARG_SIZE];
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

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024);
} rb SEC(".maps");

/* Shared enter handler: receives filename and argv pointers */
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

    #pragma unroll
    for (int i = 0; i < MAX_ARGS; i++) {
        const char *arg = NULL;
        bpf_probe_read_user(&arg, sizeof(arg), (void *)&argv[i]);
        if (!arg)
            break;
        bpf_probe_read_user_str(&ec.args[i], ARG_SIZE, (void *)arg);
        ec.argc++;
    }

    bpf_map_update_elem(&exec_ctx_map, &pid, &ec, BPF_ANY);
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

/* Shared exit handler for both execve and execveat */
static __always_inline int handle_exit_exec(long ret)
{
    u32 pid = bpf_get_current_pid_tgid() & 0xFFFFFFFF;

    struct exec_ctx *ec = bpf_map_lookup_elem(&exec_ctx_map, &pid);
    if (!ec)
        return 0;

    struct event *e = bpf_ringbuf_reserve(&rb, sizeof(*e), 0);
    if (!e)
        goto cleanup;

    if (ret == 0) {
        struct exec_success_data sd = {};
        sd.pid = pid;
        sd.tgid = ec->tgid;
        sd.uid = ec->uid;
        sd.start_ns = ec->start_ns;
        __builtin_memcpy(sd.comm, ec->comm, sizeof(sd.comm));
        __builtin_memcpy(sd.filename, ec->filename, sizeof(sd.filename));
        sd.argc = ec->argc;
        __builtin_memcpy(sd.args, ec->args, sizeof(sd.args));

        e->type = EVENT_EXEC_SUCCESS;
        __builtin_memcpy(e->data, &sd, sizeof(sd));
        bpf_ringbuf_submit(e, 0);

        bpf_map_update_elem(&active_pids, &pid, &ec->start_ns, BPF_ANY);
    } else {
        struct exec_failed_data fd = {};
        fd.pid = pid;
        fd.tgid = ec->tgid;
        fd.uid = ec->uid;
        fd.start_ns = ec->start_ns;
        __builtin_memcpy(fd.comm, ec->comm, sizeof(fd.comm));
        __builtin_memcpy(fd.filename, ec->filename, sizeof(fd.filename));
        fd.argc = ec->argc;
        __builtin_memcpy(fd.args, ec->args, sizeof(fd.args));
        fd.errno = ret;

        e->type = EVENT_EXEC_FAILED;
        __builtin_memcpy(e->data, &fd, sizeof(fd));
        bpf_ringbuf_submit(e, 0);
    }

cleanup:
    bpf_map_delete_elem(&exec_ctx_map, &pid);
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

    struct event *e = bpf_ringbuf_reserve(&rb, sizeof(*e), 0);
    if (e) {
        struct process_exit_data pd = {};
        pd.pid = pid;
        pd.tgid = bpf_get_current_pid_tgid() >> 32;
        pd.exit_ns = bpf_ktime_get_ns();
        pd.exit_code = 0;

        e->type = EVENT_PROCESS_EXIT;
        __builtin_memcpy(e->data, &pd, sizeof(pd));
        bpf_ringbuf_submit(e, 0);
    }

    bpf_map_delete_elem(&active_pids, &pid);
    return 0;
}

char _license[] SEC("license") = "GPL";
