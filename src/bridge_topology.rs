//! What post knows about this host's bridge: its own host name, the enrolled
//! peer set, and whether the running bridge can carry participant mail.
//!
//! Post reads these files; the bridge writes them. Every reader here fails
//! visibly: a missing or invalid file is its own answer, never "no peers" or
//! "the bridge is fine".

use crate::mailbox::Context;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const CONFIG_MAX_BYTES: u64 = 1024 * 1024;

/// `^[a-z0-9-]{1,32}$`, the bridge's host grammar.
pub(crate) fn valid_host(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

pub(crate) fn bridge_dir(context: &Context) -> PathBuf {
    context.root.join("bridge")
}

/// This host's bridge config, as far as post needs it.
#[derive(Debug, Clone)]
pub(crate) struct BridgeConfig {
    pub host: String,
    /// Host keys of `peers`; empty when the map is absent or empty.
    #[allow(dead_code)] // read by host-qualified address resolution
    pub peers: Vec<String>,
}

/// Read `bridge/config.json`. `Ok(None)` means this host has no bridge
/// config at all; `Err` means one exists but cannot be trusted.
pub(crate) fn load_config(context: &Context) -> Result<Option<BridgeConfig>, String> {
    let path = bridge_dir(context).join("config.json");
    let Some(bytes) = read_regular(&path, CONFIG_MAX_BYTES)? else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{} must be a JSON object", path.display()))?;
    let host = object
        .get("host")
        .and_then(serde_json::Value::as_str)
        .filter(|host| valid_host(host))
        .ok_or_else(|| {
            format!(
                "{} host must be a string matching ^[a-z0-9-]{{1,32}}$",
                path.display()
            )
        })?
        .to_owned();
    let peers = match object.get("peers") {
        None => Vec::new(),
        Some(serde_json::Value::Object(peers)) => {
            let mut hosts = Vec::new();
            for key in peers.keys() {
                if !valid_host(key) {
                    return Err(format!(
                        "{} peers key '{key}' is not a valid host",
                        path.display()
                    ));
                }
                hosts.push(key.clone());
            }
            hosts
        }
        Some(_) => return Err(format!("{} peers must be an object", path.display())),
    };
    Ok(Some(BridgeConfig { host, peers }))
}

/// Read a bounded regular file. `Ok(None)` only for a conclusively absent
/// path; a symlink, a directory, an oversize file, or an I/O error is `Err`.
pub(crate) fn read_regular(path: &Path, max: u64) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    };
    if !metadata.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(max + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(format!("{} exceeds {max} bytes", path.display()));
    }
    Ok(Some(bytes))
}
