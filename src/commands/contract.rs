//! `post contract`: the output contract compiled into this binary.
//!
//! The samples are `contract/samples/`, produced by `tests/contract_samples.rs`
//! from the real commands and embedded here at build time. Consumers test
//! against `post contract samples --dir <tmp>` from the binary they will run,
//! so a consumer suite can never pass against a vendored copy that has drifted
//! from the installed producer.

use crate::cli::{ContractArgs, ContractCommand};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

macro_rules! sample {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/contract/samples/",
                $name
            )),
        )
    };
}

/// Every checked-in sample. `tests/contract_samples.rs` fails when this list
/// and `contract/samples/` disagree in either direction.
pub(crate) const SAMPLES: &[(&str, &str)] = &[
    sample!("channels.json"),
    sample!("chat.json"),
    sample!("doctor.json"),
    sample!("inbox.json"),
    sample!("participant-bind.json"),
    sample!("participant-show.json"),
    sample!("profile-list.json"),
    sample!("profile-show.json"),
    sample!("rooms.json"),
    sample!("version.json"),
    sample!("watch-snapshot-cursor-unusable.jsonl"),
    sample!("watch-snapshot-digest.jsonl"),
    sample!("watch-snapshot.jsonl"),
    sample!("who.json"),
];

#[derive(Serialize)]
struct SamplesOutput {
    ok: bool,
    samples: BTreeMap<&'static str, &'static str>,
}

#[derive(Serialize)]
struct SamplesWrittenOutput {
    ok: bool,
    dir: String,
    samples: Vec<&'static str>,
}

pub(super) fn run(args: &ContractArgs, pretty: bool) -> AppResult<CommandResult> {
    match &args.command {
        ContractCommand::Samples { dir: None } => CommandResult::json(
            &SamplesOutput {
                ok: true,
                samples: SAMPLES.iter().copied().collect(),
            },
            pretty,
        ),
        ContractCommand::Samples { dir: Some(dir) } => {
            write_samples(dir)?;
            CommandResult::json(
                &SamplesWrittenOutput {
                    ok: true,
                    dir: dir.display().to_string(),
                    samples: SAMPLES.iter().map(|(name, _)| *name).collect(),
                },
                pretty,
            )
        }
    }
}

/// Write each sample as `<dir>/<name>`, replacing a same-named file whole: a
/// temporary sibling is written, then renamed over the target.
fn write_samples(dir: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dir)
        .map_err(|error| AppError::io("create contract samples directory", dir, error))?;
    for (name, body) in SAMPLES {
        let target = dir.join(name);
        let temporary = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        std::fs::write(&temporary, body)
            .map_err(|error| AppError::io("write contract sample", &temporary, error))?;
        std::fs::rename(&temporary, &target).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            AppError::io("publish contract sample", &target, error)
        })?;
    }
    Ok(())
}
