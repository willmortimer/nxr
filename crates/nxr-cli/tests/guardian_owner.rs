//! The resource guardian keeps output locks after the supervisor process is gone.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn nxr() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nxr"))
}

fn host_tool(name: &str) -> String {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    panic!("test tool {name} is not on PATH");
}

fn wait_for_file(path: &std::path::Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.is_file() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_dead_supervisor_leaves_the_writer_lock_held() {
    let root = tempfile::tempdir().expect("root");
    let target = tempfile::tempdir().expect("target");
    let result = root.path().join("writer.pid");
    // The Nix Linux sandbox does not let this test signal an unrelated pid.
    // The writer exits when the stop file appears, which is the same condition
    // the guardian waits for.
    let stop = root.path().join("stop");
    let sleep_argv = serde_json::to_string(&[
        host_tool("bash"),
        "-c".to_owned(),
        "while [ ! -e \"$1\" ]; do sleep 0.05; done".to_owned(),
        "bash".to_owned(),
        stop.display().to_string(),
    ])
    .expect("argv");
    let echo_argv = serde_json::to_string(&[host_tool("echo"), "done".to_owned()]).expect("argv");
    let supervisor = Command::new(nxr())
        .env("NXR_GUARDIAN_SUPERVISOR_ONCE", "1")
        .env("NXR_GUARDIAN_DIRS", target.path())
        .env("NXR_GUARDIAN_ARGV", sleep_argv.clone())
        .env("NXR_GUARDIAN_RESULT", &result)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("supervisor");
    let status = supervisor.wait_with_output().expect("wait supervisor");
    assert!(status.status.success(), "supervisor admission");
    assert!(wait_for_file(&result), "writer pid");
    let writer: u32 = std::fs::read_to_string(&result)
        .expect("pid text")
        .trim()
        .parse()
        .expect("pid");
    assert!(writer > 0, "writer pid");
    let blocked = Command::new(nxr())
        .env("NXR_GUARDIAN_SUPERVISOR_ONCE", "1")
        .env("NXR_GUARDIAN_DIRS", target.path())
        .env("NXR_GUARDIAN_ARGV", sleep_argv.clone())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("second supervisor");
    assert!(!blocked.status.success(), "second writer must be refused");
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        stderr.contains("already has a running local build") || stderr.contains("guardian"),
        "{stderr}"
    );
    std::fs::write(&stop, b"stop").expect("stop writer");
    let released = Instant::now() + Duration::from_secs(10);
    let mut free = false;
    while Instant::now() < released {
        let again = Command::new(nxr())
            .env("NXR_GUARDIAN_SUPERVISOR_ONCE", "1")
            .env("NXR_GUARDIAN_DIRS", target.path())
            .env("NXR_GUARDIAN_ARGV", echo_argv.clone())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .expect("third supervisor");
        if again.status.success() {
            free = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        free,
        "the guardian releases the lock after the writer exits"
    );
}
