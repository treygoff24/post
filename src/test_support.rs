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
    // The root is a uniquely named temp dir this test created; plain stdlib
    // removal is the portable cleanup, with no external binary involved.
    fs::remove_dir_all(root).expect("run test cleanup");
}
