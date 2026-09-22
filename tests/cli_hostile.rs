//! The CLI must report malformed input as an error, never panic.

use std::process::Command;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nexcomp-cli-hostile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Run the CLI and return (exit code, stderr).
fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_nexcomp")).args(args).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn truncated_encrypted_wrapper_is_an_error_not_a_panic() {
    let bad = scratch("truncated.nxc");
    std::fs::write(&bad, b"NXE2").unwrap();
    let out = scratch("out.bin");
    for args in [
        vec!["decompress", bad.to_str().unwrap(), out.to_str().unwrap(), "--decrypt", "test"],
        vec!["inspect", bad.to_str().unwrap(), "--decrypt", "test"],
    ] {
        let (code, stderr) = run(&args);
        assert_ne!(code, 0, "{args:?} should fail");
        assert!(!stderr.contains("panicked"), "{args:?} panicked: {stderr}");
    }
}
