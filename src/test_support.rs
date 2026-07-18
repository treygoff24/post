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
    let cleanup = std::process::Command::new("trash")
        .arg(root)
        .status()
        .expect("run recoverable test cleanup");
    assert!(cleanup.success(), "trash should clean test root");
}
