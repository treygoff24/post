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

/// The stub smoke committed into the throwaway repo. FAKE_SMOKE picks what
/// it reports through --results.
const STUB_SMOKE: &str = r#"#!/usr/bin/env bash
[ "$1" = --results ] || { echo "stub smoke: expected --results" >&2; exit 2; }
results="$2"
line() { printf '{"check": "%s", "result": "pass", "detail": ""}\n' "$1"; }
# The six checks the real smoke reports, each once, in its order.
six() {
  for id in setup version samples doorbell_parsers doorbell_contract porch; do line "$id"; done
}
case "${FAKE_SMOKE:-pass}" in
  pass) six > "$results" ;;
  allowed-skip)
    six | sed '$d' > "$results"
    printf '%s\n' '{"check": "porch", "result": "skipped", "detail": "no porch3", "allowed": true}' >> "$results" ;;
  drop-contract) six | grep -v doorbell_contract > "$results" ;;
  unknown-id) { six; line bogus; } > "$results" ;;
  duplicate) { six; line setup; } > "$results" ;;
  skip-version)
    six | grep -v '"version"' > "$results"
    printf '%s\n' '{"check": "version", "result": "skipped", "detail": "x", "allowed": true}' >> "$results" ;;
  silent) : > "$results" ;;
  lying)
    printf '%s\n' '{"check": "porch", "result": "fail", "detail": "launch check failed"}' > "$results" ;;
  fail) exit 1 ;;
esac
"#;

/// The fake cargo: `build --release --locked` writes the scripted post,
/// stamped with the checked-out commit's short sha as build.rs would.
const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
[ "$*" = "build --release --locked" ] || { echo "fake cargo: unexpected: $*" >&2; exit 64; }
sha=$(git rev-parse --short HEAD)
out="${CARGO_TARGET_DIR:-target}/release"
mkdir -p "$out"
sed "s/@SHA@/$sha/g" "$FAKE_POST_TEMPLATE" > "$out/post"
chmod +x "$out/post"
"#;

/// The built binary. FAKE_VERIFY picks the served-skill verdict.
const FAKE_POST: &str = r#"#!/usr/bin/env bash
# fake built post @SHA@
case "$1 ${2:-}" in
  "version --json") printf '{"ok":true,"build_sha":"@SHA@","capabilities":["participants"]}\n' ;;
  "contract skill-manifest")
    case "${FAKE_VERIFY:-match}" in
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
    fakes: PathBuf,
    bin_dir: PathBuf,
    receipt: PathBuf,
    short_sha: String,
}

impl Rig {
    fn new() -> Self {
        let sandbox = Sandbox::new_unseeded();
        let repo = sandbox.path.join("repo");
        fs::create_dir_all(repo.join("scripts")).expect("repo dirs");
        write_exec(&repo.join("scripts/install-smoke.sh"), STUB_SMOKE);
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "scripts/install-smoke.sh"]);
        git(&repo, &["commit", "-q", "-m", "stub"]);
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
            fakes,
            bin_dir,
            receipt,
            short_sha,
        }
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

#[test]
fn a_truncated_backup_under_the_build_sha_name_is_not_trusted() {
    let rig = Rig::new();
    let live = rig.live("truncated-case");
    // A first run that died mid-copy.
    let stale = rig.bin_dir.join("post-c808283.bak");
    fs::write(&stale, &live[..live.len() / 2]).expect("truncated backup");
    let output = rig.run(&[]);
    assert_code(&output, 0);
    let backup = assert_backed_up_and_installed(&rig, &live);
    assert_ne!(backup, stale, "the truncated file is not the backup");
    assert_eq!(
        fs::read(&stale).expect("stale"),
        &live[..live.len() / 2],
        "left alone"
    );
    assert!(
        stderr(&output).contains("not a copy of the live post"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_backup_of_other_bytes_with_the_same_build_sha_is_not_trusted() {
    let rig = Rig::new();
    let live = rig.live("the live build");
    let other = live_post("a different build reporting the same sha");
    let stale = rig.bin_dir.join("post-c808283.bak");
    write_exec(&stale, &other);
    let output = rig.run(&[]);
    assert_code(&output, 0);
    let backup = assert_backed_up_and_installed(&rig, &live);
    assert_eq!(
        backup,
        rig.bin_dir
            .join(format!("post-c808283-{}.bak", &sha256_hex(&live)[..12]))
    );
    assert_eq!(fs::read_to_string(&stale).expect("stale"), other);
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
    assert_eq!(receipt["smoke_checks"][5]["check"], "porch");
    assert_eq!(receipt["smoke_checks"][5]["result"], "skipped");
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
    assert_eq!(
        checks,
        [
            "setup",
            "version",
            "samples",
            "doorbell_parsers",
            "doorbell_contract",
            "porch"
        ]
    );
}

/// The gate knows the six checks: a smoke that exits 0 while one is missing,
/// repeated, unknown, or skipped (other than porch) installs nothing.
#[test]
fn a_smoke_missing_a_check_or_reporting_an_unknown_one_installs_nothing() {
    for (mode, why) in [
        ("drop-contract", "doorbell_contract"),
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
    for mode in ["silent", "lying"] {
        let rig = Rig::new();
        let live = rig.live(mode);
        let output = rig.run(&[("FAKE_SMOKE", mode)]);
        assert_code(&output, 3);
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
            .env_remove("BASH_ENV")
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
    assert_eq!(
        ids,
        [
            "setup",
            "version",
            "samples",
            "doorbell_parsers",
            "doorbell_contract",
            "porch"
        ]
    );
    for check in &checks[..5] {
        assert_eq!(check["result"], "pass", "{check}");
    }
    assert_eq!(checks[5]["result"], "skipped");
    assert_eq!(checks[5]["allowed"], true);
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
