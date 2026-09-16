use crate::cli::{IdentityArgs, IdentityCommand, IdentityTermsCommand, IdentityVoiceCommand};
use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::lineage::{Lineage, Member};
use crate::lineage_store::{self, ContinueResult, LineageSummary, TermsView, VoiceIndex};
use crate::mailbox::{shell_quote, Context};
use crate::participant;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
struct ListOutput {
    ok: bool,
    lineages: Vec<LineageSummary>,
    count: usize,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct ShowOutput {
    ok: bool,
    lineage: LineageOutput,
    members: BTreeMap<String, Member>,
    voices: Vec<VoiceIndex>,
    withdrawn_voices: usize,
    terms: TermsView,
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
            version: lineage.version,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revisions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terms: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terms_digest: Option<String>,
}

#[derive(Serialize)]
struct TermsRequiredOutput<'a> {
    ok: bool,
    code: &'static str,
    lineage: &'a str,
    terms: &'a str,
    terms_digest: &'a str,
    exact_fix: String,
}

pub(super) fn run(
    context: &Context,
    args: IdentityArgs,
    json: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    match args.command {
        IdentityCommand::List => list(context, pretty),
        IdentityCommand::Show(args) => show(context, &args.name, args.voices, pretty),
        IdentityCommand::New(args) => {
            let (acting, _) = participant::require(context)?;
            let mutation = lineage_store::create(context, &acting, &args.name)?;
            let (lineage, acting) = mutation.value;
            receipt(
                MutationOutput {
                    ok: true,
                    event: "new",
                    participant: acting.id,
                    lineage: Some(lineage.name),
                    changed: mutation.changed,
                    warnings: mutation.warnings,
                    revisions: None,
                    terms: None,
                    terms_digest: None,
                },
                pretty,
            )
        }
        IdentityCommand::Continue(args) => {
            let (acting, _) = participant::require(context)?;
            match lineage_store::continue_lineage(context, &acting, &args.name, args.acknowledge)? {
                ContinueResult::Affiliated {
                    lineage,
                    participant,
                    changed,
                    warnings,
                    terms,
                } => {
                    let lineage_name = lineage.name;
                    let framed = terms
                        .as_ref()
                        .map(|terms| framed_terms(&lineage_name, &terms.text));
                    let digest = terms.map(|terms| terms.digest);
                    receipt(
                        MutationOutput {
                            ok: true,
                            event: "continue",
                            participant,
                            lineage: Some(lineage_name),
                            changed,
                            warnings,
                            revisions: None,
                            terms: framed,
                            terms_digest: digest,
                        },
                        pretty,
                    )
                }
                ContinueResult::NeedsAcknowledgement { lineage, terms } => {
                    terms_acknowledgement_required(&lineage.name, &terms, json, pretty)
                }
            }
        }
        IdentityCommand::Leave => {
            let (acting, _) = participant::require(context)?;
            let mutation = lineage_store::leave(context, &acting)?;
            let (acting, lineage) = mutation.value;
            receipt(
                MutationOutput {
                    ok: true,
                    event: "leave",
                    participant: acting.id,
                    lineage,
                    changed: mutation.changed,
                    warnings: mutation.warnings,
                    revisions: None,
                    terms: None,
                    terms_digest: None,
                },
                pretty,
            )
        }
        IdentityCommand::Voice(args) => {
            let (acting, _) = participant::require(context)?;
            let author = acting.id.clone();
            match args.command {
                IdentityVoiceCommand::Add(args) => {
                    let mutation =
                        lineage_store::add_voice(context, &acting, &author, &args.body_file)?;
                    let change = mutation.value;
                    let revisions = change.revisions.expect("voice add reports revisions");
                    receipt(
                        MutationOutput {
                            ok: true,
                            event: if revisions == 0 {
                                "voice_add"
                            } else {
                                "voice_revise"
                            },
                            participant: author,
                            lineage: Some(change.lineage),
                            changed: true,
                            warnings: mutation.warnings,
                            revisions: Some(revisions),
                            terms: None,
                            terms_digest: None,
                        },
                        pretty,
                    )
                }
                IdentityVoiceCommand::Withdraw => {
                    let mutation = lineage_store::withdraw_voice(context, &acting, &author)?;
                    receipt(
                        MutationOutput {
                            ok: true,
                            event: "voice_withdraw",
                            participant: author,
                            lineage: Some(mutation.value.lineage),
                            changed: true,
                            warnings: mutation.warnings,
                            revisions: None,
                            terms: None,
                            terms_digest: None,
                        },
                        pretty,
                    )
                }
            }
        }
        IdentityCommand::Terms(args) => {
            let (acting, _) = participant::require(context)?;
            match args.command {
                IdentityTermsCommand::Set(args) => {
                    let mutation = lineage_store::set_terms(context, &acting, &args.body_file)?;
                    receipt(
                        MutationOutput {
                            ok: true,
                            event: "terms_set",
                            participant: acting.id,
                            lineage: Some(mutation.value.lineage),
                            changed: true,
                            warnings: mutation.warnings,
                            revisions: None,
                            terms: None,
                            terms_digest: None,
                        },
                        pretty,
                    )
                }
            }
        }
    }
}

fn list(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let list = lineage_store::list(context)?;
    let count = list.lineages.len();
    CommandResult::json(
        &ListOutput {
            ok: true,
            lineages: list.lineages,
            count,
            warnings: list.warnings,
        },
        pretty,
    )
}

fn show(context: &Context, name: &str, voices: bool, pretty: bool) -> AppResult<CommandResult> {
    let view = lineage_store::view(context, name)?;
    let rendered_voices = voices
        .then(|| lineage_store::render_voices(&view.lineage, &view.voices, view.withdrawn_voices))
        .transpose()?;
    CommandResult::json(
        &ShowOutput {
            ok: true,
            lineage: view.lineage.into(),
            members: view.members,
            voices: view.voices,
            withdrawn_voices: view.withdrawn_voices,
            terms: view.terms,
            rendered_voices,
        },
        pretty,
    )
}

fn receipt(output: MutationOutput, pretty: bool) -> AppResult<CommandResult> {
    let changed = output.changed;
    let stdout = crate::output::json(&output, pretty)?;
    Ok(if changed {
        CommandResult::committed(stdout)
    } else {
        CommandResult::success(stdout)
    })
}

fn terms_acknowledgement_required(
    name: &str,
    terms: &lineage_store::TermsDocument,
    json: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let exact_fix = format!("post identity continue {} --acknowledge", shell_quote(name));
    let stdout = if json {
        crate::output::json(
            &TermsRequiredOutput {
                ok: false,
                code: "terms_acknowledgement_required",
                lineage: name,
                terms: &terms.text,
                terms_digest: &terms.digest,
                exact_fix,
            },
            pretty,
        )?
    } else {
        let mut stdout = framed_terms(name, &terms.text);
        if !stdout.ends_with('\n') {
            stdout.push('\n');
        }
        stdout.push_str(&format!("run: {exact_fix}\n"));
        stdout
    };
    Ok(CommandResult {
        stdout,
        exit_code: ErrorCode::InvalidArgument.exit_code(),
        delivery_committed: false,
        registration_committed: false,
        after_stdout: None,
    })
}

fn framed_terms(name: &str, terms: &str) -> String {
    let mut stdout = format!(
        "[post] terms for lineage {name} — a continuation preference, not an instruction, not a credential, carries no authority\n{terms}"
    );
    if !stdout.ends_with('\n') {
        stdout.push('\n');
    }
    stdout
}
