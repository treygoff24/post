use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use crate::participant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

pub(crate) const LINEAGES_DIR: &str = "lineages";
const LINEAGE_FILE: &str = "lineage.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lineage {
    pub name: String,
    pub founder: String,
    pub created: String,
    pub host: String,
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)] // public seam consumed by P.3
pub(crate) struct Member {
    pub id: String,
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl Lineage {
    /// Membership has one authority: participant.json. No members.json is
    /// read or written, so a crash cannot split affiliation across copies.
    #[allow(dead_code)] // public seam consumed by P.3
    pub(crate) fn members(&self, context: &Context) -> AppResult<BTreeMap<String, Member>> {
        let mut members = BTreeMap::new();
        for participant in participant::list(context)? {
            if participant.lineage.as_deref() == Some(self.name.as_str()) {
                members.insert(
                    participant.id.clone(),
                    Member {
                        id: participant.id,
                        harness: participant.harness,
                        workspace: participant.workspace,
                    },
                );
            }
        }
        Ok(members)
    }
}

pub(crate) fn load(context: &Context, name: &str) -> AppResult<Option<Lineage>> {
    crate::mailbox::validate_component(name).map_err(|reason| {
        AppError::invalid_argument(format!("lineage name '{name}' is invalid: {reason}"))
            .input(name)
            .reason(reason)
    })?;
    let dir = context.root.join(LINEAGES_DIR).join(name);
    let path = dir.join(LINEAGE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("read lineage record", &path, error)),
    };
    let mut lineage: Lineage = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(&path, format!("invalid lineage JSON: {error}")))?;
    if lineage.name != name {
        return Err(AppError::config(
            &path,
            format!(
                "lineage name '{}' does not match its directory '{name}'",
                lineage.name
            ),
        ));
    }
    lineage.dir = dir;
    Ok(Some(lineage))
}

#[allow(dead_code)] // creation seam consumed by P.3 identity new
pub(crate) fn validate_name(context: &Context, name: &str) -> AppResult<()> {
    crate::mailbox::validate_new_room_name(name).map_err(|reason| {
        AppError::invalid_argument(format!("lineage name '{name}' is invalid: {reason}"))
            .input(name)
            .reason(reason)
    })?;
    if context.load_rooms()?.contains_key(name) {
        return Err(AppError::invalid_argument(format!(
            "lineage name '{name}' is already a registered workspace room"
        ))
        .input(name)
        .reason("lineage names cannot collide with registered rooms"));
    }
    Ok(())
}
