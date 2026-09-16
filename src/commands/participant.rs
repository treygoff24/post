use crate::cli::{ParticipantArgs, ParticipantCommand};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use crate::participant::{self, Participant, Resolved};
use serde::Serialize;

#[derive(Serialize)]
struct ParticipantOutput {
    ok: bool,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    participant: Option<Participant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provenance: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<&'static str>,
}

#[derive(Serialize)]
struct ParticipantListOutput {
    ok: bool,
    participants: Vec<Participant>,
    count: usize,
}

pub(super) fn run(
    context: &Context,
    args: ParticipantArgs,
    json: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    match args.command {
        ParticipantCommand::Show => show(context, pretty),
        ParticipantCommand::Bind(args) => {
            if args.harness.is_some() && args.key.is_none() && !args.fresh {
                return Err(AppError::invalid_argument(
                    "--harness is valid only with --key or --new",
                ));
            }
            let fresh_key = args.fresh.then(participant::fresh_uuid_key).transpose()?;
            let fresh_harness = if args.fresh {
                match args.harness.clone() {
                    Some(harness) => Some(harness),
                    None => Some(
                        std::env::var_os("POST_HARNESS")
                            .map(|value| {
                                value.into_string().map_err(|_| {
                                    AppError::invalid_argument(
                                        "POST_HARNESS is set but is not valid UTF-8",
                                    )
                                })
                            })
                            .transpose()?
                            .unwrap_or_else(|| "shell".to_owned()),
                    ),
                }
            } else {
                None
            };
            let bootstrap = match (args.key.as_deref(), fresh_key.as_deref()) {
                (Some(key), None) => Some((
                    args.harness
                        .as_deref()
                        .expect("clap requires --harness with --key"),
                    key,
                )),
                (None, Some(key)) => Some((
                    fresh_harness
                        .as_deref()
                        .expect("--new resolves a harness label"),
                    key,
                )),
                (None, None) => None,
                (Some(_), Some(_)) => unreachable!("clap makes --key and --new exclusive"),
            };
            let cwd = std::env::current_dir().map_err(|error| {
                AppError::io(
                    "resolve current directory for participant bind",
                    ".".as_ref(),
                    error,
                )
            })?;
            let participant =
                participant::bind(context, &cwd, args.workspace.as_deref(), bootstrap)?;
            if bootstrap.is_some() && !json {
                return Ok(CommandResult::success(format!(
                    "export POST_PARTICIPANT={}\n",
                    participant.id
                )));
            }
            let provenance = participant::resolve(context)?
                .provenance()
                .map(|p| p.as_str());
            let id = participant.id.clone();
            CommandResult::json(
                &ParticipantOutput {
                    ok: true,
                    status: "bound",
                    id: Some(id),
                    participant: Some(participant),
                    provenance,
                    fix: None,
                },
                pretty,
            )
        }
        ParticipantCommand::List => {
            let participants = participant::list(context)?;
            let count = participants.len();
            CommandResult::json(
                &ParticipantListOutput {
                    ok: true,
                    participants,
                    count,
                },
                pretty,
            )
        }
    }
}

fn show(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let output = match participant::resolve(context)? {
        Resolved::Bound {
            participant,
            provenance,
        } => ParticipantOutput {
            ok: true,
            status: "bound",
            id: Some(participant.id.clone()),
            participant: Some(*participant),
            provenance: Some(provenance.as_str()),
            fix: None,
        },
        Resolved::Unbound => ParticipantOutput {
            ok: true,
            status: "unbound",
            id: None,
            participant: None,
            provenance: None,
            fix: Some("run: post participant bind"),
        },
    };
    CommandResult::json(&output, pretty)
}
