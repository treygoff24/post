//! scripts/install-post.sh and scripts/install-smoke.sh, run for real against
//! temporary directories. The installer runs unmodified; what it builds and
//! replaces is faked: a throwaway git repo holds the commit (with a stub
//! smoke), a fake `cargo` on PATH writes a scripted `post` stamped with that
//! commit's short sha, and the live `post` in the temporary bin dir is a
//! script reporting build c808283, as the live hosts do. HOME points into the
//! sandbox, so no default path can reach a real install.

mod common;

use common::Sandbox;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const INSTALLER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install-post.sh");
const SMOKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install-smoke.sh");

/// The checks install-smoke.sh reports, in its order.
const SMOKE_CHECKS: [&str; 6] = [
    "setup",
    "version",
    "build_id",
    "samples",
    "who_speed",
    "porch",
];

/// The wide store the real smoke seeds. Fifty still covers all three workspace
/// variants and the `who` count check; the 2,000-participant contract is
/// owned by tests/scaling.rs.
const SMOKE_WHO_PARTICIPANTS: &str = "50";

/// The stub smoke committed into the throwaway repo. FAKE_SMOKE picks what
/// it reports through --results; FAKE_SMOKE_ARGS, when set, names a file that
/// receives the arguments the installer called it with.
const STUB_SMOKE: &str = r#"#!/usr/bin/env bash
[ -z "${FAKE_SMOKE_ARGS:-}" ] || printf '%s\n' "$*" > "$FAKE_SMOKE_ARGS"
[ "$1" = --results ] || { echo "stub smoke: expected --results" >&2; exit 2; }
results="$2"
line() { printf '{"check": "%s", "result": "pass", "detail": ""}\n' "$1"; }
# The six checks the real smoke reports, each once, in its order.
all() {
  for id in setup version build_id samples who_speed porch; do line "$id"; done
}
case "${FAKE_SMOKE:-pass}" in
  pass) all > "$results" ;;
  allowed-skip)
    all | sed '$d' > "$results"
    printf '%s\n' '{"check": "porch", "result": "skipped", "detail": "no porch3", "allowed": true}' >> "$results" ;;
  drop-samples) all | grep -v samples > "$results" ;;
  drop-build-id) all | grep -v build_id > "$results" ;;
  drop-who-speed) all | grep -v who_speed > "$results" ;;
  unknown-id) { all; line bogus; } > "$results" ;;
  duplicate) { all; line setup; } > "$results" ;;
  skip-version)
    all | grep -v '"version"' > "$results"
    printf '%s\n' '{"check": "version", "result": "skipped", "detail": "x", "allowed": true}' >> "$results" ;;
  silent) : > "$results" ;;
  lying)
    all | sed 's/"porch", "result": "pass"/"porch", "result": "fail"/' > "$results" ;;
  fail) exit 1 ;;
esac
"#;

/// The fake cargo: `build --release --locked` writes the scripted post,
/// stamped with the checked-out commit's short sha as build.rs would, and
/// `metadata` names the target directory the build used. FAKE_CONFIG_TARGET_DIR
/// plays a `build.target-dir` set in a cargo config file, which (unlike
/// CARGO_TARGET_DIR) the installer cannot see in its environment.
const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
target="${CARGO_TARGET_DIR:-${FAKE_CONFIG_TARGET_DIR:-$PWD/target}}"
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
if [ "$*" = "metadata --no-deps --format-version 1 --locked" ]; then
  printf '{"target_directory":"%s"}\n' "$target"
  exit 0
fi
[ "$*" = "build --release --locked" ] || { echo "fake cargo: unexpected: $*" >&2; exit 64; }
sha=$(git rev-parse --short HEAD)
out="$target/release"
mkdir -p "$out"
sed "s/@SHA@/$sha/g" "$FAKE_POST_TEMPLATE" > "$out/post"
chmod +x "$out/post"
"#;

/// The built binary. FAKE_VERIFY picks the served-skill verdict and
/// FAKE_SELF_VERIFY the verdict against the binary's own source tree.
const FAKE_POST: &str = r#"#!/usr/bin/env bash
# fake built post @SHA@
case "$1 ${2:-}" in
  "version --json") printf '{"ok":true,"build_sha":"@SHA@","capabilities":["participants"]}\n' ;;
  "contract skill-manifest")
    case "${4:-}" in
      */src/skills/post) verdict="${FAKE_SELF_VERIFY:-match}" ;;
      *) verdict="${FAKE_VERIFY:-match}" ;;
    esac
    case "$verdict" in
      match) printf '{"ok":true,"verdict":"match","kind":"symlink","mismatched":[],"missing":[],"extra":[],"rendered_unverified":[]}\n' ;;
      drift) printf '{"ok":false,"verdict":"drift","kind":"symlink","mismatched":["SKILL.md"],"missing":[],"extra":[],"rendered_unverified":[]}\n'; exit 1 ;;
      unverified) printf '{"ok":false,"verdict":"unverified","kind":"copy","mismatched":[],"missing":[],"extra":[],"rendered_unverified":["SKILL.md"]}\n'; exit 1 ;;
      error) echo "post: inspect served skill path: No such file or directory" >&2; exit 2 ;;
    esac ;;
  *) exit 0 ;;
esac
"#;

/// A fake `install` that stages the file and then corrupts it, as a bad disk
/// or a racing writer would: the installer must notice after its `mv`.
const CORRUPTING_INSTALL: &str = r#"#!/usr/bin/env bash
/usr/bin/install "$@" && printf 'corrupt' >> "${@: -1}"
"#;

fn live_post(tag: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n# live post, {tag}\nprintf '{{\"ok\":true,\"build_sha\":\"c808283\"}}\\n'\n"
    )
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod script");
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=install-test",
            "-c",
            "user.email=install-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

struct Rig {
    sandbox: Sandbox,
    repo: PathBuf,
    /// The bare repository `origin` points at.
    origin: PathBuf,
    fakes: PathBuf,
    bin_dir: PathBuf,
    receipt: PathBuf,
    short_sha: String,
}

impl Rig {
    /// The repo's only commit is on origin's `main`, as an installable commit
    /// is on the live hosts; `unpushed_commit` and `push_to` change that.
    fn new() -> Self {
        let sandbox = Sandbox::new_unseeded();
        let repo = sandbox.path.join("repo");
        let origin = sandbox.path.join("origin.git");
        fs::create_dir_all(repo.join("scripts")).expect("repo dirs");
        write_exec(&repo.join("scripts/install-smoke.sh"), STUB_SMOKE);
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "scripts/install-smoke.sh"]);
        git(&repo, &["commit", "-q", "-m", "stub"]);
        git(&sandbox.path, &["init", "-q", "--bare", "origin.git"]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let short_sha = git(&repo, &["rev-parse", "--short", "HEAD"]);
        let fakes = sandbox.path.join("fakes");
        fs::create_dir_all(&fakes).expect("fakes dir");
        write_exec(&fakes.join("cargo"), FAKE_CARGO);
        fs::write(fakes.join("post.template"), FAKE_POST).expect("post template");
        let bin_dir = sandbox.path.join("bin");
        fs::create_dir_all(&bin_dir).expect("bin dir");
        let receipt = sandbox.path.join("share/install-receipt.json");
        Self {
            sandbox,
            repo,
            origin,
            fakes,
            bin_dir,
            receipt,
            short_sha,
        }
    }

    /// A new HEAD that no branch on origin contains.
    fn unpushed_commit(&mut self) {
        fs::write(self.repo.join("change.txt"), "a change nobody pushed\n").expect("change");
        git(&self.repo, &["add", "change.txt"]);
        git(&self.repo, &["commit", "-q", "-m", "unpushed"]);
        self.short_sha = git(&self.repo, &["rev-parse", "--short", "HEAD"]);
    }

    /// Push HEAD to a branch of origin. `by_url` pushes to the repository's
    /// path instead of the `origin` remote, so this clone's remote-tracking
    /// refs stay as they were: what a push from another machine looks like
    /// until the installer fetches.
    fn push_to(&self, branch: &str, by_url: bool) {
        let destination = if by_url {
            self.origin.display().to_string()
        } else {
            "origin".to_owned()
        };
        git(
            &self.repo,
            &[
                "push",
                "-q",
                &destination,
                &format!("HEAD:refs/heads/{branch}"),
            ],
        );
    }

    fn target(&self) -> PathBuf {
        self.bin_dir.join("post")
    }

    /// Put a live post in the bin dir; returns its bytes.
    fn live(&self, tag: &str) -> Vec<u8> {
        write_exec(&self.target(), &live_post(tag));
        fs::read(self.target()).expect("live post")
    }

    fn built_bytes(&self) -> Vec<u8> {
        FAKE_POST.replace("@SHA@", &self.short_sha).into_bytes()
    }

    fn command(&self, extra_path: Option<&Path>, env: &[(&str, &str)]) -> Command {
        let mut path = self.fakes.display().to_string();
        if let Some(extra) = extra_path {
            path = format!("{}:{path}", extra.display());
        }
        path = format!("{path}:{}", std::env::var("PATH").unwrap_or_default());
        let mut command = Command::new("bash");
        command
            .arg(INSTALLER)
            .env("PATH", path)
            .env("HOME", &self.sandbox.home)
            .env("FAKE_POST_TEMPLATE", self.fakes.join("post.template"))
            // A developer shell's BASH_ENV can re-order PATH in every
            // non-interactive bash, which would hide the fakes.
            .env_remove("BASH_ENV")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("POST_SMOKE_ALLOW_SKIP")
            .stdin(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    fn run_with(&self, extra_path: Option<&Path>, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = self.command(extra_path, env);
        command
            .arg("--bin-dir")
            .arg(&self.bin_dir)
            .arg("--served")
            .arg(self.sandbox.path.join("served"))
            .arg("--receipt")
            .arg(&self.receipt)
            .arg("--repo")
            .arg(&self.repo)
            .args(args)
            .arg("HEAD");
        command.output().expect("run install-post.sh")
    }

    fn run(&self, env: &[(&str, &str)]) -> Output {
        self.run_with(None, env, &[])
    }

    fn receipt(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(&self.receipt).expect("receipt")).expect("receipt json")
    }

    fn bin_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.bin_dir)
            .expect("bin dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stderr:\n{}",
        stderr(output)
    );
}

/// Installed, with the receipt's backup holding exactly the pre-install post.
fn assert_backed_up_and_installed(rig: &Rig, live: &[u8]) -> PathBuf {
    assert_eq!(fs::read(rig.target()).expect("post"), rig.built_bytes());
    let receipt = rig.receipt();
    let backup = PathBuf::from(receipt["backup"].as_str().expect("receipt names a backup"));
    assert_eq!(backup.parent(), Some(rig.bin_dir.as_path()));
    assert_eq!(
        fs::read(&backup).expect("backup"),
        live,
        "the backup holds the pre-install post"
    );
    assert_eq!(receipt["backup_sha256"], sha256_hex(live));
    assert_eq!(receipt["binary_sha256"], sha256_hex(&rig.built_bytes()));
    backup
}

// ---- 1. backup integrity ----

#[test]
fn a_plain_install_backs_up_the_live_post_by_build_sha() {
    let rig = Rig::new();
    let live = rig.live("plain");
    let output = rig.run(&[]);
    assert_code(&output, 0);
    let backup = assert_backed_up_and_installed(&rig, &live);
    assert_eq!(backup, rig.bin_dir.join("post-c808283.bak"));
    assert_eq!(
        rig.bin_entries(),
        ["post", "post-c808283.bak"],
        "no temporary left"
    );

    // A rerun keeps the verified backup: it already holds the live bytes of
    // the first run's post, which is now the built one, so a new name is used.
    let before = fs::read(&backup).expect("backup");
    let output = rig.run(&[]);
    assert_code(&output, 0);
    assert_eq!(fs::read(&backup).expect("backup"), before);
}

/// A host whose cargo config sets `build.target-dir` (the devbox does) builds
/// into a shared directory the installer's environment does not mention.
#[test]
fn a_target_dir_set_in_cargo_config_is_found() {
    let rig = Rig::new();
    let live = rig.live("config-target-dir");
    let shared = rig.sandbox.path.join("shared-cargo-targets");
    let output = rig.run(&[(
        "FAKE_CONFIG_TARGET_DIR",
        shared.to_str().expect("utf-8 path"),
    )]);
    assert_code(&output, 0);
    assert!(
        shared.join("release/post").is_file(),
        "the build went to the configured directory"
    );
    assert_backed_up_and_installed(&rig, &live);
}

/// A binary whose built-in skill manifest disagrees with the tree it was
/// built from is stale build-script output, and never installs.
#[test]
fn a_binary_whose_manifest_disagrees_with_its_own_source_is_refused() {
    for self_verify in ["drift", "error"] {
        let rig = Rig::new();
        let live = rig.live("stale-manifest");
        let before = rig.bin_entries();
        let output = rig.run(&[("FAKE_SELF_VERIFY", self_verify)]);
        assert_code(&output, 3);
        assert!(
            stderr(&output).contains("does not match its own source"),
            "{self_verify}: {}",
            stderr(&output)
        );
        assert_eq!(rig.bin_entries(), before, "{self_verify}: nothing written");
        assert_eq!(fs::read(rig.target()).expect("post"), live, "{self_verify}");
        assert!(!rig.receipt.exists(), "{self_verify}: no receipt");
    }
}

#[test]
fn a_backup_of_other_bytes_with_the_same_build_sha_is_not_trusted() {
    // Each row plants a file under the build-sha backup name that is not a
    // copy of the live post: a first run that died mid-copy, a different
    // build reporting the same sha, and same-length bytes that only a hash
    // (not a size check) can tell apart.
    let live_bytes = live_post("the live build").into_bytes();
    let mut same_length = live_bytes.clone();
    *same_length.last_mut().expect("bytes") ^= 1;
    let rows: [(&str, Vec<u8>); 3] = [
        ("truncated", live_bytes[..live_bytes.len() / 2].to_vec()),
        (
            "different build",
            live_post("a different build reporting the same sha").into_bytes(),
        ),
        ("same length", same_length),
    ];
    for (row, other) in rows {
        let rig = Rig::new();
        let live = rig.live("the live build");
        assert_eq!(live, live_bytes, "{row}: the live fixture is deterministic");
        let stale = rig.bin_dir.join("post-c808283.bak");
        fs::write(&stale, &other).expect("planted backup");
        let output = rig.run(&[]);
        assert_code(&output, 0);
        let backup = assert_backed_up_and_installed(&rig, &live);
        assert_eq!(
            backup,
            rig.bin_dir
                .join(format!("post-c808283-{}.bak", &sha256_hex(&live)[..12])),
            "{row}"
        );
        assert_eq!(fs::read(&stale).expect("stale"), other, "{row}: left alone");
        assert!(
            stderr(&output).contains("not a copy of the live post"),
            "{row}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn a_backup_that_already_holds_the_live_bytes_is_kept() {
    let rig = Rig::new();
    let live = rig.live("kept");
    let existing = rig.bin_dir.join("post-c808283.bak");
    fs::write(&existing, &live).expect("matching backup");
    let output = rig.run(&[]);
    assert_code(&output, 0);
    assert_eq!(assert_backed_up_and_installed(&rig, &live), existing);
    assert!(
        stderr(&output).contains("keeping it"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn when_both_backup_names_hold_other_bytes_post_is_untouched() {
    let rig = Rig::new();
    let live = rig.live("blocked");
    fs::write(rig.bin_dir.join("post-c808283.bak"), b"one").expect("first");
    fs::write(
        rig.bin_dir
            .join(format!("post-c808283-{}.bak", &sha256_hex(&live)[..12])),
        b"two",
    )
    .expect("second");
    let before = rig.bin_entries();
    let output = rig.run(&[]);
    assert_code(&output, 3);
    assert_eq!(fs::read(rig.target()).expect("post"), live);
    assert_eq!(rig.bin_entries(), before);
    assert!(!rig.receipt.exists());
}

#[test]
fn a_first_install_with_no_live_post_has_no_backup() {
    let rig = Rig::new();
    let output = rig.run(&[]);
    assert_code(&output, 0);
    assert_eq!(fs::read(rig.target()).expect("post"), rig.built_bytes());
    assert_eq!(rig.receipt()["backup"], serde_json::Value::Null);
    assert_eq!(rig.receipt()["backup_sha256"], serde_json::Value::Null);
}

#[test]
fn a_dry_run_names_the_backup_it_would_write_and_changes_nothing() {
    let rig = Rig::new();
    let live = rig.live("dry");
    fs::write(rig.bin_dir.join("post-c808283.bak"), b"short").expect("bad backup");
    let before = rig.bin_entries();
    let output = rig.run_with(None, &[], &["--dry-run"]);
    assert_code(&output, 0);
    let expected = format!("post-c808283-{}.bak", &sha256_hex(&live)[..12]);
    assert!(stderr(&output).contains(&expected), "{}", stderr(&output));
    assert_eq!(rig.bin_entries(), before);
    assert_eq!(fs::read(rig.target()).expect("post"), live);
    assert!(!rig.receipt.exists());
}

// ---- 2. a skipped smoke check is not a pass ----

#[test]
fn an_allowed_smoke_skip_is_recorded_as_pass_with_skips() {
    let rig = Rig::new();
    rig.live("skip");
    let output = rig.run(&[("FAKE_SMOKE", "allowed-skip")]);
    assert_code(&output, 0);
    let receipt = rig.receipt();
    assert_eq!(receipt["smoke_verdict"], "pass_with_skips");
    let porch = SMOKE_CHECKS.len() - 1;
    assert_eq!(receipt["smoke_checks"][porch]["check"], "porch");
    assert_eq!(receipt["smoke_checks"][porch]["result"], "skipped");
}

#[test]
fn a_full_smoke_pass_is_recorded_per_check() {
    let rig = Rig::new();
    rig.live("pass");
    assert_code(&rig.run(&[]), 0);
    let receipt = rig.receipt();
    assert_eq!(receipt["smoke_verdict"], "pass");
    let checks: Vec<&str> = receipt["smoke_checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["check"].as_str().expect("check id"))
        .collect();
    assert_eq!(checks, SMOKE_CHECKS);
}

/// The gate knows the six checks: a smoke that exits 0 while one is missing,
/// repeated, unknown, or skipped (other than porch) installs nothing.
#[test]
fn a_smoke_missing_a_check_or_reporting_an_unknown_one_installs_nothing() {
    for (mode, why) in [
        ("drop-samples", "samples"),
        ("drop-build-id", "build_id"),
        ("drop-who-speed", "who_speed"),
        ("unknown-id", "bogus"),
        ("duplicate", "setup"),
        ("skip-version", "version"),
    ] {
        let rig = Rig::new();
        let live = rig.live(mode);
        let output = rig.run(&[("FAKE_SMOKE", mode)]);
        assert_code(&output, 3);
        assert!(stderr(&output).contains(why), "{mode}: {}", stderr(&output));
        assert_eq!(fs::read(rig.target()).expect("post"), live, "{mode}");
        assert_eq!(rig.bin_entries(), ["post"], "{mode}");
        assert!(!rig.receipt.exists(), "{mode}");
    }
}

#[test]
fn a_smoke_exiting_zero_without_an_itemized_pass_installs_nothing() {
    for (mode, reason) in [
        ("silent", "the smoke reported no checks"),
        ("lying", "'porch' reported 'fail' but the smoke exited 0"),
    ] {
        let rig = Rig::new();
        let live = rig.live(mode);
        let output = rig.run(&[("FAKE_SMOKE", mode)]);
        assert_code(&output, 3);
        assert!(
            stderr(&output).contains(reason),
            "{mode}: {}",
            stderr(&output)
        );
        assert_eq!(fs::read(rig.target()).expect("post"), live, "{mode}");
        assert_eq!(rig.bin_entries(), ["post"], "{mode}");
        assert!(!rig.receipt.exists(), "{mode}");
    }
}

/// The real smoke against the real binary under test, with no Porch: the
/// skip fails the smoke unless allowed, and every other check still runs.
#[test]
fn the_real_smoke_fails_a_porch_skip_unless_the_operator_allows_it() {
    let sandbox = Sandbox::new_unseeded();
    let results = sandbox.path.join("results.jsonl");
    let run = |allow: Option<&str>| {
        let mut command = Command::new("bash");
        command
            .arg(SMOKE)
            .arg("--results")
            .arg(&results)
            .arg(env!("CARGO_BIN_EXE_post"))
            .env("PORCH_PYTHON", sandbox.path.join("no-porch/python"))
            // Not a speed test: a loaded machine must not fail it.
            .env("POST_SMOKE_WHO_SECONDS", "10")
            .env_remove("BASH_ENV")
            .env("POST_SMOKE_WHO_PARTICIPANTS", SMOKE_WHO_PARTICIPANTS)
            .env_remove("POST_SMOKE_ALLOW_SKIP")
            .stdin(Stdio::null());
        if let Some(allow) = allow {
            command.env("POST_SMOKE_ALLOW_SKIP", allow);
        }
        let output = command.output().expect("run install-smoke.sh");
        let checks: Vec<serde_json::Value> = fs::read_to_string(&results)
            .expect("results")
            .lines()
            .map(|line| serde_json::from_str(line).expect("result line"))
            .collect();
        (output, checks)
    };

    let (output, checks) = run(None);
    assert_code(&output, 1);
    let porch = checks.last().expect("a porch record");
    assert_eq!(porch["check"], "porch");
    assert_eq!(porch["result"], "skipped");
    assert_eq!(porch["allowed"], false);
    assert!(stderr(&output).contains("POST_SMOKE_ALLOW_SKIP=porch"));

    let (output, checks) = run(Some("porch"));
    assert_code(&output, 0);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("install-smoke: PASS_WITH_SKIPS"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let ids: Vec<&str> = checks
        .iter()
        .map(|check| check["check"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, SMOKE_CHECKS);
    let porch = SMOKE_CHECKS.len() - 1;
    for check in &checks[..porch] {
        assert_eq!(check["result"], "pass", "{check}");
    }
    assert_eq!(checks[porch]["result"], "skipped");
    assert_eq!(checks[porch]["allowed"], true);
}

/// Run the real smoke on `bin` with Porch allowed to skip; returns the output
/// and the itemized results. `who_seconds` is the `post who` limit: the
/// default (`None`) is the two-second contract, and tests that are not about
/// speed pass a generous one so a loaded machine cannot fail them.
fn run_real_smoke(
    sandbox: &Sandbox,
    bin: &Path,
    extra_args: &[&str],
    who_seconds: Option<&str>,
) -> (Output, Vec<serde_json::Value>) {
    let results = sandbox.path.join("results.jsonl");
    let mut command = Command::new("bash");
    command
        .arg(SMOKE)
        .arg("--results")
        .arg(&results)
        .args(extra_args)
        .arg(bin)
        .env("PORCH_PYTHON", sandbox.path.join("no-porch/python"))
        .env("POST_SMOKE_ALLOW_SKIP", "porch")
        .env_remove("BASH_ENV")
        .env_remove("POST_SMOKE_WHO_SECONDS")
        .env("POST_SMOKE_WHO_PARTICIPANTS", SMOKE_WHO_PARTICIPANTS)
        .stdin(Stdio::null());
    if let Some(seconds) = who_seconds {
        command.env("POST_SMOKE_WHO_SECONDS", seconds);
    }
    let output = command.output().expect("run install-smoke.sh");
    let checks = fs::read_to_string(&results)
        .expect("results")
        .lines()
        .map(|line| serde_json::from_str(line).expect("result line"))
        .collect();
    (output, checks)
}

fn check<'a>(checks: &'a [serde_json::Value], id: &str) -> &'a serde_json::Value {
    checks
        .iter()
        .find(|entry| entry["check"] == id)
        .unwrap_or_else(|| panic!("no {id} record in {checks:?}"))
}

/// A stand-in for the binary under test that runs the real one, except that
/// `who` first sleeps `who_delay` seconds and, when `bare_version` is set,
/// `--version` prints clap's bare line as it did before it named the build.
fn wrapped_post(sandbox: &Sandbox, who_delay: &str, bare_version: bool) -> PathBuf {
    let path = sandbox.path.join("wrapped/post");
    fs::create_dir_all(path.parent().expect("parent")).expect("wrapper dir");
    let bare = if bare_version {
        "[ \"$1\" = --version ] && { echo 'post 0.9.0'; exit 0; }\n"
    } else {
        ""
    };
    write_exec(
        &path,
        &format!(
            "#!/bin/sh\n{bare}[ \"$1\" = who ] && sleep {who_delay}\nexec {} \"$@\"\n",
            env!("CARGO_BIN_EXE_post")
        ),
    );
    path
}

/// The 2-second `post who` contract, with teeth: at the default limit a `who`
/// that takes 2.5 s fails the smoke, and the same binary passes when the
/// operator's limit is longer. A smoke that never timed `who` would pass both.
#[test]
fn the_real_smoke_fails_a_who_slower_than_two_seconds() {
    let sandbox = Sandbox::new_unseeded();
    let slow = wrapped_post(&sandbox, "2.5", false);

    let (output, checks) = run_real_smoke(&sandbox, &slow, &[], None);
    assert_code(&output, 1);
    let who = check(&checks, "who_speed");
    assert_eq!(who["result"], "fail", "{who}");
    assert!(
        who["detail"]
            .as_str()
            .expect("detail")
            .contains("the limit is 2 s"),
        "{who}"
    );
    for id in ["setup", "version", "build_id", "samples"] {
        assert_eq!(check(&checks, id)["result"], "pass", "{id}");
    }

    let (output, checks) = run_real_smoke(&sandbox, &slow, &[], Some("10"));
    assert_code(&output, 0);
    assert_eq!(check(&checks, "who_speed")["result"], "pass");
}

/// The build id must be the commit being installed and must read the same
/// from `post --version` and `post version`.
#[test]
fn the_real_smoke_fails_a_build_id_that_is_not_the_expected_commit() {
    let sandbox = Sandbox::new_unseeded();
    let real = PathBuf::from(env!("CARGO_BIN_EXE_post"));
    let reported = Command::new(&real)
        .args(["version", "--json"])
        .output()
        .expect("post version --json");
    let built: serde_json::Value = serde_json::from_slice(&reported.stdout).expect("version json");
    let build = built["build_sha"].as_str().expect("build_sha");

    let (output, checks) = run_real_smoke(&sandbox, &real, &["--expect-build", build], Some("10"));
    assert_code(&output, 0);
    assert_eq!(check(&checks, "build_id")["result"], "pass");

    let (output, checks) =
        run_real_smoke(&sandbox, &real, &["--expect-build", "0000000"], Some("10"));
    assert_code(&output, 1);
    let entry = check(&checks, "build_id");
    assert_eq!(entry["result"], "fail", "{entry}");
    let detail = entry["detail"].as_str().expect("detail");
    assert!(
        detail.contains("expected 0000000") && detail.contains(build),
        "the failure names both builds: {detail}"
    );
}

#[test]
fn the_real_smoke_fails_when_version_flag_and_version_command_disagree() {
    let sandbox = Sandbox::new_unseeded();
    let bare = wrapped_post(&sandbox, "0", true);
    let (output, checks) = run_real_smoke(&sandbox, &bare, &[], Some("10"));
    assert_code(&output, 1);
    let entry = check(&checks, "build_id");
    assert_eq!(entry["result"], "fail", "{entry}");
    assert!(
        entry["detail"]
            .as_str()
            .expect("detail")
            .contains("--version: post 0.9.0"),
        "{entry}"
    );
}

/// macOS gives every shell a TMPDIR ending in "/". The smoke must still hand
/// Porch a room directory with no doubled slash: Porch normalizes the path it
/// is given, post reports the one it stored, and a `//` made the owner
/// crosscheck disagree and fail the real install on 2026-09-23. The fake
/// Porch interpreter here fails on the same precondition.
#[test]
fn the_real_smoke_hands_porch_a_clean_room_dir_under_a_trailing_slash_tmpdir() {
    let sandbox = Sandbox::new_unseeded();
    let results = sandbox.path.join("results.jsonl");
    let tmp = sandbox.path.join("tmp");
    fs::create_dir_all(&tmp).expect("tmp dir");
    let fake_porch = sandbox.path.join("fake-porch/python");
    fs::create_dir_all(fake_porch.parent().expect("parent")).expect("fake porch dir");
    write_exec(
        &fake_porch,
        r#"#!/bin/sh
[ "$1" = -c ] && exit 0
cat >/dev/null
case "$3" in
  *//*) echo "doubled slash in the room dir: $3" >&2; exit 1 ;;
esac
exit 0
"#,
    );
    let output = Command::new("bash")
        .arg(SMOKE)
        .arg("--results")
        .arg(&results)
        .arg(env!("CARGO_BIN_EXE_post"))
        .env("TMPDIR", format!("{}/", tmp.display()))
        .env("PORCH_PYTHON", &fake_porch)
        .env("POST_SMOKE_WHO_SECONDS", "10")
        .env("POST_SMOKE_WHO_PARTICIPANTS", SMOKE_WHO_PARTICIPANTS)
        .env_remove("BASH_ENV")
        .env_remove("POST_SMOKE_ALLOW_SKIP")
        .stdin(Stdio::null())
        .output()
        .expect("run install-smoke.sh");
    let checks: Vec<serde_json::Value> = fs::read_to_string(&results)
        .expect("results")
        .lines()
        .map(|line| serde_json::from_str(line).expect("result line"))
        .collect();
    let porch = checks.last().expect("a porch record");
    assert_eq!(porch["check"], "porch");
    assert_eq!(porch["result"], "pass", "{porch}");
    assert_code(&output, 0);
}

// ---- 3. roll back a bad install; the receipt has its own exit code ----

#[test]
fn a_corrupted_install_is_rolled_back_to_the_verified_backup() {
    let rig = Rig::new();
    let live = rig.live("rollback");
    let corrupt = rig.sandbox.path.join("corrupt");
    fs::create_dir_all(&corrupt).expect("corrupt dir");
    write_exec(&corrupt.join("install"), CORRUPTING_INSTALL);
    let output = rig.run_with(Some(&corrupt), &[], &[]);
    assert_code(&output, 5);
    assert_eq!(
        fs::read(rig.target()).expect("post"),
        live,
        "post holds the pre-install bytes"
    );
    assert_eq!(
        fs::read(rig.bin_dir.join("post-c808283.bak")).expect("backup"),
        live
    );
    assert!(
        !rig.receipt.exists(),
        "no receipt for a rolled-back install"
    );
    assert_eq!(rig.bin_entries(), ["post", "post-c808283.bak"]);
}

#[test]
fn a_corrupted_first_install_is_removed() {
    let rig = Rig::new();
    let corrupt = rig.sandbox.path.join("corrupt");
    fs::create_dir_all(&corrupt).expect("corrupt dir");
    write_exec(&corrupt.join("install"), CORRUPTING_INSTALL);
    let output = rig.run_with(Some(&corrupt), &[], &[]);
    assert_code(&output, 5);
    assert!(rig.bin_entries().is_empty(), "{:?}", rig.bin_entries());
    assert!(!rig.receipt.exists());
}

#[test]
fn a_receipt_that_cannot_be_written_exits_7_with_post_installed() {
    let mut rig = Rig::new();
    let live = rig.live("receipt");
    let blocker = rig.sandbox.path.join("blocker");
    fs::write(&blocker, b"a file where the receipt directory should be").expect("blocker");
    rig.receipt = blocker.join("install-receipt.json");
    let output = rig.run(&[]);
    assert_code(&output, 7);
    assert_eq!(fs::read(rig.target()).expect("post"), rig.built_bytes());
    assert_eq!(
        fs::read(rig.bin_dir.join("post-c808283.bak")).expect("backup"),
        live
    );
    assert!(stderr(&output).contains("could not write the receipt"));
}

// ---- 4 and 6. only a match is a match ----

#[test]
fn served_skill_verdicts_map_to_distinct_exit_codes() {
    for (verify, code, verdict) in [
        ("match", 0, "match"),
        ("drift", 1, "drift"),
        ("error", 4, "unchecked"),
        ("unverified", 4, "unverified"),
    ] {
        let rig = Rig::new();
        rig.live(verify);
        let output = rig.run(&[("FAKE_VERIFY", verify)]);
        assert_code(&output, code);
        assert_eq!(
            fs::read(rig.target()).expect("post"),
            rig.built_bytes(),
            "{verify}: installed"
        );
        assert_eq!(rig.receipt()["manifest_verdict"], verdict, "{verify}");
    }
}

/// A dry run exits with the code the real install would, and still writes
/// nothing whatever that code is.
#[test]
fn a_dry_run_exits_with_the_install_outcome_code_and_writes_nothing() {
    for (verify, code) in [("match", 0), ("drift", 1), ("error", 4), ("unverified", 4)] {
        let rig = Rig::new();
        let live = rig.live(verify);
        let before = rig.bin_entries();
        let output = rig.run_with(None, &[("FAKE_VERIFY", verify)], &["--dry-run"]);
        assert_code(&output, code);
        assert!(
            stderr(&output).contains(&format!("would exit {code}")),
            "{verify}: {}",
            stderr(&output)
        );
        assert_eq!(rig.bin_entries(), before, "{verify}");
        assert_eq!(fs::read(rig.target()).expect("post"), live, "{verify}");
        assert!(!rig.receipt.exists(), "{verify}");
    }
}

// ---- 5. refuse a symlinked target ----

#[test]
fn a_symlinked_target_is_refused_before_any_write() {
    let rig = Rig::new();
    let real = rig.sandbox.path.join("elsewhere/post");
    fs::create_dir_all(real.parent().expect("parent")).expect("elsewhere");
    write_exec(&real, &live_post("behind a link"));
    let real_bytes = fs::read(&real).expect("real post");
    std::os::unix::fs::symlink(&real, rig.target()).expect("link");
    for args in [&[][..], &["--dry-run"][..]] {
        let output = rig.run_with(None, &[], args);
        assert_code(&output, 3);
        assert!(
            stderr(&output).contains(&format!("is a symlink to {}", real.display())),
            "{}",
            stderr(&output)
        );
        assert!(fs::symlink_metadata(rig.target())
            .expect("link")
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(rig.target()).expect("link"), real);
        assert_eq!(fs::read(&real).expect("real post"), real_bytes);
        assert_eq!(rig.bin_entries(), ["post"]);
        assert!(!rig.receipt.exists());
    }
}

// ---- 7. every installed build is traceable to a branch on origin ----

#[test]
fn a_commit_no_origin_branch_contains_is_refused_before_the_build() {
    let mut rig = Rig::new();
    let live = rig.live("unpushed");
    rig.unpushed_commit();
    for args in [&[][..], &["--dry-run"][..]] {
        let output = rig.run_with(None, &[], args);
        assert_code(&output, 3);
        let text = stderr(&output);
        assert!(
            text.contains(&format!("refusing to install {}", rig.short_sha))
                && text.contains("no branch on origin contains it")
                && text.contains("git push origin"),
            "{text}"
        );
        assert!(
            !text.contains("building"),
            "the refusal comes before the build: {text}"
        );
        assert_eq!(fs::read(rig.target()).expect("post"), live);
        assert_eq!(rig.bin_entries(), ["post"]);
        assert!(!rig.receipt.exists());
    }
}

/// Reachable from any branch counts, not just main; the receipt names them.
#[test]
fn a_commit_on_any_origin_branch_installs_and_the_receipt_names_the_branches() {
    let mut rig = Rig::new();
    rig.live("feature");
    rig.unpushed_commit();
    rig.push_to("worktree-fix", false);
    assert_code(&rig.run(&[]), 0);
    let receipt = rig.receipt();
    assert_eq!(receipt["reachable"], true);
    assert_eq!(
        receipt["origin_branches"],
        serde_json::json!(["worktree-fix"])
    );

    // The same commit on a second branch is listed on both.
    rig.push_to("main", false);
    assert_code(&rig.run(&[]), 0);
    assert_eq!(
        rig.receipt()["origin_branches"],
        serde_json::json!(["main", "worktree-fix"])
    );
}

/// A push from another machine reaches origin but not this clone's
/// remote-tracking refs until a fetch. The installer fetches first, so it
/// does not refuse a commit that is on origin.
#[test]
fn the_installer_fetches_origin_before_judging_reachability() {
    let mut rig = Rig::new();
    rig.live("fetch");
    rig.unpushed_commit();
    rig.push_to("main", true);
    let stale = git(&rig.repo, &["branch", "-r", "--contains", "HEAD"]);
    assert_eq!(stale, "", "the clone has not fetched the push yet");
    assert_code(&rig.run(&[]), 0);
    assert_eq!(
        rig.receipt()["origin_branches"],
        serde_json::json!(["main"])
    );
}

#[test]
fn allow_unreachable_installs_an_unpushed_commit_and_records_the_exception() {
    let mut rig = Rig::new();
    let live = rig.live("allowed");
    rig.unpushed_commit();
    let output = rig.run_with(None, &[], &["--allow-unreachable"]);
    assert_code(&output, 0);
    assert!(stderr(&output).contains("WARNING"), "{}", stderr(&output));
    assert_eq!(fs::read(rig.target()).expect("post"), rig.built_bytes());
    assert_backed_up_and_installed(&rig, &live);
    let receipt = rig.receipt();
    assert_eq!(receipt["reachable"], false);
    assert_eq!(receipt["origin_branches"], serde_json::json!([]));
}

/// A clone that cannot reach origin still holds the remote-tracking refs it
/// fetched last time. Those say nothing about origin now, so a failed fetch
/// refuses instead of falling back to them.
#[test]
fn a_failed_fetch_refuses_even_when_the_cached_refs_hold_the_commit() {
    let mut rig = Rig::new();
    let live = rig.live("fetch fails");
    rig.unpushed_commit();
    rig.push_to("worktree-fix", false);
    let cached = git(&rig.repo, &["branch", "-r", "--contains", "HEAD"]);
    assert!(
        cached.contains("origin/worktree-fix"),
        "the cached refs hold the commit: {cached}"
    );
    let gone = rig.sandbox.path.join("origin-is-gone.git");
    git(
        &rig.repo,
        &["remote", "set-url", "origin", &gone.display().to_string()],
    );
    for args in [&[][..], &["--dry-run"][..]] {
        let output = rig.run_with(None, &[], args);
        assert_code(&output, 3);
        let text = stderr(&output);
        assert!(
            text.contains(&format!("refusing to install {}", rig.short_sha))
                && text.contains("could not fetch origin")
                && text.contains("--allow-unreachable"),
            "{text}"
        );
        assert!(
            !text.contains("building"),
            "the refusal comes before the build: {text}"
        );
        assert_eq!(fs::read(rig.target()).expect("post"), live);
        assert!(!rig.receipt.exists());
    }
}

#[test]
fn allow_unreachable_after_a_failed_fetch_records_unverified_not_reachable() {
    let mut rig = Rig::new();
    let live = rig.live("fetch fails, allowed");
    rig.unpushed_commit();
    rig.push_to("worktree-fix", false);
    let gone = rig.sandbox.path.join("origin-is-gone.git");
    git(
        &rig.repo,
        &["remote", "set-url", "origin", &gone.display().to_string()],
    );
    let output = rig.run_with(None, &[], &["--allow-unreachable"]);
    assert_code(&output, 0);
    let text = stderr(&output);
    assert!(
        text.contains("WARNING") && text.contains("reachable=unverified"),
        "{text}"
    );
    assert_backed_up_and_installed(&rig, &live);
    let receipt = rig.receipt();
    assert_eq!(receipt["reachable"], "unverified");
    assert_eq!(
        receipt["origin_branches"],
        serde_json::json!([]),
        "nothing is claimed from the cached refs"
    );
}

/// The remote-tracking ref of a branch deleted on origin lingers in a clone
/// until a fetch prunes it. An abandoned commit must not pass on it.
#[test]
fn a_branch_deleted_on_origin_no_longer_makes_its_commit_reachable() {
    let mut rig = Rig::new();
    let live = rig.live("abandoned");
    rig.unpushed_commit();
    rig.push_to("worktree-fix", false);
    git(&rig.origin, &["branch", "-D", "worktree-fix"]);
    let cached = git(&rig.repo, &["branch", "-r", "--contains", "HEAD"]);
    assert!(
        cached.contains("origin/worktree-fix"),
        "the clone still holds the deleted branch's ref: {cached}"
    );
    let output = rig.run(&[]);
    assert_code(&output, 3);
    let text = stderr(&output);
    assert!(
        text.contains(&format!("refusing to install {}", rig.short_sha))
            && text.contains("no branch on origin contains it"),
        "{text}"
    );
    assert_eq!(fs::read(rig.target()).expect("post"), live);
    assert!(!rig.receipt.exists());

    // With the override it installs and says false: origin answered, and no
    // branch there holds the commit.
    assert_code(&rig.run_with(None, &[], &["--allow-unreachable"]), 0);
    assert_eq!(rig.receipt()["reachable"], false);
    assert_eq!(rig.receipt()["origin_branches"], serde_json::json!([]));
}

#[test]
fn a_checkout_with_no_origin_cannot_vouch_for_a_commit() {
    let rig = Rig::new();
    let live = rig.live("no origin");
    git(&rig.repo, &["remote", "remove", "origin"]);
    let output = rig.run(&[]);
    assert_code(&output, 3);
    assert!(
        stderr(&output).contains("no origin remote"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(rig.target()).expect("post"), live);
    assert!(!rig.receipt.exists());

    // Overridden, nothing was checked against anything: unverified, not false.
    assert_code(&rig.run_with(None, &[], &["--allow-unreachable"]), 0);
    assert_eq!(rig.receipt()["reachable"], "unverified");
}

#[test]
fn the_smoke_is_told_which_commit_is_being_installed() {
    let rig = Rig::new();
    rig.live("expect build");
    let args = rig.sandbox.path.join("smoke-args.txt");
    assert_code(
        &rig.run(&[("FAKE_SMOKE_ARGS", &args.display().to_string())]),
        0,
    );
    let recorded = fs::read_to_string(&args).expect("smoke args");
    assert!(
        recorded.contains(&format!("--expect-build {} ", rig.short_sha)),
        "{recorded}"
    );
}
