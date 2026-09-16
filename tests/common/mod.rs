#![allow(dead_code)]
use post::output::{ErrorEnvelope, SendOutput};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct Sandbox {
    pub path: PathBuf,
    pub home: PathBuf,
    pub mail_root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let sandbox = Self::new_unseeded();
        // The suite's fixture universe: three registered rooms and one armed
        // rule. Seeded explicitly because the shipped first-run defaults are
        // deliberately empty (a public binary must not seed anyone's personal
        // room map — v0.2.3).
        fs::create_dir_all(&sandbox.mail_root).expect("create sandbox mail root");
        fs::write(
            sandbox.mail_root.join("rooms.json"),
            r#"{
  "claude-space": "~/claude-space",
  "pact": "~/pact",
  "agent-memory": "~/agent-memory"
}
"#,
        )
        .expect("seed sandbox rooms");
        fs::write(
            sandbox.mail_root.join("rules.json"),
            r#"{
  "blocked": [
    {
      "from": "*",
      "to": "agent-memory",
      "reason": "ARMED INSTRUMENT: no contact with the armed room until its closeout exists. Remove this rule only after the closeout is written and the affect check has fired."
    }
  ]
}
"#,
        )
        .expect("seed sandbox rules");
        #[cfg(unix)]
        for name in ["rooms.json", "rules.json"] {
            fs::set_permissions(
                sandbox.mail_root.join(name),
                fs::Permissions::from_mode(0o600),
            )
            .expect("restrict seeded config perms");
        }
        sandbox.seed_test_participant(None, Some("test-default"));
        sandbox
    }

    /// A sandbox whose mail root does not exist yet — for tests that assert
    /// first-run seeding or no-write-on-error behavior.
    pub fn new_unseeded() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should follow Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "post-cli-{}-{nanos}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let home = path.join("home");
        fs::create_dir_all(&home).expect("create sandbox home");
        let mail_root = path.join("mail");
        Self {
            path,
            home,
            mail_root,
        }
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.run_in(args, None, &self.path)
    }

    pub fn run_with_stdin(&self, args: &[&str], input: &str) -> Output {
        self.run_in(args, Some(input), &self.path)
    }

    /// Run a suggested `exact_fix` through a shell with `post` resolved to the
    /// binary under test. The point of the field is that it runs as written.
    pub fn run_fix(&self, fix: &str, cwd: &Path) -> Output {
        let script = fix.replacen("post ", &format!("'{}' ", env!("CARGO_BIN_EXE_post")), 1);
        Command::new("sh")
            .arg("-c")
            .arg(&script)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("POST_MAIL_ROOT", &self.mail_root)
            // Hermetic like run_in_env: a developer shell launched through
            // agent-session exports POST_FROM, which must never leak into a
            // fix executed under test.
            .env_remove("POST_FROM")
            .env_remove("POST_FRAMING")
            .env_remove("POST_SENDER_ADDRESS")
            .env_remove("POST_ARX_GENERATION")
            .env("POST_PARTICIPANT", self.test_participant_for_cwd(cwd))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .expect("run the suggested fix through a shell")
    }

    /// Run with stdout pointed at the null device: the shape that used to
    /// consume a channel's unread batch without ever showing it.
    pub fn run_in_discarding_stdout(&self, args: &[&str], cwd: &Path) -> Output {
        post_command()
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("POST_MAIL_ROOT", &self.mail_root)
            .env_remove("POST_FROM")
            .env_remove("POST_FRAMING")
            .env_remove("POST_SENDER_ADDRESS")
            .env_remove("POST_ARX_GENERATION")
            .env("POST_PARTICIPANT", self.test_participant_for_cwd(cwd))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .expect("run post with stdout discarded")
    }

    /// Run with fd1 inherited from a read-only regular file. Rust's standard
    /// stdout wrapper masks EBADF for this descriptor shape, so this fixture
    /// binds Post's strict output boundary rather than only an injected writer.
    #[cfg(unix)]
    pub fn run_in_broken_stdout(&self, args: &[&str], cwd: &Path) -> Output {
        let target = self.read_only_stdout_path();
        fs::write(&target, Self::READ_ONLY_STDOUT_SENTINEL).expect("seed stdout sentinel");
        let read_only = File::open(&target).expect("open stdout fixture read-only");
        post_command()
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("POST_MAIL_ROOT", &self.mail_root)
            .env_remove("POST_FROM")
            .env_remove("POST_FRAMING")
            .env_remove("POST_SENDER_ADDRESS")
            .env_remove("POST_ARX_GENERATION")
            .env("POST_PARTICIPANT", self.test_participant_for_cwd(cwd))
            .stdout(Stdio::from(read_only))
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .expect("run post with failing stdout")
    }

    #[cfg(unix)]
    pub const READ_ONLY_STDOUT_SENTINEL: &'static [u8] = b"stdout sentinel\n";

    #[cfg(unix)]
    pub fn read_only_stdout_path(&self) -> PathBuf {
        self.path.join("stdout-read-only.fixture")
    }

    pub fn run_in(&self, args: &[&str], input: Option<&str>, cwd: &Path) -> Output {
        self.run_in_env(args, input, cwd, &[])
    }

    /// Hermetic runner with explicit identity environment. The two identity
    /// variables are ALWAYS cleared first so a developer shell that exports
    /// POST_FROM can never leak into unrelated tests; `envs` re-adds exactly
    /// what a test declares.
    pub fn run_in_env(
        &self,
        args: &[&str],
        input: Option<&str>,
        cwd: &Path,
        envs: &[(&str, &str)],
    ) -> Output {
        let mut command = post_command();
        command
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("POST_MAIL_ROOT", &self.mail_root)
            .env_remove("POST_FROM")
            .env_remove("POST_FRAMING")
            .env_remove("POST_SENDER_ADDRESS")
            .env_remove("POST_ARX_GENERATION")
            .env_remove("POST_PARTICIPANT")
            .env_remove("POST_HARNESS")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_PID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SESSION_ID");
        let has_explicit_participant = envs.iter().any(|(key, _)| {
            matches!(
                *key,
                "POST_PARTICIPANT"
                    | "CLAUDE_CODE_SESSION_ID"
                    | "CODEX_THREAD_ID"
                    | "CODEX_SESSION_ID"
            )
        });
        let bootstrap_bind = args.starts_with(&["participant", "bind"])
            && (args.contains(&"--key") || args.contains(&"--new"));
        let launcher_bind = args.starts_with(&["participant", "bind"])
            && envs.iter().any(|(key, _)| *key == "POST_SENDER_ADDRESS");
        if !has_explicit_participant && !bootstrap_bind && !launcher_bind {
            if self.mail_root.join(".post-arx.json").exists() {
                command.env("POST_PARTICIPANT", "test-default");
            } else if self.mail_root.exists() && invocation_needs_test_participant(args) {
                let workspace = envs
                    .iter()
                    .find_map(|(key, value)| (*key == "POST_FROM").then_some(*value))
                    .map(str::to_owned)
                    .or_else(|| argument_value(args, "--from").map(str::to_owned))
                    .or_else(|| argument_value(args, "--room").map(str::to_owned))
                    .or_else(|| self.workspace_for_cwd(cwd));
                let id = self.seed_test_participant(workspace.as_deref(), None);
                command.env("POST_PARTICIPANT", id);
            } else {
                command.env("POST_PARTICIPANT", "test-default");
            }
        }
        for (key, value) in envs {
            command.env(key, value);
        }
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn().expect("spawn post binary");
        if let Some(input) = input {
            // A BrokenPipe here is the child exiting without reading stdin, which
            // is correct behaviour for several of these cases -- when a real
            // --body-file path wins, the binary never reads the '-' stream at all.
            // Panicking on it made the outcome depend on whether the parent
            // finished writing before the child finished exiting, so the suite
            // failed under load and passed in isolation. Any other error is still
            // a real failure.
            match child
                .stdin
                .as_mut()
                .expect("piped stdin should exist")
                .write_all(input.as_bytes())
            {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(e) => panic!("write command stdin: {e:?}"),
            }
        }
        child.wait_with_output().expect("wait for post binary")
    }

    pub fn run_unbound(&self, args: &[&str], cwd: &Path) -> Output {
        self.run_in_env(
            args,
            None,
            cwd,
            &[("POST_PARTICIPANT", "missing-test-participant")],
        )
    }

    /// Run with every participant/harness identity variable absent. Unlike
    /// `run_in`, this never seeds or exports a fixture participant.
    pub fn run_without_identity(&self, args: &[&str], cwd: &Path) -> Output {
        post_command()
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("POST_MAIL_ROOT", &self.mail_root)
            .env_remove("POST_FROM")
            .env_remove("POST_FRAMING")
            .env_remove("POST_SENDER_ADDRESS")
            .env_remove("POST_ARX_GENERATION")
            .env_remove("POST_PARTICIPANT")
            .env_remove("POST_HARNESS")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_PID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SESSION_ID")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .expect("run post without participant identity")
    }

    pub fn run_as_participant(&self, args: &[&str], participant: &str, cwd: &Path) -> Output {
        self.run_in_env(args, None, cwd, &[("POST_PARTICIPANT", participant)])
    }

    pub fn run_as_claude(&self, args: &[&str], conversation_key: &str, cwd: &Path) -> Output {
        self.run_in_env(
            args,
            None,
            cwd,
            &[("CLAUDE_CODE_SESSION_ID", conversation_key)],
        )
    }

    pub fn run_as_codex(&self, args: &[&str], conversation_key: &str, cwd: &Path) -> Output {
        self.run_in_env(args, None, cwd, &[("CODEX_THREAD_ID", conversation_key)])
    }

    pub fn bind_claude(
        &self,
        conversation_key: &str,
        cwd: &Path,
        workspace: Option<&str>,
    ) -> serde_json::Value {
        let mut args = vec!["participant", "bind"];
        if let Some(workspace) = workspace {
            args.extend(["--workspace", workspace]);
        }
        let output = self.run_as_claude(&args, conversation_key, cwd);
        assert_success(&output);
        from_stdout(&output)
    }

    pub fn bind_codex(
        &self,
        conversation_key: &str,
        cwd: &Path,
        workspace: Option<&str>,
    ) -> serde_json::Value {
        let mut args = vec!["participant", "bind"];
        if let Some(workspace) = workspace {
            args.extend(["--workspace", workspace]);
        }
        let output = self.run_as_codex(&args, conversation_key, cwd);
        assert_success(&output);
        from_stdout(&output)
    }

    pub fn read_participant(&self, id: &str) -> serde_json::Value {
        let path = self
            .mail_root
            .join("participants")
            .join(id)
            .join("participant.json");
        serde_json::from_slice(&fs::read(&path).expect("read participant.json"))
            .expect("parse participant.json")
    }

    pub fn test_participant(&self, workspace: &str) -> String {
        self.seed_test_participant(Some(workspace), None)
    }

    fn test_participant_for_cwd(&self, cwd: &Path) -> String {
        let workspace = self.workspace_for_cwd(cwd);
        self.seed_test_participant(workspace.as_deref(), None)
    }

    fn workspace_for_cwd(&self, cwd: &Path) -> Option<String> {
        let bytes = fs::read(self.mail_root.join("rooms.json")).ok()?;
        let rooms: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&bytes).ok()?;
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        rooms
            .into_iter()
            .filter_map(|(name, stored)| {
                let stored = stored
                    .strip_prefix("~/")
                    .map_or_else(|| PathBuf::from(&stored), |rest| self.home.join(rest));
                let stored = stored.canonicalize().unwrap_or(stored);
                cwd.starts_with(&stored)
                    .then_some((stored.components().count(), name))
            })
            .max()
            .map(|(_, name)| name)
            .or_else(|| cwd.file_name()?.to_str().map(str::to_owned))
    }

    fn seed_test_participant(&self, workspace: Option<&str>, fixed_id: Option<&str>) -> String {
        let key = format!("test:{}", workspace.unwrap_or("unbound-workspace"));
        let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
        let id = fixed_id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("test-{}", &digest[..8]));
        let dir = self.mail_root.join("participants").join(&id);
        fs::create_dir_all(&dir).expect("create test participant directory");
        let path = dir.join("participant.json");
        if fixed_id.is_none() && path.is_file() {
            return id;
        }
        let record = serde_json::json!({
            "version": 1,
            "id": id,
            "harness": "test",
            "conversation_key_digest": digest,
            "created": "2026-09-16 00:00:00 +0000",
            "workspace": workspace,
            "workspace_path": serde_json::Value::Null,
            "lineage": serde_json::Value::Null,
            "lineage_since": serde_json::Value::Null
        });
        fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&record).expect("serialize fixture")
            ),
        )
        .expect("write test participant record");
        id
    }

    pub fn send_json(&self, sender: &str, body: &str) -> SendOutput {
        let output = self.run(&[
            "send",
            "--to",
            "claude-space",
            "--from",
            sender,
            "--body",
            body,
            "--json",
        ]);
        assert_success(&output);
        from_stdout(&output)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !self.path.exists() {
            return;
        }
        // The sandbox is a uniquely named temp dir this test created; plain
        // stdlib removal is the portable cleanup, no external binary involved.
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "failed to remove test sandbox '{}': {error}",
                self.path.display()
            );
        }
    }
}
pub fn seed_fence_store(sandbox: &Sandbox, state: &str) {
    fs::create_dir_all(&sandbox.mail_root).expect("fence root");
    fs::create_dir_all(sandbox.home.join("dest")).expect("fence room path");
    fs::write(
        sandbox.mail_root.join("rooms.json"),
        r#"{"dest":"~/dest"}
"#,
    )
    .expect("fence rooms");
    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[]}
"#,
    )
    .expect("fence rules");
    fs::write(sandbox.mail_root.join(".post-arx.json"), state).expect("fence state");
    fs::write(sandbox.mail_root.join(".post-arx.lock"), b"").expect("fence lock");
    sandbox.seed_test_participant(Some("dest"), Some("test-default"));
}

pub fn seed_channel_fixture(sandbox: &Sandbox) {
    let channel = sandbox.mail_root.join("channels/tax");
    fs::create_dir_all(channel.join("messages")).expect("channel messages");
    fs::write(
        channel.join("channel.json"),
        r#"{"name":"tax","created":"2026-08-20 12:00:00 -0500","created_by":"dest"}"#,
    )
    .expect("channel info");
    fs::write(
        channel.join("members.json"),
        r#"{"dest":"2026-08-20 12:00:00 -0500"}"#,
    )
    .expect("channel members");
    fs::write(
        channel.join("messages/20260820-120000-000001-aaaaaa.msg"),
        "{\"id\":\"20260820-120000-000001-aaaaaa\",\"from\":\"other\",\"channel\":\"tax\",\"subject\":\"\",\"sent\":\"2026-08-20 12:00:00 -0500\"}\n---\nfixture\n",
    )
    .expect("channel message");
}

#[cfg(unix)]
pub fn fence_under_external_lock(sandbox: &Sandbox, generation: u64) -> std::time::SystemTime {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(sandbox.mail_root.join(".post-arx.lock"))
        .expect("open migration lock");
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    let heartbeat = sandbox.mail_root.join("dest/watch.heartbeat");
    let before = fs::metadata(&heartbeat)
        .expect("heartbeat exists under transition lock")
        .modified()
        .expect("heartbeat mtime");
    write_fence_state_locked(&sandbox.mail_root, generation);
    drop(lock);
    before
}

#[cfg(unix)]
pub fn write_fence_state_locked(root: &Path, generation: u64) {
    let temporary = root.join(format!(
        "..post-arx.json.{}.tmp",
        TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut state = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .expect("create fence temp");
    writeln!(
        state,
        "{{\"state\":\"fenced\",\"generation\":{generation}}}"
    )
    .expect("write fence temp");
    state.sync_all().expect("sync fence temp");
    fs::rename(&temporary, root.join(".post-arx.json")).expect("commit fence");
    File::open(root)
        .expect("open mailbox root")
        .sync_all()
        .expect("sync mailbox root");
}

pub fn write_reference_mail(inbox: &Path, id: &str, body: &str) {
    let envelope = serde_json::json!({
        "id": id,
        "from": "fixture",
        "to": "claude-space",
        "kind": "note",
        "subject": "",
        "sent": "2026-07-15 12:00:00 -0400"
    });
    write_custom_mail(inbox, id, &envelope, body);
}

pub fn write_custom_mail(
    inbox: &Path,
    filename_id: &str,
    envelope: &impl serde::Serialize,
    body: &str,
) {
    fs::create_dir_all(inbox).expect("create custom mail fixture inbox");
    fs::write(
        inbox.join(format!("{filename_id}.mail")),
        format!(
            "{}\n---\n{body}",
            serde_json::to_string_pretty(envelope).expect("serialize custom envelope")
        ),
    )
    .expect("write custom mail fixture");
}

pub fn register_alpha_beta(sandbox: &Sandbox) -> (PathBuf, PathBuf) {
    assert_success(&sandbox.run(&["rooms"]));
    create_default_room_paths(sandbox);
    let alpha = sandbox.path.join("alpha");
    let beta = sandbox.path.join("beta");
    fs::create_dir(&alpha).expect("create alpha room path");
    fs::create_dir(&beta).expect("create beta room path");
    register_room(sandbox, "alpha", &alpha);
    register_room(sandbox, "beta", &beta);
    (alpha, beta)
}

pub fn create_default_room_paths(sandbox: &Sandbox) {
    for relative in ["agent-memory", "claude-space", "pact"] {
        fs::create_dir_all(sandbox.home.join(relative)).expect("create default room path");
    }
}

pub fn register_room(sandbox: &Sandbox, name: &str, path: &Path) {
    let output = sandbox.run(&["rooms", "add", name, path.to_string_lossy().as_ref()]);
    assert_success(&output);
    sandbox.seed_test_participant(Some(name), None);
}

pub fn join_channel(sandbox: &Sandbox, channel: &str, cwd: &Path) {
    let output = sandbox.run_in(&["chat", channel, "--join", "--json"], None, cwd);
    assert_success(&output);
}

pub fn write_channel_message(
    sandbox: &Sandbox,
    channel: &str,
    id: &str,
    from: &str,
    subject: &str,
    body: &str,
) {
    let message = serde_json::json!({
        "id": id,
        "from": from,
        "channel": channel,
        "subject": subject,
        "sent": "2026-07-22 01:01:01 -0500"
    });
    fs::write(
        sandbox
            .mail_root
            .join("channels")
            .join(channel)
            .join("messages")
            .join(format!("{id}.msg")),
        format!(
            "{}\n---\n{body}",
            serde_json::to_string_pretty(&message).expect("serialize channel message")
        ),
    )
    .expect("write channel message fixture");
}

pub fn write_bad_channel(
    sandbox: &Sandbox,
    name: &str,
    members: Option<&str>,
    messages_dir: bool,
    channel_json: &str,
) {
    let dir = sandbox.mail_root.join("channels").join(name);
    fs::create_dir_all(&dir).expect("create bad channel dir");
    fs::write(dir.join("channel.json"), channel_json).expect("write bad channel info");
    if let Some(members) = members {
        fs::write(dir.join("members.json"), members).expect("write bad channel members");
    }
    if messages_dir {
        fs::create_dir_all(dir.join("messages")).expect("create bad channel messages dir");
    }
}

/// Every direct spawn of the binary under test routes through here: the two
/// identity variables are cleared up front, so a developer shell launched
/// through agent-session (which pins POST_FROM) can never leak into a test.
/// Tests that need a pin re-add it explicitly via run_in_env/env().
pub fn post_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_post"));
    command
        .env_remove("POST_FROM")
        .env_remove("POST_SENDER_ADDRESS")
        .env_remove("POST_ARX_GENERATION")
        .env_remove("POST_HARNESS")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_PID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CODEX_SESSION_ID")
        .env("POST_PARTICIPANT", "test-default");
    command
}

pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        stdout(output),
        stderr(output)
    );
    let rendered = stderr(output);
    let unexpected: Vec<_> = rendered
        .lines()
        .filter(|line| !line.trim().is_empty() && !is_identity_notice(line))
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected stderr: {}",
        unexpected.join("\n")
    );
}

pub fn assert_migration_refused(output: &Output) {
    assert_eq!(output.status.code(), Some(78), "stderr: {}", stderr(output));
    let error: ErrorEnvelope = from_stderr(output);
    assert_eq!(error.error.code, "config_invalid");
    assert!(
        error
            .error
            .message
            .to_ascii_lowercase()
            .contains("migration fence"),
        "missing migration-fence refusal: {}",
        error.error.message
    );
}

/// The cwd-identity notice is a deliberate receipt, not noise: mutating and
/// consuming commands name the room they resolved to before they act. Every
/// other line on a successful run is still a test failure.
pub fn is_identity_notice(line: &str) -> bool {
    line.contains("(identity inferred from cwd)")
        || line.contains("(POST_FROM pin")
        || line.contains("(bound participant ")
        || line.contains("(participant binding)")
}

fn argument_value<'a>(args: &'a [&str], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find_map(|pair| (pair[0] == flag).then_some(pair[1]))
}

fn invocation_needs_test_participant(args: &[&str]) -> bool {
    match args.first().copied() {
        Some("send" | "catchup") => true,
        Some("read") => {
            !args.contains(&"--peek") && !args.contains(&"--offset") && !args.contains(&"--length")
        }
        Some("chat" | "watch" | "channels" | "search") => true,
        Some("profile") => args
            .get(1)
            .is_some_and(|value| matches!(*value, "set" | "clear")),
        Some("inbox") => args.contains(&"--adopt"),
        Some("identity") => !args
            .get(1)
            .is_some_and(|value| matches!(*value, "list" | "show")),
        _ => false,
    }
}

pub fn from_stdout<T: serde::de::DeserializeOwned>(output: &Output) -> T {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not expected JSON: {error}\nstdout: {}\nstderr: {}",
            stdout(output),
            stderr(output)
        )
    })
}

pub fn from_stderr<T: serde::de::DeserializeOwned>(output: &Output) -> T {
    let raw = stderr(output);
    let json_line = raw
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or(raw.trim());
    serde_json::from_str(json_line).unwrap_or_else(|error| {
        panic!(
            "stderr was not expected JSON: {error}\nstdout: {}\nstderr: {}",
            stdout(output),
            raw
        )
    })
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
