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

#[test]
fn pre_release_formats_are_refused_with_a_clear_error() {
    let out = scratch("out2.bin");
    for (name, magic) in [("nxc1", &b"NXC\x01"[..]), ("nx12", b"NX12"), ("nxe1", b"NXE1")] {
        let file = scratch(&format!("{name}.nxc"));
        let mut data = magic.to_vec();
        data.extend_from_slice(&[0xFF; 64]);
        std::fs::write(&file, &data).unwrap();
        for args in [
            vec!["decompress", file.to_str().unwrap(), out.to_str().unwrap()],
            vec!["inspect", file.to_str().unwrap()],
        ] {
            let (code, stderr) = run(&args);
            assert_ne!(code, 0, "{args:?} should fail");
            assert!(stderr.contains("pre-release"), "{args:?}: {stderr}");
            assert!(!stderr.contains("panicked"), "{args:?} panicked: {stderr}");
        }
    }
}

/// Run the CLI with extra environment variables.
fn run_env(args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nexcomp"));
    cmd.args(args).env_remove("NEXCOMP_PASSWORD");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn password_comes_from_a_file_or_the_environment_not_argv() {
    let input = scratch("plain.txt");
    std::fs::write(&input, b"attack at dawn ".repeat(100)).unwrap();
    let pw_file = scratch("pw.txt");
    std::fs::write(&pw_file, b"s3cret\n").unwrap();
    let sealed = scratch("sealed.nxc");
    let restored = scratch("restored.txt");
    let [input, pw_file, sealed, restored] = [&input, &pw_file, &sealed, &restored].map(|p| p.to_str().unwrap());

    let (code, stderr) = run_env(&["compress", input, sealed, "--encrypt", "--password-file", pw_file], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("warning"), "{stderr}");
    assert!(std::fs::read(sealed).unwrap().starts_with(b"NXE3"));

    // The trailing newline of the file is not part of the password.
    let (code, stderr) = run_env(&["decompress", sealed, restored], &[("NEXCOMP_PASSWORD", "s3cret")]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(restored).unwrap(), std::fs::read(input).unwrap());

    let (code, stderr) = run_env(&["inspect", sealed, "--password-file", pw_file], &[]);
    assert_eq!(code, 0, "{stderr}");

    let (code, stderr) = run_env(&["decompress", sealed, restored], &[("NEXCOMP_PASSWORD", "wrong")]);
    assert_ne!(code, 0);
    assert!(stderr.contains("authentication"), "{stderr}");

    // No terminal and no password source: a clear error, not a hang.
    let (code, stderr) = run_env(&["decompress", sealed, restored], &[]);
    assert_ne!(code, 0);
    assert!(stderr.contains("needs a password"), "{stderr}");
}

#[test]
fn password_on_the_command_line_still_works_with_a_warning() {
    let input = scratch("plain2.txt");
    std::fs::write(&input, b"hello").unwrap();
    let sealed = scratch("sealed2.nxc");
    let restored = scratch("restored2.txt");
    let [input, sealed, restored] = [&input, &sealed, &restored].map(|p| p.to_str().unwrap());

    let (code, stderr) = run_env(&["compress", input, sealed, "--encrypt", "pw"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("warning"), "{stderr}");
    let (code, stderr) = run_env(&["decompress", sealed, restored, "--decrypt", "pw"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("warning"), "{stderr}");
    assert_eq!(std::fs::read(restored).unwrap(), b"hello");
}

#[test]
fn hostile_kdf_costs_are_refused_quickly() {
    let mut file = b"NXE3\x01".to_vec();
    for cost in [u32::MAX, u32::MAX, u32::MAX] {
        file.extend_from_slice(&cost.to_le_bytes());
    }
    file.extend_from_slice(&0u64.to_le_bytes());
    file.extend_from_slice(&[0u8; 60]);
    let bad = scratch("hostile_kdf.nxc");
    std::fs::write(&bad, &file).unwrap();
    let out = scratch("out3.bin");
    let start = std::time::Instant::now();
    let (code, stderr) =
        run_env(&["decompress", bad.to_str().unwrap(), out.to_str().unwrap()], &[("NEXCOMP_PASSWORD", "pw")]);
    assert_ne!(code, 0);
    assert!(stderr.contains("out of range"), "{stderr}");
    assert!(start.elapsed().as_secs() < 5);
}
