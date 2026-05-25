use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ClientIdMode {
    /// Use hostname only (e.g. `myhost`)
    Hostname,
    /// Append UID (e.g. `myhost-1000`)
    #[value(name = "hostname-uid")]
    HostnameUid,
    /// Append username (e.g. `myhost-alice`)
    #[value(name = "hostname-user")]
    HostnameUser,
    /// Append both UID and username (e.g. `myhost-1000-alice`)
    #[value(name = "hostname-uid-user")]
    HostnameUidUser,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "tspan-ebpf", version)]
#[command(about = "TSPAN eBPF process execution tracker")]
pub struct Config {
    #[arg(long, env = "TSPAN_EBPF_SERVER", default_value = "http://localhost:8080")]
    pub server: String,

    #[arg(long, env = "TSPAN_EBPF_TOKEN")]
    pub token: String,

    #[arg(long, env = "TSPAN_EBPF_CLIENT", default_value_t = get_hostname())]
    pub client_id: String,

    #[arg(long, env = "TSPAN_EBPF_CLIENT_MODE", default_value = "hostname")]
    pub client_id_mode: ClientIdMode,

    #[arg(long, env = "TSPAN_EBPF_RETRY_FILE", default_value = "/var/lib/tspan-ebpf/retry.jsonl")]
    pub retry_file: String,

    /// Comma-separated list of allowed UIDs (empty = all)
    #[arg(long, env = "TSPAN_EBPF_ALLOW_UIDS", value_delimiter = ',')]
    pub allow_uids: Vec<u32>,

    /// Regex pattern for commands to deny (empty = none)
    #[arg(long, env = "TSPAN_EBPF_DENY_COMM")]
    pub deny_comm: Option<String>,
}

fn get_hostname() -> String {
    let mut buf = [0u8; 256];
    unsafe {
        if libc::gethostname(buf.as_mut_ptr() as *mut i8, buf.len()) == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..len]).into_owned();
        }
    }
    "unknown".to_string()
}
