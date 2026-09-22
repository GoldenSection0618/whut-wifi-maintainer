#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn finish_promptly(command: &mut Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("invalid configuration did not exit: {:?}", output);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn invalid_startup_protects_configuration_and_exits_without_network_wait() {
    let directory = std::env::temp_dir().join(format!(
        "whut-startup-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    fs::create_dir(&directory).unwrap();
    let binary = directory.join("whut-wifi-maintainer");
    fs::copy(env!("CARGO_BIN_EXE_whut-wifi-maintainer"), &binary).unwrap();
    let path = directory.join("config.toml");
    for settings in [
        "",
        "wired=false\nwired_interface='wan'",
        "wired=true",
        "wired=true\nwired_interface=''",
        "wired=true\nwired_interface=' '",
        "wired=true\nwired_interface=' wan '",
    ] {
        let content = format!("username='fixture'\npassword='fixture-secret'\n{settings}");
        fs::write(&path, &content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let output = finish_promptly(Command::new(&binary).current_dir(&directory));
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("wired"), "{stderr}");
        assert!(!stderr.contains("fixture-secret"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("保持器已启动"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }
    fs::remove_file(path).unwrap();
    fs::remove_file(binary).unwrap();
    fs::remove_dir(directory).unwrap();
}
