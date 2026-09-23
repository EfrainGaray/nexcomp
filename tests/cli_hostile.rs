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

#[test]
fn version_and_inspect_report_the_real_version_and_format() {
    let out = Command::new(env!("CARGO_BIN_EXE_nexcomp")).arg("--version").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("nexcomp {}", env!("CARGO_PKG_VERSION")));

    let input = scratch("plain4.txt");
    std::fs::write(&input, b"hello hello hello").unwrap();
    let packed = scratch("packed4.nxc");
    let (code, stderr) = run(&["compress", input.to_str().unwrap(), packed.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    let out = Command::new(env!("CARGO_BIN_EXE_nexcomp")).args(["inspect", packed.to_str().unwrap()]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("format=NX15 size=17 "), "{out:?}");
}

/// A zero-byte file is not an archive. Treating one as an empty original made
/// a `.nxc` truncated by a failed copy look like a successful restore.
#[test]
fn a_zero_byte_archive_is_refused() {
    let empty = scratch("zero.nxc");
    std::fs::write(&empty, b"").unwrap();
    let out = scratch("zero.out");
    let _ = std::fs::remove_file(&out);
    let (code, stderr) = run(&["decompress", empty.to_str().unwrap(), out.to_str().unwrap()]);
    assert_ne!(code, 0, "a zero-byte archive must fail: {stderr}");
    assert!(!out.exists(), "nothing may be written for a refused archive");
}

/// Compressing nothing still writes a container, so the empty file round-trips
/// through the same format as everything else.
#[test]
fn empty_input_round_trips_through_a_container() {
    let src = scratch("empty.bin");
    std::fs::write(&src, b"").unwrap();
    let archive = scratch("empty.nxc");
    let out = scratch("empty.out");
    let (code, stderr) = run(&["compress", src.to_str().unwrap(), archive.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(std::fs::metadata(&archive).unwrap().len() > 0, "an archive always has its magic");
    let (code, stderr) = run(&["decompress", archive.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(&out).unwrap(), b"");
}

/// 1.7.0 and earlier sealed an empty input as an empty payload rather than as
/// a container. The wrapper's tag vouches for that emptiness, so those files
/// have to keep restoring.
#[test]
fn an_empty_file_sealed_by_an_earlier_release_still_opens() {
    let sealed = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compat/empty-sealed-by-1.7.0.nxe3");
    let out = scratch("sealed-empty.out");
    let _ = std::fs::remove_file(&out);
    let run_with_password = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_nexcomp"))
            .args(args)
            .env("NEXCOMP_PASSWORD", "nexcomp-fixture")
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let (code, stderr) = run_with_password(&["decompress", sealed, out.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(&out).unwrap(), b"");
    let (code, stderr) = run_with_password(&["inspect", sealed]);
    assert_eq!(code, 0, "{stderr}");
}

/// The output is staged under a temporary name, which must not follow a
/// symlink someone planted, must not destroy the destination when the decode
/// fails, and must not turn a device into a regular file.
#[test]
fn staging_the_output_does_not_follow_symlinks_or_break_devices() {
    let dir = scratch("staging");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.bin");
    std::fs::write(&source, vec![7u8; 40_000]).unwrap();
    let archive = dir.join("source.nxc");
    let (code, stderr) = run(&["compress", source.to_str().unwrap(), archive.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");

    // A device is written through, not renamed onto.
    let (code, stderr) = run(&["decompress", archive.to_str().unwrap(), "/dev/null"]);
    assert_eq!(code, 0, "writing to /dev/null: {stderr}");

    // A planted temporary must not be followed to somewhere else.
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"victim").unwrap();
    let dest = dir.join("dest.bin");
    for attempt in 0..4 {
        let planted = dir.join(format!("dest.bin.part{}-{attempt}", std::process::id()));
        let _ = std::os::unix::fs::symlink(&victim, &planted);
    }
    let (code, stderr) = run(&["decompress", archive.to_str().unwrap(), dest.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(&victim).unwrap(), b"victim", "the symlink target was written through");
    assert_eq!(std::fs::read(&dest).unwrap().len(), 40_000);

    // A failed decode leaves the destination as it was.
    let truncated = dir.join("truncated.nxc");
    let whole = std::fs::read(&archive).unwrap();
    std::fs::write(&truncated, &whole[..whole.len() / 2]).unwrap();
    let precious = dir.join("precious.txt");
    std::fs::write(&precious, b"precious").unwrap();
    let (code, _) = run(&["decompress", truncated.to_str().unwrap(), precious.to_str().unwrap()]);
    assert_ne!(code, 0);
    assert_eq!(std::fs::read(&precious).unwrap(), b"precious");
}
