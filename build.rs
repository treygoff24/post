use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// True when tracked files differ from HEAD: the binary was built from code
/// no commit contains, so its short sha alone would name the wrong source.
/// Untracked files do not count (they never affect a cargo build unless a
/// tracked file references them, and `target/` litter must not read dirty).
fn tree_is_dirty() -> bool {
    git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|out| !out.is_empty())
}

fn main() {
    let sha = git(&["rev-parse", "--short", "HEAD"])
        .filter(|value| !value.is_empty())
        .map(|value| {
            if tree_is_dirty() {
                format!("{value}-dirty")
            } else {
                value
            }
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=POST_BUILD_SHA={sha}");

    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        .and_then(|reference| git(&["rev-parse", "--git-path", &reference]))
    {
        println!("cargo:rerun-if-changed={reference}");
    }
    // The dirty flag must follow edits and staging, not only new commits.
    // Rerunning this script is cheap and cargo recompiles the crate only when
    // the emitted value actually changes.
    if let Some(index) = git(&["rev-parse", "--git-path", "index"]) {
        println!("cargo:rerun-if-changed={index}");
    }
    for tracked in ["src", "Cargo.toml", "Cargo.lock", "build.rs", "tests"] {
        println!("cargo:rerun-if-changed={tracked}");
    }

    skill_manifest();
}

/// The served skill bundle's files, relative to `skills/post/`.
const SKILL_ROOT: &str = "skills/post";
const SKILL_COVERED: [&str; 4] = ["SKILL.md", "references", "hooks", "agents"];

/// D: embed the sha256 of every served skill file, so an install can check
/// the prose and hooks actually served against the binary it installs. The
/// manifest lives in the binary, not in the bundle, so it never covers itself.
/// Dot-files (editor and OS litter such as .DS_Store) are not part of the
/// bundle on either side of the comparison.
fn skill_manifest() {
    // Read the manifest dir when the script runs, never with `env!`: cargo
    // reuses a compiled build script across checkouts that share a target
    // dir, and a path baked in at compile time names whichever checkout
    // compiled it (on the devbox, an installer's since-deleted temp worktree),
    // which silently produced an empty manifest.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let root = Path::new(&manifest_dir).join(SKILL_ROOT);
    assert!(
        root.join("SKILL.md").is_file(),
        "skill bundle missing at {}: refusing to embed an empty skill manifest",
        root.display()
    );
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    for covered in SKILL_COVERED {
        collect(&root, &root.join(covered), &mut files);
    }
    files.sort();
    let mut table = format!(
        "pub(crate) const SKILL_ROOT: &str = {SKILL_ROOT:?};\npub(crate) const SKILL_COVERED: &[&str] = &{SKILL_COVERED:?};\npub(crate) const SKILL_MANIFEST: &[SkillFile] = &[\n"
    );
    for (relative, path) in files {
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let fenced = has_fence_marker(&bytes);
        table.push_str(&format!(
            "    SkillFile {{ path: {relative:?}, sha256: {:?}, fenced: {fenced} }},\n",
            hex(&sha256(&bytes))
        ));
    }
    table.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("skill_manifest.rs");
    std::fs::write(&out, table).expect("write skill manifest");
}

fn collect(root: &Path, path: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let entries = std::fs::read_dir(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for entry in entries {
            let entry = entry.expect("skill entry");
            if !entry.file_name().to_string_lossy().starts_with('.') {
                collect(root, &entry.path(), files);
            }
        }
    } else if metadata.is_file() {
        let relative = path
            .strip_prefix(root)
            .expect("under the skill root")
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        files.push((relative, path.to_path_buf()));
    }
}

/// Whether the file carries a skill-render fence marker line such as
/// `<!-- mac-only -->`: a rendered copy of such a file legitimately differs.
fn has_fence_marker(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).lines().any(|line| {
        let line = line.trim();
        line.starts_with("<!--")
            && line.ends_with("-->")
            && line
                .trim_start_matches("<!--")
                .trim_end_matches("-->")
                .trim()
                .trim_start_matches('/')
                .ends_with("-only")
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 (FIPS 180-4). The build script has no crates of its own; the
/// suite cross-checks every entry against the `sha2` crate.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[index] = u32::from_be_bytes(*word);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut digest = [0u8; 32];
    for (chunk, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *chunk = word.to_be_bytes();
    }
    digest
}
