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
}

#[derive(Debug)]
pub struct ParsedMail {
    pub envelope: Envelope,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingRule {
    pub from: String,
    pub to: String,
    pub reason: String,
}

pub type RoomMap = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesConfig {
    pub blocked: Vec<BlockingRule>,
}
