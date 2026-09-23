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
const REGISTRY_MAX_BYTES: u64 = 4096;
const REGISTRY_MAX_HOSTS: usize = 64;
const HEALTH_MAX_BYTES: u64 = 64 * 1024;

/// The capabilities an F3 bridge advertises. Both must be present before post
/// writes a host-qualified letter anywhere.
pub(crate) const REQUIRED_CAPABILITIES: [&str; 2] =
    ["typed-outbound-exclusion", "participant-mail-v1"];

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

/// The bridge's persisted host registry, `bridge/registry/hosts.json`:
/// exactly `{"v":1,"hosts":[...]}`, at most 64 unique hosts in the bridge's
/// grammar, at most 4 KiB. A missing file is an error, not an empty set: post
/// never falls back to config peers (that could resurrect a revoked host).
pub(crate) fn load_registry(context: &Context) -> Result<Vec<String>, String> {
    let path = bridge_dir(context).join("registry").join("hosts.json");
    let bytes = read_regular(&path, REGISTRY_MAX_BYTES)?
        .ok_or_else(|| format!("{} does not exist", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{} must be a JSON object", path.display()))?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["hosts", "v"] {
        return Err(format!(
            "{} must have exactly the keys v and hosts",
            path.display()
        ));
    }
    if object.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(format!("{} v must be 1", path.display()));
    }
    let hosts = object
        .get("hosts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{} hosts must be an array", path.display()))?;
    if hosts.len() > REGISTRY_MAX_HOSTS {
        return Err(format!(
            "{} lists more than {REGISTRY_MAX_HOSTS} hosts",
            path.display()
        ));
    }
    let mut seen = Vec::with_capacity(hosts.len());
    for host in hosts {
        let host = host
            .as_str()
            .filter(|host| valid_host(host))
            .ok_or_else(|| format!("{} lists an invalid host {host}", path.display()))?;
        if seen.iter().any(|known| known == host) {
            return Err(format!("{} lists {host} twice", path.display()));
        }
        seen.push(host.to_owned());
    }
    Ok(seen)
}

/// The effective enrolled peer set: the registry minus this host,
/// intersected with the config's `peers` keys when that map is non-empty.
pub(crate) fn enrolled_peers(
    context: &Context,
    config: &BridgeConfig,
) -> Result<Vec<String>, String> {
    let mut peers: Vec<String> = load_registry(context)?
        .into_iter()
        .filter(|host| host != &config.host)
        .filter(|host| config.peers.is_empty() || config.peers.contains(host))
        .collect();
    peers.sort();
    Ok(peers)
}

/// What `bridge/health.json` says about the running bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BridgeHealth {
    /// Fresh, with every required capability.
    Ready,
    /// Fresh, but known to predate F3: the named capabilities are missing.
    Unsupported(Vec<String>),
    /// Missing, unreadable, malformed, or stale: the running bridge is unknown.
    Unavailable(String),
}

/// Read `bridge/health.json` against `now`. Fresh means `ticked_at` is no
/// older than three times `interval_s`, and not further in the future than
/// that either (a far-future stamp could vouch forever). Unknown keys are
/// ignored; the bridge adds counters over time.
/// How far ahead of this host's clock a bridge stamp may be.
pub(crate) const MAX_CLOCK_SKEW: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) fn bridge_health(context: &Context, now: std::time::SystemTime) -> BridgeHealth {
    let path = bridge_dir(context).join("health.json");
    let value = match read_health_json(&path) {
        Ok(value) => value,
        Err(reason) => return BridgeHealth::Unavailable(reason),
    };
    let Some(capabilities) = value
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
    else {
        return BridgeHealth::Unavailable(format!("{} has no capabilities array", path.display()));
    };
    let mut advertised = Vec::with_capacity(capabilities.len());
    for capability in capabilities {
        let Some(capability) = capability.as_str() else {
            return BridgeHealth::Unavailable(format!(
                "{} capabilities must be strings",
                path.display()
            ));
        };
        advertised.push(capability);
    }
    if let Err(reason) = health_is_fresh(&value, &path, now) {
        return BridgeHealth::Unavailable(reason);
    }
    let missing: Vec<String> = REQUIRED_CAPABILITIES
        .iter()
        .filter(|required| !advertised.contains(required))
        .map(|required| (*required).to_owned())
        .collect();
    if missing.is_empty() {
        BridgeHealth::Ready
    } else {
        BridgeHealth::Unsupported(missing)
    }
}

/// The room-rename interlock: what the bridge's export guard must prove
/// before a name can move under it. `bridge/health.json` must be fresh by
/// the participant-mail rule AND carry a `local_held` object whose `faults`
/// and `candidates_unaccounted` are both the integer 0. `ok` is deliberately
/// not consulted — `ok:false room_name_collision` is exactly the state a
/// rename exists to resolve. Err carries the operator-facing reason.
pub(crate) fn export_guard_health(
    context: &Context,
    now: std::time::SystemTime,
) -> Result<(), String> {
    let path = bridge_dir(context).join("health.json");
    let value = read_health_json(&path)?;
    health_is_fresh(&value, &path, now)?;
    let Some(local_held) = value.get("local_held").and_then(|held| held.as_object()) else {
        return Err(format!("{} has no local_held object", path.display()));
    };
    for field in ["faults", "candidates_unaccounted"] {
        match local_held.get(field).and_then(serde_json::Value::as_u64) {
            Some(0) => {}
            Some(other) => {
                return Err(format!(
                    "{} reports local_held.{field} = {other}: the bridge is not cleanly holding this host's names",
                    path.display()
                ))
            }
            None => {
                return Err(format!(
                    "{} local_held.{field} is missing or not an integer",
                    path.display()
                ))
            }
        }
    }
    Ok(())
}

/// Read and parse `bridge/health.json`. Err covers missing, unreadable,
/// oversized, and non-JSON files; no freshness or content judgment here.
fn read_health_json(path: &Path) -> Result<serde_json::Value, String> {
    let bytes = match read_regular(path, HEALTH_MAX_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(format!("{} does not exist", path.display())),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))
}

/// Freshness per the participant-mail rule: `ticked_at` is no older than
/// three times `interval_s` and never further ahead than MAX_CLOCK_SKEW.
fn health_is_fresh(
    value: &serde_json::Value,
    path: &Path,
    now: std::time::SystemTime,
) -> Result<(), String> {
    let Some(ticked_at) = value
        .get("ticked_at")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::participant::parse_rfc3339)
    else {
        return Err(format!(
            "{} ticked_at is missing or not RFC 3339",
            path.display()
        ));
    };
    let Some(interval) = value
        .get("interval_s")
        .and_then(serde_json::Value::as_f64)
        .filter(|interval| interval.is_finite() && *interval > 0.0 && *interval <= 86_400.0)
    else {
        return Err(format!(
            "{} interval_s must be a positive number of seconds (at most 86400)",
            path.display()
        ));
    };
    let window = std::time::Duration::from_secs_f64(interval * 3.0);
    let fresh = match now.duration_since(ticked_at) {
        Ok(age) => age <= window,
        // A stamp from the future is clock skew, allowed only briefly; it
        // never earns a second freshness window.
        Err(ahead) => ahead.duration() <= MAX_CLOCK_SKEW,
    };
    if !fresh {
        return Err(format!(
            "{} is stale: ticked_at is older than three times interval_s or more than {} s ahead",
            path.display(),
            MAX_CLOCK_SKEW.as_secs()
        ));
    }
    Ok(())
}

/// A `participant:<id>@<host>` address after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostQualified {
    /// Resolved to a local address: an exact local record named `<id>@<host>`,
    /// or `<host>` is this host.
    Local(crate::participant::Address),
    /// A participant on an enrolled peer host.
    Remote { id: String, host: String },
}

/// Resolve a host-qualified participant address (design "The address").
/// `Ok(None)` means `raw` is not one (no `participant:` prefix, no `@`, or an
/// exact room name), and the caller's ordinary resolver applies. Every error
/// is final: a host-qualified address never falls back to a workspace, a
/// lineage, or the bare id.
pub(crate) fn resolve_host_qualified(
    context: &Context,
    raw: &str,
) -> crate::error::AppResult<Option<HostQualified>> {
    use crate::error::{AppError, ErrorCode};
    use crate::participant::{Address, AddressKind};
    let Some(rest) = raw.strip_prefix("participant:") else {
        return Ok(None);
    };
    if !rest.contains('@') || context.load_rooms()?.contains_key(raw) {
        return Ok(None);
    }
    // 1. An exact local record keeps a historical id containing `@` local.
    if crate::participant::load(context, rest)?.is_some() {
        return Ok(Some(HostQualified::Local(Address {
            kind: AddressKind::Participant,
            name: rest.to_owned(),
        })));
    }
    // 2. Split at the last `@`.
    let (id, host) = rest.rsplit_once('@').expect("contains '@'");
    if !valid_host(host) {
        return Err(AppError::invalid_argument(format!(
            "host '{host}' in '{raw}' must match ^[a-z0-9-]{{1,32}}$"
        ))
        .input(raw)
        .reason("invalid host in a host-qualified participant address"));
    }
    if id.is_empty() || id.contains('@') {
        return Err(AppError::invalid_argument(format!(
            "participant id '{id}' in '{raw}' must be non-empty and must not contain '@'"
        ))
        .input(raw)
        .reason("invalid participant id in a host-qualified address"));
    }
    crate::participant::validate_participant_id(id)?;
    let config = match load_config(context) {
        Ok(Some(config)) => config,
        Ok(None) => {
            return Err(AppError::new(
                ErrorCode::NoBridge,
                format!("'{raw}' names a host, but this host has no bridge config"),
                "Host-qualified addresses need the post bridge; send to a local address, or install and configure the bridge first.",
            )
            .input(raw)
            .reason("bridge/config.json is absent"));
        }
        Err(error) => return Err(topology_unavailable(raw, error)),
    };
    // 3. Own host resolves with the unchanged local resolver.
    if host == config.host {
        return crate::participant::resolve_target(context, &format!("participant:{id}"))
            .map(|address| Some(HostQualified::Local(address)));
    }
    // 4. A remote host must be in the effective enrolled set.
    let peers =
        enrolled_peers(context, &config).map_err(|error| topology_unavailable(raw, error))?;
    if !peers.iter().any(|peer| peer == host) {
        let enrolled = if peers.is_empty() {
            "none".to_owned()
        } else {
            peers.join(", ")
        };
        return Err(AppError::new(
            ErrorCode::UnknownHost,
            format!("host '{host}' is not an enrolled peer (enrolled: {enrolled})"),
            "Retry with a host listed in details.matches; a host joins that list only when the bridge enrolls it.",
        )
        .input(raw)
        .reason("host is not in the effective enrolled peer set")
        .matches(peers));
    }
    Ok(Some(HostQualified::Remote {
        id: id.to_owned(),
        host: host.to_owned(),
    }))
}

fn topology_unavailable(raw: &str, detail: String) -> crate::error::AppError {
    crate::error::AppError::new(
        crate::error::ErrorCode::TopologyUnavailable,
        format!("cannot resolve '{raw}': {detail}"),
        "The bridge keeps this file current; wait for its next tick, check `post-bridge status`, and retry.",
    )
    .input(raw)
    .reason(detail)
}
