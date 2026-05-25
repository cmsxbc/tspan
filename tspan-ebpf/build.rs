use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let ebpf_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("ebpf");
    let src = ebpf_dir.join("main.bpf.c");
    let dst = out_dir.join("main.bpf.o");
    let vmlinux_h = ebpf_dir.join("vmlinux.h");

    // Generate vmlinux.h if missing
    if !vmlinux_h.exists() {
        let status = std::process::Command::new("bpftool")
            .args(&["btf", "dump", "file", "/sys/kernel/btf/vmlinux", "format", "c"])
            .stdout(std::fs::File::create(&vmlinux_h).expect("failed to create vmlinux.h"))
            .status()
            .expect("failed to run bpftool to generate vmlinux.h");
        if !status.success() {
            panic!("bpftool failed to generate vmlinux.h");
        }
    }

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-changed={}", vmlinux_h.display());
    println!("cargo:rerun-if-changed=/sys/kernel/btf/vmlinux");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_arch = match arch.as_str() {
        "x86_64" => "x86",
        "aarch64" => "arm64",
        _ => panic!("unsupported arch for eBPF: {}", arch),
    };

    let status = Command::new("clang")
        .arg("-O2")
        .arg("-g")
        .arg("-target")
        .arg("bpf")
        .arg(format!("-D__TARGET_ARCH_{}", target_arch))
        .arg("-I")
        .arg(&ebpf_dir)
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&dst)
        .status()
        .expect("failed to run clang to compile eBPF program");

    if !status.success() {
        panic!("clang failed to compile eBPF program");
    }
}
