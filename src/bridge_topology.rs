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
use unicode_normalization::UnicodeNormalization;

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

/// How far ahead of this host's clock a bridge stamp may be.
pub(crate) const MAX_CLOCK_SKEW: std::time::Duration = std::time::Duration::from_secs(5);

/// Read `bridge/health.json` against `now`. Fresh means `ticked_at` is no
/// older than three times `interval_s`, and not further in the future than
/// that either (a far-future stamp could vouch forever). Unknown keys are
/// ignored; the bridge adds counters over time.
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

pub(crate) enum ChannelRelayStatus {
    Queued,
    LocalOnly(String),
    Unconfirmed(String),
}

/// Mirror validate_room(name, topology=True) in post-bridge/bridgelib/common.py.
/// Post accepts a wider channel-name grammar, so a locally valid name can
/// still be permanently unpublishable by the bridge.
fn bridge_refuses_channel_name(value: &str) -> bool {
    let normalized: String = value.nfc().collect();
    if normalized.is_empty()
        || matches!(normalized.as_str(), "." | "..")
        || normalized.contains(['/', '\\'])
        || normalized
            .chars()
            .next()
            .is_some_and(bridge_name_whitespace)
        || normalized
            .chars()
            .last()
            .is_some_and(bridge_name_whitespace)
        || normalized.chars().any(bridge_refused_name_character)
    {
        return true;
    }

    let folded = if normalized.is_ascii() {
        normalized.to_ascii_lowercase()
    } else {
        normalized.clone()
    };
    let compatibility: String = normalized.nfkc().collect();
    let reserved_fold = if compatibility.is_ascii() {
        compatibility.to_ascii_lowercase()
    } else {
        compatibility
    };
    const TOPOLOGY_DENY: &[&str] = &[
        "*",
        "archive",
        "participants",
        "lineages",
        "routing",
        ".participants.lock",
        "rooms.json",
        "rules.json",
        "profiles.json",
        "owner.json",
        ".rooms.lock",
        ".post-arx.json",
        ".post-arx.lock",
        "bridge",
        "remote",
        "channels",
        ".bridge",
        ".bridge.lock",
        "bridge.log",
    ];
    TOPOLOGY_DENY.contains(&folded.as_str())
        || TOPOLOGY_DENY.contains(&reserved_fold.as_str())
        || (folded.starts_with(".rooms.json.") && folded.ends_with(".tmp"))
        || (folded.starts_with("..post-arx.json.") && folded.ends_with(".tmp"))
}

fn bridge_name_whitespace(character: char) -> bool {
    matches!(
        character as u32,
        0x0009..=0x000D | 0x001C..=0x0020 | 0x0085 | 0x00A0 | 0x1680
            | 0x2000..=0x200A | 0x2028..=0x2029 | 0x202F | 0x205F | 0x3000
    )
}

fn bridge_refused_name_character(character: char) -> bool {
    let code = character as u32;
    // refused_profile_char, Unicode category Cf, and default_ignorable in
    // bridgelib/common.py. Cf ranges match Python's Unicode data on this host.
    matches!(
        code,
        0..=31 | 127..=159 | 0x202A..=0x202E | 0x2066..=0x2069
            | 0x200E | 0x200F | 0x061C | 0x2028 | 0x2029
            | 0x00AD | 0x0600..=0x0605 | 0x06DD | 0x070F | 0x0890..=0x0891
            | 0x08E2 | 0x180E | 0x200B..=0x200F | 0x2060..=0x206F
            | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD
            | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F
            | 0x034F | 0x115F..=0x1160 | 0x17B4..=0x17B5
            | 0x180B..=0x180F | 0x3164 | 0xFE00..=0xFE0F | 0xFFA0
            | 0xFFF0..=0xFFF8 | 0xE0000..=0xE0FFF
    )
}

/// A successful channel send is local first. Report durable routing limits
/// separately from a bridge whose current state cannot be confirmed.
pub(crate) fn channel_relay_status(
    context: &Context,
    channel: &str,
    roomless: bool,
) -> ChannelRelayStatus {
    use ChannelRelayStatus::{LocalOnly, Queued, Unconfirmed};
    if bridge_refuses_channel_name(channel) {
        return LocalOnly(format!(
            "channel name is not relayable by the bridge: {channel}"
        ));
    }
    let config_path = bridge_dir(context).join("config.json");
    let config_bytes = match read_regular(&config_path, CONFIG_MAX_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return LocalOnly("no bridge config".to_owned()),
        Err(reason) => return LocalOnly(format!("bridge config unusable: {reason}")),
    };
    let config: serde_json::Value = match serde_json::from_slice(&config_bytes) {
        Ok(value) => value,
        Err(_) => return LocalOnly("invalid bridge config".to_owned()),
    };
    let Some(config_object) = config.as_object() else {
        return LocalOnly("invalid bridge config".to_owned());
    };
    let Some(host) = config_object
        .get("host")
        .and_then(serde_json::Value::as_str)
    else {
        return LocalOnly("invalid bridge host".to_owned());
    };
    if !valid_host(host) {
        return LocalOnly("invalid bridge host".to_owned());
    }
    match config_object.get("channels") {
        Some(serde_json::Value::Null) => return LocalOnly("channel sync is off".to_owned()),
        Some(serde_json::Value::Object(policy)) => {
            if policy
                .keys()
                .any(|key| !matches!(key.as_str(), "mode" | "allow" | "deny"))
            {
                return LocalOnly("invalid channel sync policy".to_owned());
            }
            let names = |key: &str| -> Option<Vec<&str>> {
                policy.get(key).map_or(Some(Vec::new()), |value| {
                    value.as_array().and_then(|values| {
                        values
                            .iter()
                            .map(serde_json::Value::as_str)
                            .collect::<Option<Vec<_>>>()
                    })
                })
            };
            let (Some(allow), Some(deny)) = (names("allow"), names("deny")) else {
                return LocalOnly("invalid channel sync policy".to_owned());
            };
            if allow.iter().chain(&deny).any(|name| {
                crate::mailbox::validate_component(name).is_err()
                    || name.chars().any(char::is_control)
            }) {
                return LocalOnly("invalid channel sync policy".to_owned());
            }
            if deny.contains(&channel) {
                return LocalOnly("channel is denied by this host".to_owned());
            }
            match policy.get("mode").and_then(serde_json::Value::as_str) {
                Some("allow") if !allow.contains(&channel) => {
                    return LocalOnly("channel is not allowlisted".to_owned());
                }
                Some("all" | "allow") => {}
                _ => return LocalOnly("invalid channel sync policy".to_owned()),
            }
        }
        Some(_) => return LocalOnly("invalid channel sync policy".to_owned()),
        None => {}
    }
    let Ok(Some(config)) = load_config(context) else {
        return LocalOnly("invalid bridge config".to_owned());
    };
    match enrolled_peers(context, &config) {
        Ok(peers) if peers.is_empty() => return LocalOnly("no enrolled peer hosts".to_owned()),
        Err(_) => return Unconfirmed(
            "bridge peer registry is unavailable; do not resend; check post doctor or the bridge"
                .to_owned(),
        ),
        _ => {}
    }
    let health_path = bridge_dir(context).join("health.json");
    let Ok(health) = read_health_json(&health_path) else {
        return Unconfirmed("bridge has not reported recently; the post relays on the bridge's next tick if it is running".to_owned());
    };
    if health.get("interval_s").is_none() {
        return Unconfirmed("bridge health has no interval_s; the post relays on the bridge's next tick if it is running".to_owned());
    }
    if health_is_fresh(&health, &health_path, std::time::SystemTime::now()).is_err() {
        return Unconfirmed("bridge has not reported recently; the post relays on the bridge's next tick if it is running".to_owned());
    }
    if roomless
        && !health
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|capabilities| {
                capabilities
                    .iter()
                    .any(|value| value.as_str() == Some("roomless-channel-v1"))
            })
    {
        return Unconfirmed("this host's bridge predates roomless relay; the post relays after the bridge is upgraded; do not resend".to_owned());
    }
    Queued
}

/// The second half of the room-rename interlock. Health counters are
/// carried forward on busy and quiet ticks, so a fresh health file does not
/// prove the last full tick saw every letter: a letter delivered to `room`
/// after it has no hold yet, and once the name leaves this host the guard can
/// no longer stamp one. This applies the bridge's own outbound candidate rule
/// (`select_outbound` in post-bridge `sweep.py`) to `archive/*.mail`: a
/// letter whose envelope is workspace-addressed (`address_kind` absent or
/// `"workspace"`, no `to_host` key), whose `to` is exactly `room`, and that
/// has none of `bridge/received/<id>`, `bridge/published/<id>`, or any
/// `bridge/delivered/*/*/<id>`. Returns every such letter (sorted) that has
/// no `bridge/local-held/<id>.json`. Every marker counts only when its target
/// exists (symlinks followed, as the bridge's `Path.exists()` does); existence
/// is enough, because the bridge
/// holds any letter with a record and faults an invalid one. A letter whose
/// envelope does not parse is skipped, as the bridge skips it. Err carries
/// an operator-facing reason when the store cannot be read.
pub(crate) fn unheld_room_letters(context: &Context, room: &str) -> Result<Vec<String>, String> {
    let archive = context.root.join("archive");
    let entries = match fs::read_dir(&archive) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot list {}: {error}", archive.display())),
    };
    let bridge = bridge_dir(context);
    let mut delivered: Option<std::collections::HashSet<std::ffi::OsString>> = None;
    let mut unheld = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot list {}: {error}", archive.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("mail") {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        let Some(id) = outbound_candidate_id(&bytes, room) else {
            continue;
        };
        // Markers follow symlinks, as the bridge's `Path.exists()` does: a
        // dangling marker is absent there, so the letter would still export.
        let exists = |path: PathBuf| marker_exists(&path);
        if exists(bridge.join("received").join(&id)) || exists(bridge.join("published").join(&id)) {
            continue;
        }
        let delivered = match &delivered {
            Some(set) => set,
            None => delivered.insert(delivered_marker_names(&bridge.join("delivered"))?),
        };
        if delivered.contains(std::ffi::OsStr::new(&id)) {
            continue;
        }
        if !exists(bridge.join("local-held").join(format!("{id}.json"))) {
            unheld.push(id);
        }
    }
    unheld.sort();
    Ok(unheld)
}

/// The mail id when `bytes` is an outbound candidate for `room` under the
/// bridge's rule, else None (including every envelope that does not parse).
fn outbound_candidate_id(bytes: &[u8], room: &str) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (head, _) = text.split_once("\n---\n")?;
    let envelope: serde_json::Map<String, serde_json::Value> = serde_json::from_str(head).ok()?;
    match envelope.get("address_kind") {
        None => {}
        Some(serde_json::Value::String(kind)) if kind == "workspace" => {}
        Some(_) => return None,
    }
    if envelope.contains_key("to_host") || envelope.get("to")?.as_str()? != room {
        return None;
    }
    let id = envelope.get("id")?.as_str()?;
    // An invalid id is skipped: the bridge never exports one either (outbound_ignored).
    crate::mailbox::validate_component(id).ok()?;
    Some(id.to_owned())
}

/// A bridge marker is present only when its target exists (symlinks are
/// followed). Any error, a dangling link included, reads as absent, which
/// can only make the rename interlock refuse.
fn marker_exists(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

/// Every file name at depth two under `bridge/delivered/` (the
/// `delivered/*/*/<id>` markers) whose target exists. A missing tree has no
/// markers.
fn delivered_marker_names(
    root: &Path,
) -> Result<std::collections::HashSet<std::ffi::OsString>, String> {
    let mut names = std::collections::HashSet::new();
    let list = |dir: &Path| -> Result<Vec<PathBuf>, String> {
        match fs::read_dir(dir) {
            Ok(entries) => entries
                .map(|entry| {
                    entry
                        .map(|entry| entry.path())
                        .map_err(|error| format!("cannot list {}: {error}", dir.display()))
                })
                .collect(),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ENOTDIR) =>
            {
                Ok(Vec::new())
            }
            Err(error) => Err(format!("cannot list {}: {error}", dir.display())),
        }
    };
    for first in list(root)? {
        for second in list(&first)? {
            for marker in list(&second)? {
                if !marker_exists(&marker) {
                    continue;
                }
                if let Some(name) = marker.file_name() {
                    names.insert(name.to_owned());
                }
            }
        }
    }
    Ok(names)
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

/// How old the bridge's last health tick is, and whether that is fresh.
pub(crate) struct HealthTick {
    /// Time since `ticked_at`; zero for a stamp slightly ahead of this clock.
    pub age: std::time::Duration,
    pub fresh: bool,
}

/// Read `ticked_at` and `interval_s` from a parsed health file. Err covers a
/// missing or malformed stamp or interval; a stale tick is `Ok` with
/// `fresh: false`, so callers can say how old it is.
fn health_tick_of(
    value: &serde_json::Value,
    path: &Path,
    now: std::time::SystemTime,
) -> Result<HealthTick, String> {
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
    Ok(match now.duration_since(ticked_at) {
        Ok(age) => HealthTick {
            age,
            fresh: age <= window,
        },
        // A stamp from the future is clock skew, allowed only briefly; it
        // never earns a second freshness window.
        Err(ahead) => HealthTick {
            age: std::time::Duration::ZERO,
            fresh: ahead.duration() <= MAX_CLOCK_SKEW,
        },
    })
}

/// The age and freshness of `bridge/health.json`'s last tick. Err says why the
/// file cannot vouch for anything: missing, unreadable, malformed, or without
/// a usable `ticked_at`/`interval_s`.
pub(crate) fn health_tick(
    context: &Context,
    now: std::time::SystemTime,
) -> Result<HealthTick, String> {
    let path = bridge_dir(context).join("health.json");
    let value = read_health_json(&path)?;
    health_tick_of(&value, &path, now)
}

/// Freshness per the participant-mail rule: `ticked_at` is no older than
/// three times `interval_s` and never further ahead than MAX_CLOCK_SKEW.
fn health_is_fresh(
    value: &serde_json::Value,
    path: &Path,
    now: std::time::SystemTime,
) -> Result<(), String> {
    if !health_tick_of(value, path, now)?.fresh {
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

#[cfg(test)]
mod tests {
    use super::{
        bridge_refuses_channel_name, channel_relay_status, unheld_room_letters, ChannelRelayStatus,
        Context,
    };
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;
    use std::path::Path;

    const ID: &str = "20260923-000000-abc123";

    #[test]
    fn bridge_name_rejects_default_ignorable_gap() {
        // U+2065 is refused by the bridge but the macOS filesystem cannot
        // create it as a channel directory for an end-to-end receipt test.
        assert!(bridge_refuses_channel_name("mid\u{2065}gap"));
    }

    /// G3: every hold marker counts only when its target exists. A dangling
    /// symlink is absent to the bridge (`Path.exists()`), so the letter
    /// stays unheld; a link to a real file is present.
    #[test]
    fn dangling_marker_symlinks_do_not_hold_a_letter() {
        let root = test_root("bridge-markers");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        fs::create_dir_all(root.join("archive")).expect("archive");
        fs::write(
            root.join("archive").join(format!("{ID}.mail")),
            format!(
                "{{\"id\":\"{ID}\",\"from\":\"beta\",\"to\":\"alpha\",\"kind\":\"note\",\"subject\":\"\",\"sent\":\"x\"}}\n---\nbody"
            ),
        )
        .expect("archive letter");
        let bridge = root.join("bridge");
        let real = root.join("real-marker");
        fs::write(&real, "x").expect("real marker target");
        let markers = [
            bridge.join("received").join(ID),
            bridge.join("published").join(ID),
            bridge.join("delivered/host-b/alpha").join(ID),
            bridge.join("local-held").join(format!("{ID}.json")),
        ];
        let unheld = || unheld_room_letters(&context, "alpha").expect("scan");
        assert_eq!(unheld(), vec![ID.to_owned()], "no marker: unheld");
        for marker in &markers {
            fs::create_dir_all(marker.parent().expect("marker dir")).expect("marker dir");
            let link = |target: &Path| {
                let _ = fs::remove_file(marker);
                std::os::unix::fs::symlink(target, marker).expect("marker symlink");
            };
            link(&root.join("missing-target"));
            assert_eq!(
                unheld(),
                vec![ID.to_owned()],
                "dangling {} must not hold the letter",
                marker.display()
            );
            link(&real);
            assert!(
                unheld().is_empty(),
                "{} linked to a real file holds the letter",
                marker.display()
            );
            fs::remove_file(marker).expect("remove marker");
        }
        trash_test_root(&root);
    }

    /// A missing config is "no bridge config"; a config that is there but
    /// cannot be used (not a regular file) says so, not that it is absent.
    #[test]
    fn relay_status_names_why_the_config_is_unusable() {
        let root = test_root("bridge-config-reason");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let reason = || match channel_relay_status(&context, "general", true) {
            ChannelRelayStatus::LocalOnly(reason) => reason,
            _ => panic!("a channel send without a usable config is local only"),
        };
        assert_eq!(reason(), "no bridge config");
        let config = root.join("bridge").join("config.json");
        fs::create_dir_all(&config).expect("config as a directory");
        let unusable = reason();
        assert!(
            unusable.contains("not a regular file") && unusable != "no bridge config",
            "{unusable}"
        );
        trash_test_root(&root);
    }
}
