use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use serde::Serialize;

pub(crate) const CAPABILITIES: [&str; 6] = [
    "participants",
    "lineages",
    "routing-receipts",
    "cursors-v2",
    "avatars-v1",
    "emotes-v1",
];

const STORE_VERSION: u64 = 2;

#[derive(Serialize)]
struct VersionOutput {
    ok: bool,
    version: &'static str,
    build_sha: &'static str,
    store_version: u64,
    /// The store root this binary would use (`POST_MAIL_ROOT`, else
    /// `$HOME/.claude-mail`); null when the environment cannot resolve one.
    store: Option<String>,
    /// Why `store` is null; absent when it resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    store_error: Option<String>,
    capabilities: &'static [&'static str],
}

/// Resolve the store root the way every other command does, touching nothing:
/// `Context::from_env` only reads the environment.
fn resolved_store() -> (Option<String>, Option<String>) {
    match Context::from_env() {
        Ok(context) => (Some(context.root.display().to_string()), None),
        Err(error) => (None, Some(error.message)),
    }
}

fn build_sha() -> &'static str {
    option_env!("POST_BUILD_SHA").unwrap_or("unknown")
}

/// The one build line every version surface prints. `post --version` is
/// routed to `post version` in app.rs, so the two can never disagree. The
/// build id is the short commit, with `-dirty` when tracked files differed
/// from it at build time, so an installed binary is traceable to a commit.
fn version_line() -> String {
    format!(
        "post {} (build {}, store v{}; {})",
        env!("CARGO_PKG_VERSION"),
        build_sha(),
        STORE_VERSION,
        CAPABILITIES.join(",")
    )
}

pub(super) fn run(json: bool, pretty: bool) -> AppResult<CommandResult> {
    let (store, store_error) = resolved_store();
    let output = VersionOutput {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        build_sha: build_sha(),
        store_version: STORE_VERSION,
        store,
        store_error,
        capabilities: &CAPABILITIES,
    };
    if json {
        CommandResult::json(&output, pretty)
    } else {
        Ok(CommandResult::success(format!("{}\n", version_line())))
    }
}
