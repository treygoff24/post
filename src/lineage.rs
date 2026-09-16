use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use crate::participant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

pub(crate) const LINEAGES_DIR: &str = "lineages";
const LINEAGE_FILE: &str = "lineage.json";

const fn default_lineage_version() -> u64 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lineage {
    #[serde(default = "default_lineage_version")]
    pub version: u64,
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
    #[serde(default)]
    pub active: bool,
}

impl Lineage {
    /// Membership has one authority: participant.json. No members.json is
    /// read or written, so a crash cannot split affiliation across copies.
    #[allow(dead_code)] // public seam consumed by P.3
    pub(crate) fn members(&self, context: &Context) -> AppResult<BTreeMap<String, Member>> {
        let mut members = BTreeMap::new();
        let now = std::time::SystemTime::now();
        for participant in participant::list(context)? {
            if participant.lineage.as_deref() == Some(self.name.as_str()) {
                let active = participant.is_active(now);
                members.insert(
                    participant.id.clone(),
                    Member {
                        id: participant.id,
                        harness: participant.harness,
                        workspace: participant.workspace,
                        active,
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

#[cfg(test)]
mod tests {
    use super::Lineage;
    use crate::mailbox::Context;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    #[test]
    fn members_preserve_historical_affiliation_and_report_activity() {
        let root = test_root("lineage-active-members");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        for (id, lifecycle) in [
            (
                "test-active",
                serde_json::json!({
                    "last_seen": "2099-01-01T00:00:00Z",
                    "lease_hours": 24
                }),
            ),
            ("test-missing-lease", serde_json::json!({})),
            (
                "test-stale",
                serde_json::json!({
                    "last_seen": "2020-01-01T00:00:00Z",
                    "lease_hours": 1
                }),
            ),
            (
                "test-ended",
                serde_json::json!({
                    "last_seen": "2099-01-01T00:00:00Z",
                    "lease_hours": 24,
                    "ended_at": "2026-09-16T00:00:00Z"
                }),
            ),
        ] {
            let dir = root.join("participants").join(id);
            fs::create_dir_all(&dir).expect("participant dir");
            let mut record = serde_json::json!({
                "version": 1,
                "id": id,
                "harness": "test",
                "conversation_key_digest": "0".repeat(64),
                "created": "2026-09-16 00:00:00 +0000",
                "lineage": "ember"
            });
            record
                .as_object_mut()
                .expect("record object")
                .extend(lifecycle.as_object().expect("lifecycle object").clone());
            fs::write(
                dir.join("participant.json"),
                serde_json::to_vec_pretty(&record).expect("participant JSON"),
            )
            .expect("participant record");
        }
        let lineage = Lineage {
            name: "ember".to_owned(),
            founder: "test-active".to_owned(),
            created: "2026-09-16T00:00:00Z".to_owned(),
            host: "test".to_owned(),
            dir: root.join("lineages/ember"),
        };
        let members = lineage.members(&context).expect("lineage members");
        assert!(members["test-active"].active);
        assert!(!members["test-missing-lease"].active);
        assert!(!members["test-stale"].active);
        assert!(!members["test-ended"].active);
        assert_eq!(members.len(), 4, "inactive affiliation remains historical");
        trash_test_root(&root);
    }
}
