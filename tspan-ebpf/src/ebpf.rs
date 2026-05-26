use anyhow::{Context, Result};
use aya::maps::RingBuf;
use aya::programs::TracePoint;
use aya::Ebpf;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::event::{
    ExecFailedInfo, ExecSuccessInfo, ProcessExitData, ARG_BUF_SIZE, COMM_SIZE,
    EVENT_EXEC_FAILED, EVENT_EXEC_SUCCESS, EVENT_PROCESS_EXIT, FILENAME_SIZE,
};

#[derive(Debug, Clone)]
pub enum EbpfEvent {
    Success(ExecSuccessInfo),
    Failed(ExecFailedInfo),
    Exit(ProcessExitData),
}

pub fn load_and_attach() -> Result<Ebpf> {
    let bpf_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/main.bpf.o")).to_vec();

    let mut ebpf = Ebpf::load(&bpf_bytes)?;

    let program: &mut TracePoint = ebpf
        .program_mut("trace_enter_execve")
        .context("trace_enter_execve not found")?
        .try_into()?;
    program.load()?;
    program.attach("syscalls", "sys_enter_execve")?;

    let program: &mut TracePoint = ebpf
        .program_mut("trace_enter_execveat")
        .context("trace_enter_execveat not found")?
        .try_into()?;
    program.load()?;
    program.attach("syscalls", "sys_enter_execveat")?;

    let program: &mut TracePoint = ebpf
        .program_mut("trace_exit_execve")
        .context("trace_exit_execve not found")?
        .try_into()?;
    program.load()?;
    program.attach("syscalls", "sys_exit_execve")?;

    let program: &mut TracePoint = ebpf
        .program_mut("trace_exit_execveat")
        .context("trace_exit_execveat not found")?
        .try_into()?;
    program.load()?;
    program.attach("syscalls", "sys_exit_execveat")?;

    let program: &mut TracePoint = ebpf
        .program_mut("trace_process_exit")
        .context("trace_process_exit not found")?
        .try_into()?;
    program.load()?;
    program.attach("sched", "sched_process_exit")?;

    Ok(ebpf)
}

fn bytes_to_string(buf: &[u8]) -> String {
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

fn parse_args(args_data: &[u8], argc: u32) -> Vec<String> {
    let mut args = Vec::new();
    for s in args_data.split(|&b| b == 0) {
        if s.is_empty() {
            continue;
        }
        args.push(bytes_to_string(s));
        if args.len() >= argc as usize {
            break;
        }
    }
    args
}

pub async fn poll_ring_buffer(
    mut ebpf: Ebpf,
    tx: mpsc::Sender<EbpfEvent>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let map = ebpf.map_mut("rb").context("ring buffer map 'rb' not found")?;
    let mut ring_buf = RingBuf::try_from(map)?;

    loop {
        if *shutdown.borrow() {
            break;
        }

        if let Some(item) = ring_buf.next() {
            if item.len() < 4 {
                continue;
            }
            let ty = u32::from_ne_bytes(item[0..4].try_into().unwrap());
            match ty {
                EVENT_EXEC_SUCCESS | EVENT_EXEC_FAILED => {
                    if item.len() < 184 {
                        continue;
                    }
                    let pid = u32::from_ne_bytes(item[4..8].try_into().unwrap());
                    let tgid = u32::from_ne_bytes(item[8..12].try_into().unwrap());
                    let uid = u32::from_ne_bytes(item[12..16].try_into().unwrap());
                    let start_ns = u64::from_ne_bytes(item[16..24].try_into().unwrap());
                    let filename = bytes_to_string(&item[24..24 + FILENAME_SIZE]);
                    let comm = bytes_to_string(&item[152..152 + COMM_SIZE]);
                    let argc = u32::from_ne_bytes(item[168..172].try_into().unwrap());
                    let args_len = u32::from_ne_bytes(item[172..176].try_into().unwrap()) as usize;
                    let errno = i64::from_ne_bytes(item[176..184].try_into().unwrap());

                    let actual_len = args_len.min(ARG_BUF_SIZE);
                    let args = parse_args(&item[184..184 + actual_len], argc);

                    let event = if ty == EVENT_EXEC_SUCCESS {
                        EbpfEvent::Success(ExecSuccessInfo {
                            pid,
                            tgid,
                            uid,
                            start_ns,
                            comm,
                            filename,
                            args,
                        })
                    } else {
                        EbpfEvent::Failed(ExecFailedInfo {
                            pid,
                            tgid,
                            uid,
                            start_ns,
                            comm,
                            filename,
                            args,
                            errno,
                        })
                    };

                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
                EVENT_PROCESS_EXIT => {
                    if item.len() < 32 {
                        continue;
                    }
                    let pid = u32::from_ne_bytes(item[4..8].try_into().unwrap());
                    let tgid = u32::from_ne_bytes(item[8..12].try_into().unwrap());
                    // u64 exit_ns at offset 16 (after 4-byte padding for alignment)
                    let exit_ns = u64::from_ne_bytes(item[16..24].try_into().unwrap());
                    let exit_code = u32::from_ne_bytes(item[24..28].try_into().unwrap());
                    let event = EbpfEvent::Exit(ProcessExitData {
                        pid,
                        tgid,
                        exit_ns,
                        exit_code,
                    });
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
                _ => {
                    tracing::warn!("unknown event type: {}", ty);
                }
            }
        } else {
            sleep(Duration::from_millis(5)).await;
        }
    }

    Ok(())
}
