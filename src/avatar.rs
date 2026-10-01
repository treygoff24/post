//! Porch format-1 packs and frozen emote payloads. Presentation only.
use crate::error::{AppError, ErrorCode};
use crate::mailbox::Context;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;

pub(crate) const INPUT_MAX: usize = 32768;
const STANDARD: &[&str] = &["idle", "talk", "wave", "think", "celebrate", "sleep"];
const MOTIONS: &[&str] = &["hop", "shake", "flip", "blink", "none"];
const PARTICLES: &[&str] = &["heart", "spark", "zzz", "question", "exclaim", "none"];
type Rules = BTreeSet<&'static str>;

// Deserialize objects ourselves: serde_json's Value silently replaces duplicate keys.
pub(crate) struct Unique(pub(crate) Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(
                    serde_json::Number::from_f64(v)
                        .ok_or_else(|| E::custom("invalid number"))?
                        .into(),
                ))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
                let mut items = Vec::new();
                while let Some(Unique(item)) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Unique(items.into()))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut items = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if items.contains_key(&key) {
                        return Err(de::Error::custom("duplicate-key"));
                    }
                    let Unique(item) = map.next_value()?;
                    items.insert(key, item);
                }
                Ok(Unique(items.into()))
            }
        }
        d.deserialize_any(V)
    }
}

pub(crate) fn canonical(value: &Value) -> Vec<u8> {
    fn normalize(v: &Value) -> Value {
        match v {
            Value::Number(n) if n.as_f64().is_some_and(|f| f.fract() == 0.0) => {
                if let Some(i) = n.as_i64() {
                    i.into()
                } else if let Some(u) = n.as_u64() {
                    u.into()
                } else {
                    (n.as_f64().unwrap() as i64).into()
                }
            }
            Value::Object(m) => {
                Value::Object(m.iter().map(|(k, v)| (k.clone(), normalize(v))).collect())
            }
            Value::Array(a) => a.iter().map(normalize).collect(),
            _ => v.clone(),
        }
    }
    serde_json::to_vec(&normalize(value)).expect("JSON value serializes")
}

pub(crate) fn name_valid(n: &str) -> bool {
    (1..=24).contains(&n.len())
        && n.as_bytes()[0].is_ascii_lowercase()
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn pixel(b: u8) -> bool {
    b == b'.' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}
fn object<'a>(v: &'a Value, r: &mut Rules) -> Option<&'a Map<String, Value>> {
    if v.is_null() {
        r.insert("null-value");
        None
    } else if let Some(m) = v.as_object() {
        Some(m)
    } else {
        r.insert("type-mismatch");
        None
    }
}
fn string<'a>(v: &'a Value, r: &mut Rules) -> Option<&'a str> {
    if v.is_null() {
        r.insert("null-value");
        None
    } else if let Some(s) = v.as_str() {
        Some(s)
    } else {
        r.insert("type-mismatch");
        None
    }
}
fn members(m: &Map<String, Value>, required: &[&str], allowed: &[&str], r: &mut Rules) {
    if required.iter().any(|k| !m.contains_key(*k)) {
        r.insert("missing-field");
    }
    if m.keys().any(|k| !allowed.contains(&k.as_str())) {
        r.insert("unknown-field");
    }
}
fn integer(v: &Value, r: &mut Rules) -> Option<f64> {
    if v.is_null() {
        r.insert("null-value");
        return None;
    }
    let Some(n) = v.as_f64() else {
        r.insert("type-mismatch");
        return None;
    };
    if n.fract() != 0.0 {
        r.insert("not-integer");
        return None;
    }
    Some(n)
}
fn frames(v: &Value, size: usize, limit: usize, r: &mut Rules) {
    let Some(m) = object(v, r) else {
        return;
    };
    if !m.contains_key("idle") {
        r.insert("missing-field");
    }
    if m.len() > limit {
        r.insert(if size == 16 {
            "body-frame-count"
        } else {
            "head-frame-count"
        });
    }
    for (name, frame) in m {
        if !name_valid(name) {
            r.insert("frame-name-grammar");
            continue;
        }
        if frame.is_null() {
            r.insert("null-value");
            continue;
        }
        let Some(rows) = frame.as_array() else {
            r.insert("type-mismatch");
            continue;
        };
        let size_rule = if size == 16 {
            "body-frame-size"
        } else {
            "head-frame-size"
        };
        if rows.len() != size {
            r.insert(size_rule);
        }
        for row in rows {
            let Some(row) = string(row, r) else {
                continue;
            };
            if row.chars().count() != size {
                r.insert(size_rule);
                continue;
            }
            if !row.bytes().all(pixel) {
                r.insert("pixel-char");
            }
        }
    }
}
fn steps(v: &Value, body: Option<&Map<String, Value>>, r: &mut Rules) {
    if v.is_null() {
        r.insert("null-value");
        return;
    }
    let Some(a) = v.as_array() else {
        r.insert("type-mismatch");
        return;
    };
    if !(1..=16).contains(&a.len()) {
        r.insert("emote-step-count");
    }
    let mut duration = Some(0.0);
    for step in a {
        let Some(m) = object(step, r) else {
            duration = None;
            continue;
        };
        members(m, &["pose", "ms"], &["pose", "ms", "motion", "particle"], r);
        if let Some(pose) = m.get("pose").and_then(|v| string(v, r)) {
            if !name_valid(pose) {
                r.insert("frame-name-grammar");
            } else if let Some(body) = body {
                if !body.contains_key(pose) && !STANDARD.contains(&pose) {
                    r.insert("emote-pose-unknown");
                }
            }
        }
        for (key, set, rule) in [
            ("motion", MOTIONS, "step-motion"),
            ("particle", PARTICLES, "step-particle"),
        ] {
            if let Some(s) = m.get(key).and_then(|v| string(v, r)) {
                if !set.contains(&s) {
                    r.insert(rule);
                }
            }
        }
        match m.get("ms").and_then(|v| integer(v, r)) {
            Some(ms) if (60.0..=2000.0).contains(&ms) => {
                duration = duration.map(|d| d + ms);
            }
            Some(_) => {
                r.insert("step-ms-range");
                duration = None;
            }
            None => duration = None,
        }
    }
    if duration.is_some_and(|d| d > 4000.0) {
        r.insert("emote-duration");
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Value, Vec<&'static str>> {
    if bytes.len() > INPUT_MAX {
        return Err(vec!["input-too-large"]);
    }
    let v = serde_json::from_slice::<Unique>(bytes)
        .map_err(|e| {
            vec![if e.to_string().contains("duplicate-key") {
                "duplicate-key"
            } else {
                "json-syntax"
            }]
        })?
        .0;
    let mut r = Rules::new();
    if let Some(m) = object(&v, &mut r) {
        members(
            m,
            &["format", "accent", "body", "head"],
            &["format", "accent", "body", "head", "emotes"],
            &mut r,
        );
        if let Some(n) = m.get("format").and_then(|v| integer(v, &mut r)) {
            if n != 1.0 {
                r.insert("format-unsupported");
            }
        }
        if let Some(s) = m.get("accent").and_then(|v| string(v, &mut r)) {
            if s.len() != 1
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                r.insert("accent-grammar");
            }
        }
        if let Some(b) = m.get("body") {
            frames(b, 16, 16, &mut r);
        }
        if let Some(h) = m.get("head") {
            frames(h, 8, 8, &mut r);
        }
        if let Some(emotes) = m.get("emotes").and_then(|v| object(v, &mut r)) {
            if emotes.len() > 16 {
                r.insert("emote-count");
            }
            for (name, emote) in emotes {
                if !name_valid(name) {
                    r.insert("emote-name-grammar");
                    continue;
                }
                if let Some(e) = object(emote, &mut r) {
                    members(e, &["steps"], &["steps"], &mut r);
                    if let Some(s) = e.get("steps") {
                        steps(s, m.get("body").and_then(Value::as_object), &mut r);
                    }
                }
            }
        }
    }
    if r.is_empty() {
        if let Some(emotes) = v.get("emotes").and_then(Value::as_object) {
            for e in emotes.values() {
                if canonical(&freeze_steps(&v, &e["steps"])).len() > 1280 {
                    r.insert("emote-freeze-too-large");
                }
            }
        }
        if canonical(&v).len() > 16384 {
            r.insert("canonical-too-large");
        }
    }
    if r.is_empty() {
        Ok(v)
    } else {
        Err(r.into_iter().collect())
    }
}

fn freeze_steps(pack: &Value, steps: &Value) -> Value {
    let mut body = Map::new();
    let mut head = Map::new();
    for step in steps.as_array().expect("validated steps") {
        let pose = step["pose"].as_str().expect("validated pose");
        for (kind, dest) in [("body", &mut body), ("head", &mut head)] {
            let name = if pack[kind].get(pose).is_some() {
                pose
            } else {
                "idle"
            };
            let rows: Vec<&str> = pack[kind][name]
                .as_array()
                .expect("validated frame")
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            dest.insert(name.to_owned(), rows.join("/").into());
        }
    }
    json!({"frames": {"body": body, "head": head}, "steps": steps})
}
pub(crate) fn builtins() -> Value {
    serde_json::from_str(include_str!(
        "../tests/fixtures/porch-contract/emotes/builtin-1.json"
    ))
    .expect("frozen builtins")
}
pub(crate) fn freeze(pack: &Value, name: &str) -> Option<(&'static str, Value)> {
    if let Some(e) = pack.get("emotes").and_then(|e| e.get(name)) {
        return Some(("custom", freeze_steps(pack, &e["steps"])));
    }
    let table = builtins();
    table["emotes"]
        .get(name)
        .map(|e| ("builtin", freeze_steps(pack, &e["steps"])))
}

pub(crate) fn read_bounded(reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((INPUT_MAX + 1) as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}
pub(crate) fn load(context: &Context, id: &str) -> (Option<Value>, Vec<String>) {
    let path = context.root.join("avatars").join(format!("{id}.json"));
    let result = match File::open(&path) {
        Ok(file) => read_bounded(file)
            .map_err(|e| e.to_string())
            .and_then(|b| parse(&b).map_err(|rules| rules.join(","))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (None, Vec::new()),
        Err(e) => Err(e.to_string()),
    };
    match result {
        Ok(pack) => (Some(pack), Vec::new()),
        Err(reason) => (None, vec![format!("invalid avatar for {id}: {reason}")]),
    }
}
pub(crate) fn invalid(rules: Vec<&str>) -> AppError {
    let mut e = AppError::new(
        ErrorCode::InvalidArgument,
        "invalid avatar pack",
        "Use a format-1 avatar pack satisfying the reported rules.",
    )
    .reason(rules.join(","));
    e.details.rules = Some(rules.into_iter().map(str::to_owned).collect());
    e
}

/// Payload checks are independent of the current avatar or installed library.
pub(crate) fn payload_rule(v: Option<&Value>) -> Option<&'static str> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Some("payload-missing");
    };
    let Some(m) = v.as_object() else {
        return Some("type-mismatch");
    };
    if m.keys()
        .any(|k| !["name", "source", "library", "at", "steps", "frames"].contains(&k.as_str()))
    {
        return Some("payload-unknown-field");
    }
    if ["name", "source", "library", "steps", "frames"]
        .iter()
        .any(|k| !m.contains_key(*k))
    {
        return Some("missing-field");
    }
    for key in ["name", "source", "library", "at"] {
        if let Some(v) = m.get(key) {
            if !v.is_string() {
                return Some("type-mismatch");
            }
        }
    }
    if !name_valid(m["name"].as_str().unwrap()) {
        return Some("payload-name-grammar");
    }
    if !["custom", "builtin"].contains(&m["source"].as_str().unwrap()) {
        return Some("payload-source");
    }
    let library = m["library"].as_str().unwrap();
    if !library.strip_prefix("builtin-").is_some_and(|n| {
        !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
    }) {
        return Some("payload-library-grammar");
    }
    let mut r = Rules::new();
    steps(&m["steps"], None, &mut r);
    if let Some(rule) = r.into_iter().next() {
        return Some(match rule {
            "null-value" | "not-integer" => "type-mismatch",
            "unknown-field" => "payload-unknown-field",
            rule => rule,
        });
    }
    let Some(frames) = m["frames"].as_object() else {
        return Some("type-mismatch");
    };
    if frames.len() != 2 || !frames.contains_key("body") || !frames.contains_key("head") {
        return Some("payload-frames");
    }
    for (kind, size, limit) in [("body", 16, 16), ("head", 8, 8)] {
        let Some(map) = frames[kind].as_object() else {
            return Some("type-mismatch");
        };
        if !(1..=limit).contains(&map.len()) {
            return Some("payload-frames");
        }
        for (name, v) in map {
            if !name_valid(name) {
                return Some("frame-name-grammar");
            }
            let Some(s) = v.as_str() else {
                return Some("type-mismatch");
            };
            let rows: Vec<&str> = s.split('/').collect();
            if rows.len() != size
                || rows
                    .iter()
                    .any(|r| r.len() != size || !r.bytes().all(pixel))
            {
                return Some("payload-frame-string");
            }
        }
        for s in m["steps"].as_array().unwrap() {
            if !map.contains_key(s["pose"].as_str().unwrap()) && !map.contains_key("idle") {
                return Some("payload-pose-unresolved");
            }
        }
    }
    if canonical(&json!({"frames":m["frames"],"steps":m["steps"]})).len() > 1280 {
        return Some("payload-too-large");
    }
    None
}
