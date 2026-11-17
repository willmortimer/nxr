//! The resource guardian keeps output locks after the supervisor process is gone.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn nxr() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nxr"))
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

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

#[test]
fn a_dead_supervisor_leaves_the_writer_lock_held() {
    let root = tempfile::tempdir().expect("root");
    let target = tempfile::tempdir().expect("target");
    let result = root.path().join("writer.pid");
    let supervisor = Command::new(nxr())
        .env("NXR_GUARDIAN_SUPERVISOR_ONCE", "1")
        .env("NXR_GUARDIAN_DIRS", target.path())
        .env(
            "NXR_GUARDIAN_ARGV",
            serde_json::to_string(&["/bin/sleep", "30"]).expect("argv"),
        )
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
    let blocked = Command::new(nxr())
        .env("NXR_GUARDIAN_SUPERVISOR_ONCE", "1")
        .env("NXR_GUARDIAN_DIRS", target.path())
        .env(
            "NXR_GUARDIAN_ARGV",
            serde_json::to_string(&["/bin/sleep", "30"]).expect("argv"),
        )
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
    kill_pid(writer);
    let released = Instant::now() + Duration::from_secs(5);
    let mut free = false;
    while Instant::now() < released {
        let again = Command::new(nxr())
            .env("NXR_GUARDIAN_SUPERVISOR_ONCE", "1")
            .env("NXR_GUARDIAN_DIRS", target.path())
            .env(
                "NXR_GUARDIAN_ARGV",
                serde_json::to_string(&["/bin/echo", "done"]).expect("argv"),
            )
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
