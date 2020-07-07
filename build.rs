#![feature(asm)]

extern crate cc;

use std::fs;
use std::path::{PathBuf, Path};
use std::env;
use std::process::Command;
use std::os::unix::fs as unix_fs;

const FEATURE_CHECKS_PATH: &str = "feature_checks";

const C_DEPS_DIR: &str = "c_deps";
const C_DEPS_MAKEFILE: &str = "c_deps.mk";

/// Simple feature check, returning `true` if we have the feature.
///
/// The checks themselves are in files under `FEATURE_CHECKS_PATH`.
fn feature_check(filename: &str) -> bool {
    let mut path = PathBuf::new();
    path.push(FEATURE_CHECKS_PATH);
    path.push(filename);

    let mut check_build = cc::Build::new();
    check_build.file(path).try_compile("check_perf_pt").is_ok()
}

fn make_c_deps_dir() -> PathBuf {
    let out_dir = env::var("OUT_DIR").unwrap();
    let mut c_deps_dir = PathBuf::from(out_dir);
    c_deps_dir.push(C_DEPS_DIR);

    if !c_deps_dir.exists() {
        fs::create_dir(&c_deps_dir).unwrap();

        let mut dest = c_deps_dir.clone();
        dest.push(C_DEPS_MAKEFILE);

        let mut src = env::current_dir().unwrap().clone();
        src.push(C_DEPS_MAKEFILE);

        unix_fs::symlink(src, dest).unwrap();
    }

    c_deps_dir
}

fn build_libipt(c_deps_dir: &Path) {
    eprintln!("Building libipt...");

    let prev_dir = env::current_dir().unwrap();
    env::set_current_dir(&c_deps_dir).unwrap();
    let res = Command::new("make")
        .arg("-f")
        .arg(C_DEPS_MAKEFILE)
        .arg("libipt")
        .output()
        .unwrap_or_else(|_| panic!("Fatal error when building libipt"));
    if !res.status.success() {
        eprintln!("libipt build failed\n>>> stdout");
        eprintln!("stdout: {}", String::from_utf8_lossy(&res.stdout));
        eprintln!("\n>>> stderr");
        eprintln!("stderr: {}", String::from_utf8_lossy(&res.stderr));
        panic!();
    }

    env::set_current_dir(&prev_dir).unwrap();
}

// We always fetch libipt regardless of if we will build our own libipt. This is becuase there are
// a couple of private CPU configuration files that we need to borrow from libipt.
fn fetch_libipt(c_deps_dir: &Path) {
    eprintln!("Fetch libipt...");

    let prev_dir = env::current_dir().unwrap();
    env::set_current_dir(c_deps_dir).unwrap();
    let res = Command::new("make")
        .arg("processor-trace") // target just fetches the code.
        .output()
        .unwrap_or_else(|_| panic!("Fatal error when fetching libipt"));
    if !res.status.success() {
        eprintln!("libipt fetch failed\n>>> stdout");
        eprintln!("stdout: {}", String::from_utf8_lossy(&res.stdout));
        eprintln!("\n>>> stderr");
        eprintln!("stderr: {}", String::from_utf8_lossy(&res.stderr));
        panic!();
    }

    env::set_current_dir(&prev_dir).unwrap();
}

// Checks if the CPU supports Intel Processor Trace.
// We use this to decide whether to run the perf_pt backend tests. Although this would be better as
// a runtime check, it's OK since we won't distribute the test binary.
fn cpu_supports_pt() -> bool {
    const LEAF: u32 = 0x07;
    const SUBPAGE: u32 = 0x0;
    const EBX_BIT: u32 = 1 << 25;
    let ebx_out: u32;

    unsafe {
        asm!(
              "cpuid",
              inout("eax") LEAF => _,
              inout("ecx") SUBPAGE => _,
              lateout("ebx") ebx_out,
              lateout("edx") _,
        );
    }
    ebx_out & EBX_BIT != 0
}

fn main() {
    let mut c_build = cc::Build::new();

    let c_deps_dir = make_c_deps_dir();
    let c_deps_dir_s = c_deps_dir.display();

    // Check if we should build the perf_pt backend.
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        if feature_check("check_perf_pt.c") {
            c_build.file("src/backends/perf_pt/collect.c");
            c_build.file("src/backends/perf_pt/decode.c");
            c_build.file("src/backends/perf_pt/util.c");

            // XXX At the time of writing you can't conditionally build C code for tests in a build
            // script: https://github.com/rust-lang/cargo/issues/1581
            c_build.file("src/backends/perf_pt/test_helpers.c");

            // Decide whether to build our own libipt.
            if let Ok(val) = env::var("IPT_PATH") {
                let mut inc_path = PathBuf::from(val.clone());
                inc_path.push("include");
                c_build.include(inc_path);
                c_build.flag(&format!("-L{}/lib", val));
                println!("cargo:rustc-link-search={}/lib", val);
                println!("cargo:rustc-env=PTXED={}/bin/ptxed", val);
            } else {
                build_libipt(&c_deps_dir);
                c_build.include(&format!("{}/inst/include/", c_deps_dir_s));
                c_build.flag(&format!("-L{}/inst/lib", c_deps_dir_s));
                println!("cargo:rustc-link-search={}/inst/lib", c_deps_dir_s);
                println!("cargo:rustc-env=PTXED={}/inst/bin/ptxed", c_deps_dir_s);
            }

            // We borrow the CPU detection functions from libipt (they are not exposed publicly).
            // If we built our own libipt above, then the fetch is a no-op.
            fetch_libipt(&c_deps_dir);

            c_build.include(&format!(
                "{}/processor-trace/libipt/internal/include",
                c_deps_dir_s
            ));
            c_build.file(&format!(
                "{}/processor-trace/libipt/src/pt_cpu.c",
                c_deps_dir_s
            ));
            c_build.file(&format!(
                "{}/processor-trace/libipt/src/posix/pt_cpuid.c",
                c_deps_dir_s
            ));

            println!("cargo:rustc-cfg=perf_pt");
            if cpu_supports_pt() {
                println!("cargo:rustc-cfg=perf_pt_test");
            }
            println!("cargo:rustc-link-lib=ipt");

            // XXX Cargo bug: no way to encode an rpath, otherwise we would do that here:
            // https://github.com/rust-lang/cargo/issues/5077
            //
            // Until this is implemented, the user will need to add `c_deps_dir` to
            // LD_LIBRARY_PATH if the build process compiles its own libipt.
        }
    }
    c_build.include("src/util");
    c_build.compile("hwtracer_c");

    // Additional circumstances under which to re-run this build.rs.
    println!("cargo:rerun-if-env-changed=IPT_PATH");
    println!("cargo:rerun-if-changed=src/util");
    println!("cargo:rerun-if-changed={}", c_deps_dir_s);
    println!("cargo:rerun-if-changed=src/backends/perf_pt");
    println!(
        "cargo:rerun-if-changed={}/processor-trace/libipt/src/pt_cpu.c",
        c_deps_dir_s
    );
    println!(
        "cargo:rerun-if-changed={}/processor-trace/libipt/src/posix/pt_cpuid.c",
        c_deps_dir_s
    );
}
