//! `scripts/gate.sh` cannot hang forever. A test that never exits (a watcher, a
//! lock nobody releases) used to hold the gate, the machine-wide gate lock, and
//! every session queued behind it. The gate now runs `cargo test` under
//! `scripts/with-timeout.py`; these tests run the real script and the real
//! wrapper with stand-in tool binaries on PATH, so no cargo build and no real
//! test suite runs.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("post-gate-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod script");
}

fn real_python() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .expect("python3 on PATH")
}

/// Stand-in `cargo`, `node`, and `python3` that succeed instantly, except that
/// `cargo test` runs `on_test` (a shell snippet). `python3` passes the timeout
/// wrapper through to the real interpreter and nothing else.
fn shim_dir(root: &Path, on_test: &str) -> PathBuf {
    let dir = root.join("shims");
    fs::create_dir_all(&dir).expect("shim dir");
    executable(
        &dir.join("cargo"),
        &format!(
            r#"#!/bin/sh
case "$1" in
  --version) echo "cargo 0.0.0 (shim)";;
  test) {on_test};;
  *) exit 0;;
esac
"#
        ),
    );
    let fake_post = root.join("post");
    executable(&fake_post, "#!/bin/sh\nexit 0\n");
    executable(
        &dir.join("node"),
        &format!(
            r#"#!/bin/sh
case "$1" in
  scripts/cargo-release-bin.mjs) echo "{}";;
  --version) echo v0.0.0;;
  *) exit 0;;
esac
"#,
            fake_post.display()
        ),
    );
    executable(
        &dir.join("python3"),
        &format!(
            r#"#!/bin/sh
case "$1" in
  scripts/with-timeout.py|--version) exec "{}" "$@";;
  *) exit 0;;
esac
"#,
            real_python().display()
        ),
    );
    dir
}

fn run_gate(shims: &Path, timeout_seconds: Option<&str>) -> (Output, Duration) {
    let mut command = Command::new("bash");
    command
        .arg(repo().join("scripts/gate.sh"))
        .env("PATH", format!("{}:/usr/bin:/bin", shims.display()))
        // A developer's non-interactive shell start-up file (BASH_ENV) may put
        // the real toolchain back in front of the stand-ins, and the gate
        // would then run the real suite.
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match timeout_seconds {
        Some(seconds) => command.env("GATE_TEST_TIMEOUT", seconds),
        None => command.env_remove("GATE_TEST_TIMEOUT"),
    };
    let started = Instant::now();
    let output = command.output().expect("run gate.sh");
    (output, started.elapsed())
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn a_hung_test_step_fails_the_gate_and_takes_its_children_with_it() {
    let root = scratch("hang");
    let pidfile = root.join("child.pid");
    // A "test binary" that never exits, spawned by the `cargo test` stand-in.
    let shims = shim_dir(
        &root,
        &format!(
            r#"sleep 300 &
    echo $! > "{}"
    wait"#,
            pidfile.display()
        ),
    );

    let (output, elapsed) = run_gate(&shims, Some("1"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("GATE FAIL: cargo test") && stderr.contains("GATE_TEST_TIMEOUT"),
        "the failure names the step and the knob: {stderr}"
    );
    assert!(
        stderr.contains("GATE FAILED") && !stdout.contains("GATE PASS"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "the gate returned in {elapsed:?}, not after the 300 s hang"
    );

    let child = fs::read_to_string(&pidfile).expect("the stand-in test started its child");
    let child = child.trim();
    // The kill is prompt (SIGTERM to the group); give the reaper a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(child) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !alive(child),
        "the hung test's child {child} was left running"
    );
    fs::remove_dir_all(&root).expect("remove scratch");
}

#[test]
fn a_test_step_that_finishes_in_time_passes_the_gate() {
    let root = scratch("control");
    let shims = shim_dir(&root, "exit 0");

    let (output, _) = run_gate(&shims, Some("30"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(!stderr.contains("GATE FAIL"), "stderr: {stderr}");
    fs::remove_dir_all(&root).expect("remove scratch");
}

#[test]
fn a_failing_test_step_still_fails_the_gate_with_its_own_message() {
    let root = scratch("failing");
    let shims = shim_dir(&root, "exit 3");

    let (output, _) = run_gate(&shims, None);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("GATE FAIL: cargo test"), "stderr: {stderr}");
    fs::remove_dir_all(&root).expect("remove scratch");
}

fn with_timeout(args: &[&str]) -> (Output, Duration) {
    let started = Instant::now();
    let output = Command::new("python3")
        .arg(repo().join("scripts/with-timeout.py"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run with-timeout.py");
    (output, started.elapsed())
}

#[test]
fn with_timeout_passes_the_commands_exit_status_through() {
    let (output, _) = with_timeout(&["30", "sh", "-c", "exit 7"]);
    assert_eq!(output.status.code(), Some(7));
    let (output, _) = with_timeout(&["30", "sh", "-c", "echo out; exit 0"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "out\n");
}

#[test]
fn with_timeout_kills_a_command_that_outlives_its_limit() {
    let (output, elapsed) = with_timeout(&["1", "sleep", "300"]);
    assert_eq!(output.status.code(), Some(124));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exceeded 1 s"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");
}

/// The command itself exits on SIGTERM, but a grandchild in its group traps
/// it. Stopping at the command's exit would leave that grandchild running
/// after the limit; the group gets SIGKILL after the grace period regardless.
/// The grandchild's output goes to /dev/null so that a regression fails on the
/// survivor check below instead of blocking on the inherited pipe for 300 s.
#[test]
fn with_timeout_kills_a_grandchild_that_ignores_sigterm_after_the_command_exits() {
    let root = scratch("term-trap");
    let pidfile = root.join("grandchild.pid");
    let script = format!(
        r#"( trap '' TERM; exec sleep 300 >/dev/null 2>&1 ) &
echo $! > "{}"
sleep 300"#,
        pidfile.display()
    );
    let (output, elapsed) = with_timeout(&["1", "sh", "-c", &script]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(124), "stderr: {stderr}");
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");

    let grandchild = fs::read_to_string(&pidfile).expect("the command started its grandchild");
    let grandchild = grandchild.trim();
    // SIGKILL is prompt; give the reaper a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let survived = alive(grandchild);
    if survived {
        // Do not leave the 300 s sleep behind when the assertion fails.
        let _ = Command::new("kill").args(["-9", grandchild]).status();
    }
    assert!(
        !survived,
        "the grandchild {grandchild} that ignores SIGTERM outlived the limit"
    );
    fs::remove_dir_all(&root).expect("remove scratch");
}

#[test]
fn with_timeout_refuses_bad_arguments_and_missing_commands() {
    for args in [
        &[][..],
        &["5"][..],
        &["0", "true"][..],
        &["soon", "true"][..],
        &["5", "/nonexistent/command"][..],
    ] {
        let (output, _) = with_timeout(args);
        assert_eq!(
            output.status.code(),
            Some(125),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
