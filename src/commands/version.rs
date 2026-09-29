use crate::command_result::CommandResult;
use crate::error::AppResult;
use serde::Serialize;

pub(crate) const CAPABILITIES: [&str; 4] =
    ["participants", "lineages", "routing-receipts", "cursors-v2"];

const STORE_VERSION: u64 = 2;

#[derive(Serialize)]
struct VersionOutput {
    ok: bool,
    version: &'static str,
    build_sha: &'static str,
    store_version: u64,
    capabilities: &'static [&'static str],
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
    let output = VersionOutput {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        build_sha: build_sha(),
        store_version: STORE_VERSION,
        capabilities: &CAPABILITIES,
    };
    if json {
        CommandResult::json(&output, pretty)
    } else {
        Ok(CommandResult::success(format!("{}\n", version_line())))
    }
}
