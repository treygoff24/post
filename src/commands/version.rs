use crate::command_result::CommandResult;
use crate::error::AppResult;
use serde::Serialize;

pub(crate) const CAPABILITIES: [&str; 1] = ["participants"];

#[derive(Serialize)]
struct VersionOutput {
    ok: bool,
    version: &'static str,
    build_sha: &'static str,
    store_version: u64,
    capabilities: &'static [&'static str],
}

pub(super) fn run(json: bool, pretty: bool) -> AppResult<CommandResult> {
    let output = VersionOutput {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        build_sha: option_env!("POST_BUILD_SHA").unwrap_or("unknown"),
        store_version: 1,
        capabilities: &CAPABILITIES,
    };
    if json {
        CommandResult::json(&output, pretty)
    } else {
        Ok(CommandResult::success(format!(
            "post {} (build {}, store v1; {})\n",
            output.version,
            output.build_sha,
            CAPABILITIES.join(",")
        )))
    }
}
