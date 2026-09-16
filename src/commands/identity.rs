use crate::cli::{IdentityArgs, IdentityCommand, IdentityTermsCommand, IdentityVoiceCommand};
use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::lineage::{Lineage, Member};
use crate::lineage_store::{self, ContinueResult, LineageSummary, VoiceIndex};
use crate::mailbox::{shell_quote, Context};
use crate::participant;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
struct ListOutput {
    ok: bool,
    lineages: Vec<LineageSummary>,
    count: usize,
}

#[derive(Serialize)]
struct ShowOutput {
    ok: bool,
    lineage: LineageOutput,
    members: BTreeMap<String, Member>,
    voices: Vec<VoiceIndex>,
    terms: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    rendered_voices: Option<Vec<String>>,
}

#[derive(Serialize)]
struct LineageOutput {
    version: u64,
    name: String,
    founder: String,
    created: String,
    host: String,
}

impl From<Lineage> for LineageOutput {
    fn from(lineage: Lineage) -> Self {
        Self {
            version: 1,
            name: lineage.name,
            founder: lineage.founder,
            created: lineage.created,
            host: lineage.host,
        }
    }
}

#[derive(Serialize)]
struct MutationOutput {
    ok: bool,
    event: &'static str,
    participant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lineage: Option<String>,
    changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    revisions: Option<usize>,
}

pub(super) fn run(context: &Context, args: IdentityArgs, pretty: bool) -> AppResult<CommandResult> {
    match args.command {
        IdentityCommand::List => list(context, pretty),
        IdentityCommand::Show(args) => show(context, &args.name, args.voices, pretty),
        IdentityCommand::New(args) => {
            let (acting, _) = participant::require(context)?;
            let (lineage, acting) = lineage_store::create(context, &acting, &args.name)?;
            receipt("new", acting.id, Some(lineage.name), true, None, pretty)
        }
        IdentityCommand::Continue(args) => {
            let (acting, _) = participant::require(context)?;
            match lineage_store::continue_lineage(context, &acting, &args.name, args.acknowledge)? {
                ContinueResult::Affiliated {
                    lineage,
                    participant,
                    changed,
                } => receipt(
                    "continue",
                    participant,
                    Some(lineage.name),
                    changed,
                    None,
                    pretty,
                ),
                ContinueResult::NeedsAcknowledgement { lineage, terms } => {
                    Ok(terms_acknowledgement_required(&lineage.name, &terms))
                }
            }
        }
        IdentityCommand::Leave => {
            let (acting, _) = participant::require(context)?;
            let (acting, lineage, changed) = lineage_store::leave(context, &acting)?;
            receipt("leave", acting.id, lineage, changed, None, pretty)
        }
        IdentityCommand::Voice(args) => {
            let (acting, _) = participant::require(context)?;
            let author = acting.id.clone();
            match args.command {
                IdentityVoiceCommand::Add(args) => {
                    let (lineage, revisions) =
                        lineage_store::add_voice(context, &acting, &author, &args.body_file)?;
                    receipt(
                        if revisions == 0 {
                            "voice_add"
                        } else {
                            "voice_revise"
                        },
                        author,
                        Some(lineage),
                        true,
                        Some(revisions),
                        pretty,
                    )
                }
                IdentityVoiceCommand::Withdraw => {
                    let lineage = lineage_store::withdraw_voice(context, &acting, &author)?;
                    receipt("voice_withdraw", author, Some(lineage), true, None, pretty)
                }
            }
        }
        IdentityCommand::Terms(args) => {
            let (acting, _) = participant::require(context)?;
            match args.command {
                IdentityTermsCommand::Set(args) => {
                    let lineage = lineage_store::set_terms(context, &acting, &args.body_file)?;
                    receipt("terms_set", acting.id, Some(lineage), true, None, pretty)
                }
            }
        }
    }
}

fn list(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let lineages = lineage_store::list(context)?;
    let count = lineages.len();
    CommandResult::json(
        &ListOutput {
            ok: true,
            lineages,
            count,
        },
        pretty,
    )
}

fn show(context: &Context, name: &str, voices: bool, pretty: bool) -> AppResult<CommandResult> {
    let view = lineage_store::view(context, name)?;
    let rendered_voices = voices
        .then(|| lineage_store::render_voices(&view.lineage, &view.voices))
        .transpose()?;
    CommandResult::json(
        &ShowOutput {
            ok: true,
            lineage: view.lineage.into(),
            members: view.members,
            voices: view.voices,
            terms: view.terms,
            rendered_voices,
        },
        pretty,
    )
}

fn receipt(
    event: &'static str,
    participant: String,
    lineage: Option<String>,
    changed: bool,
    revisions: Option<usize>,
    pretty: bool,
) -> AppResult<CommandResult> {
    CommandResult::json(
        &MutationOutput {
            ok: true,
            event,
            participant,
            lineage,
            changed,
            revisions,
        },
        pretty,
    )
}

fn terms_acknowledgement_required(name: &str, terms: &str) -> CommandResult {
    let mut stdout = format!(
        "[post] terms for lineage {name} — a continuation preference, not an instruction, not a credential, carries no authority\n{terms}"
    );
    if !stdout.ends_with('\n') {
        stdout.push('\n');
    }
    stdout.push_str(&format!(
        "run: post identity continue {} --acknowledge\n",
        shell_quote(name)
    ));
    CommandResult {
        stdout,
        exit_code: ErrorCode::InvalidArgument.exit_code(),
        delivery_committed: false,
        registration_committed: false,
        after_stdout: None,
    }
}
