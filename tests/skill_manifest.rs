//! D: the skill bundle manifest built into the binary, and the install-time
//! check of a served skill path against it.

mod common;

use common::{post_command, Sandbox};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

fn skill_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/post")
}

fn run(args: &[&str]) -> Output {
    post_command().args(args).output().expect("run post")
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "JSON ({error}): {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// The covered files of a skill tree, by relative path, dot-files skipped.
fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(metadata) = fs::metadata(at) else {
        return;
    };
    if metadata.is_dir() {
        for entry in fs::read_dir(at).expect("skill dir") {
            let entry = entry.expect("entry");
            if !entry.file_name().to_string_lossy().starts_with('.') {
                walk(root, &entry.path(), found);
            }
        }
    } else {
        let relative = at
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        found.insert(relative, fs::read(at).expect("skill file"));
    }
}

fn covered_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    for covered in ["SKILL.md", "references", "hooks", "agents"] {
        walk(root, &root.join(covered), &mut found);
    }
    found
}

fn copy_bundle(to: &Path) {
    for (relative, bytes) in covered_files(&skill_root()) {
        let target = to.join(&relative);
        fs::create_dir_all(target.parent().unwrap()).expect("copy dir");
        fs::write(target, bytes).expect("copy file");
    }
}

fn verify(served: &Path) -> (Option<i32>, Value) {
    let output = run(&[
        "contract",
        "skill-manifest",
        "--verify",
        served.to_str().expect("utf-8"),
    ]);
    (output.status.code(), json(&output))
}

/// The build script's hand-written sha256 agrees with the sha2 crate on every
/// file, and the manifest covers exactly the served tree.
#[test]
fn manifest_is_the_sha256_of_every_covered_file() {
    let output = run(&["contract", "skill-manifest"]);
    assert!(output.status.success());
    let manifest = json(&output);
    assert_eq!(manifest["root"], "skills/post");
    let listed: BTreeMap<String, String> = manifest["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().to_owned(),
                file["sha256"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let actual: BTreeMap<String, String> = covered_files(&skill_root())
        .into_iter()
        .map(|(path, bytes)| {
            let hex = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            (path, hex)
        })
        .collect();
    assert_eq!(listed, actual);
    assert!(listed.contains_key("SKILL.md"));
    assert!(listed.keys().any(|path| path.starts_with("references/")));
    assert!(listed.keys().any(|path| path.starts_with("hooks/")));
    assert!(listed.keys().any(|path| path.starts_with("agents/")));
    assert_eq!(manifest["count"], listed.len());
}

#[test]
fn a_served_symlink_is_checked_through_what_it_resolves_to() {
    let sandbox = Sandbox::new();
    let served = sandbox.path.join("post");
    std::os::unix::fs::symlink(skill_root(), &served).expect("served symlink");
    let (code, report) = verify(&served);
    assert_eq!(code, Some(0), "{report}");
    assert_eq!(report["verdict"], "match");
    assert_eq!(report["kind"], "symlink");
    assert_eq!(
        report["resolved"],
        fs::canonicalize(skill_root())
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
}

#[test]
fn a_faithful_copy_matches_and_is_recorded_as_a_copy() {
    let sandbox = Sandbox::new();
    let served = sandbox.path.join("post");
    copy_bundle(&served);
    fs::write(served.join(".DS_Store"), "litter").expect("dot-file litter");
    let (code, report) = verify(&served);
    assert_eq!(code, Some(0), "{report}");
    assert_eq!(report["verdict"], "match");
    assert_eq!(report["kind"], "copy");
}

#[test]
fn a_changed_missing_or_extra_served_file_is_drift() {
    let sandbox = Sandbox::new();
    let served = sandbox.path.join("post");
    copy_bundle(&served);
    let skill = served.join("SKILL.md");
    let mut text = fs::read_to_string(&skill).expect("SKILL.md");
    text.push_str("\nOne served line the binary never saw.\n");
    fs::write(&skill, text).expect("edit served SKILL.md");
    let (code, report) = verify(&served);
    assert_eq!(code, Some(1), "{report}");
    assert_eq!(report["verdict"], "drift");
    assert_eq!(report["ok"], false);
    assert_eq!(report["mismatched"], serde_json::json!(["SKILL.md"]));

    copy_bundle(&served);
    let reference = fs::read_dir(served.join("references"))
        .expect("references")
        .next()
        .expect("a reference")
        .expect("entry")
        .path();
    let name = format!(
        "references/{}",
        reference.file_name().unwrap().to_string_lossy()
    );
    fs::remove_file(&reference).expect("drop a reference");
    fs::write(served.join("hooks/stray.mjs"), "stray").expect("extra hook");
    let (code, report) = verify(&served);
    assert_eq!(code, Some(1), "{report}");
    assert_eq!(report["missing"], serde_json::json!([name]));
    assert_eq!(report["extra"], serde_json::json!(["hooks/stray.mjs"]));
}

#[test]
fn a_missing_served_path_is_an_error_not_a_verdict() {
    let sandbox = Sandbox::new();
    let output = run(&[
        "contract",
        "skill-manifest",
        "--verify",
        sandbox.path.join("absent").to_str().unwrap(),
    ]);
    assert_ne!(output.status.code(), Some(0));
    assert_ne!(output.status.code(), Some(1), "an error is not drift");
    assert!(output.stdout.is_empty());
}
