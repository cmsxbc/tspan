/// Event structures shared with the eBPF program.
/// Must match `ebpf/main.bpf.c` exactly.

pub const MAX_ARGS: usize = 2;
pub const ARG_SIZE: usize = 32;
pub const FILENAME_SIZE: usize = 128;
pub const EVENT_DATA_SIZE: usize = 256;

pub const EVENT_EXEC_SUCCESS: u32 = 1;
pub const EVENT_EXEC_FAILED: u32 = 2;
pub const EVENT_PROCESS_EXIT: u32 = 3;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct RawEvent {
    pub ty: u32,
    pub data: [u8; EVENT_DATA_SIZE],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExecSuccessData {
    pub pid: u32,
    pub tgid: u32,
    pub uid: u32,
    pub start_ns: u64,
    pub comm: [u8; 16],
    pub filename: [u8; FILENAME_SIZE],
    pub argc: u8,
    pub args: [[u8; ARG_SIZE]; MAX_ARGS],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExecFailedData {
    pub pid: u32,
    pub tgid: u32,
    pub uid: u32,
    pub start_ns: u64,
    pub comm: [u8; 16],
    pub filename: [u8; FILENAME_SIZE],
    pub argc: u8,
    pub args: [[u8; ARG_SIZE]; MAX_ARGS],
    pub errno: i64,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProcessExitData {
    pub pid: u32,
    pub tgid: u32,
    pub exit_ns: u64,
    pub exit_code: u32,
}

/// Helper to convert a null-terminated byte array to a String.
pub fn bytes_to_string(buf: &[u8]) -> String {
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// Build command string from filename + args.
pub fn build_command(filename: &[u8], argc: u8, args: &[[u8; ARG_SIZE]; MAX_ARGS]) -> String {
    let mut cmd = bytes_to_string(filename);
    for i in 0..(argc as usize).min(MAX_ARGS) {
        let arg = bytes_to_string(&args[i]);
        if !arg.is_empty() {
            cmd.push(' ');
            cmd.push_str(&arg);
        }
    }
    cmd
}
