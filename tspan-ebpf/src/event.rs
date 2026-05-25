/// Event structures shared with the eBPF program.

pub const ARG_BUF_SIZE: usize = 65536;
pub const FILENAME_SIZE: usize = 128;
pub const COMM_SIZE: usize = 16;

pub const EVENT_EXEC_SUCCESS: u32 = 1;
pub const EVENT_EXEC_FAILED: u32 = 2;
pub const EVENT_PROCESS_EXIT: u32 = 3;

#[derive(Debug, Clone)]
pub struct ExecSuccessInfo {
    pub pid: u32,
    pub tgid: u32,
    pub uid: u32,
    pub start_ns: u64,
    pub comm: String,
    pub filename: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExecFailedInfo {
    pub pid: u32,
    pub tgid: u32,
    pub uid: u32,
    pub start_ns: u64,
    pub comm: String,
    pub filename: String,
    pub args: Vec<String>,
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

/// Build alias and command from parsed exec info.
pub fn build_alias_and_command(filename: &str, args: &[String]) -> (String, String) {
    let alias = filename.to_string();
    let command = args.join(" ");
    (alias, command)
}
