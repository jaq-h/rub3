use std::process::Command;

fn wrapper_bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_deotp-wrapper").into()
}

#[cfg(target_os = "macos")]
const TRUE_BIN: &str = "/usr/bin/true";
#[cfg(not(target_os = "macos"))]
const TRUE_BIN: &str = "/bin/true";

#[cfg(target_os = "macos")]
const FALSE_BIN: &str = "/usr/bin/false";
#[cfg(not(target_os = "macos"))]
const FALSE_BIN: &str = "/bin/false";

#[test]
fn runs_child_and_exits_zero() {
    let status = Command::new(wrapper_bin())
        .args(["--binary", TRUE_BIN])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn propagates_nonzero_exit_code() {
    let status = Command::new(wrapper_bin())
        .args(["--binary", FALSE_BIN])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn passes_args_to_child() {
    let output = Command::new(wrapper_bin())
        .args(["--binary", "/bin/echo", "--", "hello", "deotp"])
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"hello deotp\n");
}

#[test]
fn errors_on_missing_binary() {
    let status = Command::new(wrapper_bin())
        .args(["--binary", "/nonexistent/binary"])
        .status()
        .unwrap();
    assert_ne!(status.code(), Some(0));
}
