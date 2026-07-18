use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
