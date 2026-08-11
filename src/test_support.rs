use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn test_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock should follow Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("post-{label}-{nonce}"));
    fs::create_dir_all(&root).expect("create test root");
    root
}

pub(crate) fn trash_test_root(root: &Path) {
    // Prefer the recoverable `trash` cleanup where it exists (this project's
    // home machine); fall back to a direct remove on machines without it
    // (CI runners, stranger installs) — the root is a temp dir this process
    // created, so an unrecoverable delete of it is safe.
    match std::process::Command::new("trash").arg(root).status() {
        Ok(status) => assert!(status.success(), "trash should clean test root"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::remove_dir_all(root).expect("run test cleanup");
        }
        Err(error) => panic!("run recoverable test cleanup: {error}"),
    }
}
