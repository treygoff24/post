use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailKind {
    Letter,
    Note,
    Signal,
}

impl MailKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Letter => "letter",
            Self::Note => "note",
            Self::Signal => "signal",
        }
    }
}

impl std::fmt::Display for MailKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: MailKind,
    pub subject: String,
    pub sent: String,
    /// Sender's display name as of send time (presentation only; identity
    /// is always `from`). Absent when the sender had no profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Sender's emoji sigil as of send time; same rules as display_name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp: Option<String>,
    /// Opaque per-launch instance address (`harness.repo.uuid`), recorded
    /// verbatim from POST_SENDER_ADDRESS when the launch helper exported it.
    /// Non-routable, self-declared, never synthesized by post. Absent on old
    /// mail and on sends outside the helper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_address: Option<String>,
    /// How the `from` field was determined at send time: `declared-env`,
    /// `declared-flag`, `inferred-cwd`, or `inferred-basename`. Evidence, not
    /// a credential — kept as a plain string so an unknown future value can
    /// never break message parse (the signature_ref lesson). Renderers only
    /// speak the four known values; anything else renders silence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_provenance: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ParsedMail {
    pub envelope: Envelope,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingRule {
    pub from: String,
    pub to: String,
    pub reason: String,
}

impl BlockingRule {
    pub(crate) fn matches_route(&self, sender: &str, recipient: &str) -> bool {
        (self.from == "*" || self.from == sender) && self.targets(recipient)
    }

    pub(crate) fn targets(&self, recipient: &str) -> bool {
        self.to == "*" || self.to == recipient
    }
}

/// A channel message is NOT mail: it has no kind (the kinds law stays
/// untouched, and kind=signal structurally cannot occur in a channel) and
/// no single recipient. Contract: mail 20260722-013246 (pinned) as amended
/// by 013434 (microsecond ids).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: String,
    pub from: String,
    pub channel: String,
    #[serde(default)]
    pub subject: String,
    pub sent: String,
    /// "join" on membership events; absent on ordinary messages. Set only
    /// by the CLI's join path — sends never carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Sender's display name as of send time (presentation only; identity
    /// is always `from`). Absent when the sender had no profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Sender's emoji sigil as of send time; same rules as display_name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp: Option<String>,
    /// Prior message id this replies to (threads-lite). Absent on ordinary
    /// messages; old stores without the field keep reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    /// Registered room names @mentioned in the body (word-boundary match).
    /// Absent/empty on old messages and on bodies with no mentions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    /// Signed-message-v2 locator: `{"version": 2, "tag": "<ts>"}`. Kept as a
    /// raw JSON value ON PURPOSE — a malformed hand-written owner locator
    /// must leave the message readable so verification can render SIGNATURE
    /// FAILED loudly; a strictly typed field would fail the whole .msg parse
    /// and silently drop the message instead. The custom deserializer keeps
    /// a PRESENT `null` as Some(Null) — plain Option would fold it into
    /// None and an owner message stamped `"signature_ref": null` would
    /// silently read as unsigned instead of failing loudly (Sol's review
    /// catch, 20260812-210155). Sender-writable transport metadata:
    /// presence is never a credential, authority is computed only at read
    /// time (v1 doctrine unchanged). Ignored for every room but the
    /// configured owner's.
    #[serde(
        default,
        deserialize_with = "deserialize_present_json",
        skip_serializing_if = "Option::is_none"
    )]
    pub signature_ref: Option<serde_json::Value>,
    /// Opaque per-launch instance address; same contract as
    /// `Envelope::sender_address`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_address: Option<String>,
    /// Evidence for how `from` was determined; same contract as
    /// `Envelope::sender_provenance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_provenance: Option<String>,
}

/// How a sender's `from` was determined at send time. All four are
/// self-declared evidence about the send, never a credential; authority
/// remains the signature layer's alone. Serialized as the kebab-case string
/// in the envelope's `sender_provenance` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SenderProvenance {
    /// `from` came from the POST_FROM environment pin (launch helper).
    DeclaredEnv,
    /// `from` was set explicitly with `--from`.
    DeclaredFlag,
    /// `from` was inferred from cwd falling inside a registered room's tree.
    InferredCwd,
    /// `from` fell back to the cwd basename (no registered room matched).
    InferredBasename,
}

impl SenderProvenance {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DeclaredEnv => "declared-env",
            Self::DeclaredFlag => "declared-flag",
            Self::InferredCwd => "inferred-cwd",
            Self::InferredBasename => "inferred-basename",
        }
    }
}

/// Present key (any value, including null) → Some; absent key → the field's
/// #[serde(default)] None. This is what distinguishes "no locator" from
/// "a locator that is garbage" — the latter must stay visible to fail loudly.
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[derive(Debug)]
pub(crate) struct ParsedChannelMessage {
    pub message: ChannelMessage,
    #[allow(dead_code)] // consumed by the read/cursor lane's patch
    pub body: String,
}

pub(crate) type RoomMap = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RulesConfig {
    pub blocked: Vec<BlockingRule>,
}
