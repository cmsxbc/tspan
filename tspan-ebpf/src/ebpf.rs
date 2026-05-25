use anyhow::{Context, Result};
use aya::maps::RingBuf;
use aya::programs::TracePoint;
use aya::Ebpf;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::event::{
    ExecFailedData, ExecSuccessData, ProcessExitData, RawEvent, EVENT_EXEC_FAILED,
    EVENT_EXEC_SUCCESS, EVENT_PROCESS_EXIT,
};

#[derive(Debug, Clone)]
pub enum EbpfEvent {
    Success(ExecSuccessData),
    Failed(ExecFailedData),
    Exit(ProcessExitData),
}

pub fn load_and_attach() -> Result<Ebpf> {
    let bpf_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/main.bpf.o"));

    #[cfg(debug_assertions)]
    let mut ebpf = Ebpf::load(bpf_bytes)?;
    #[cfg(not(debug_assertions))]
    let mut ebpf = Ebpf::load(bpf_bytes)?;

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
            let raw: &RawEvent = unsafe { std::mem::transmute(item.as_ptr()) };
            match raw.ty {
                EVENT_EXEC_SUCCESS => {
                    let data: &ExecSuccessData = unsafe { std::mem::transmute(raw.data.as_ptr()) };
                    if tx.send(EbpfEvent::Success(*data)).await.is_err() {
                        break;
                    }
                }
                EVENT_EXEC_FAILED => {
                    let data: &ExecFailedData = unsafe { std::mem::transmute(raw.data.as_ptr()) };
                    if tx.send(EbpfEvent::Failed(*data)).await.is_err() {
                        break;
                    }
                }
                EVENT_PROCESS_EXIT => {
                    let data: &ProcessExitData = unsafe { std::mem::transmute(raw.data.as_ptr()) };
                    if tx.send(EbpfEvent::Exit(*data)).await.is_err() {
                        break;
                    }
                }
                _ => {
                    tracing::warn!("unknown event type: {}", raw.ty);
                }
            }
        } else {
            sleep(Duration::from_millis(5)).await;
        }
    }

    Ok(())
}
