use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_whut-wifi-maintainer"))
}

#[test]
fn validation_is_offline_and_does_not_rewrite_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("selected.toml");
    let config = if cfg!(windows) {
        "username='test'\npassword='secret'\n"
    } else {
        "username='test'\npassword='secret'\nwired=true\nwired_interface='no-such-iface'\n"
    };
    std::fs::write(&path, config).unwrap();
    let output = binary()
        .args(["--non-interactive", "--check-config", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), config);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
}

#[test]
fn missing_explicit_config_exits_without_prompting_or_fallback() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("config.toml"),
        "username='fallback'\npassword='secret'",
    )
    .unwrap();
    let output = binary()
        .current_dir(directory.path())
        .args(["--non-interactive", "--config", "missing.toml"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("请输入"));
}

#[test]
fn invalid_config_diagnostic_never_echoes_secret() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid.toml");
    std::fs::write(
        &path,
        "username='test'\npassword = credential-that-must-stay-private",
    )
    .unwrap();
    let output = binary()
        .args(["--check-config", "--config"])
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("credential-that-must-stay-private"));
}
