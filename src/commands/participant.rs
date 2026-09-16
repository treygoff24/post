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
    pretty: bool,
) -> AppResult<CommandResult> {
    match args.command {
        ParticipantCommand::Show => show(context, pretty),
        ParticipantCommand::Bind(args) => {
            let cwd = std::env::current_dir().map_err(|error| {
                AppError::io(
                    "resolve current directory for participant bind",
                    ".".as_ref(),
                    error,
                )
            })?;
            let participant = participant::bind(context, &cwd, args.workspace.as_deref())?;
            let provenance = participant::resolve(context)?
                .provenance()
                .map(|p| p.as_str());
            CommandResult::json(
                &ParticipantOutput {
                    ok: true,
                    status: "bound",
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
            participant: Some(*participant),
            provenance: Some(provenance.as_str()),
            fix: None,
        },
        Resolved::Unbound => ParticipantOutput {
            ok: true,
            status: "unbound",
            participant: None,
            provenance: None,
            fix: Some("run: post participant bind"),
        },
    };
    CommandResult::json(&output, pretty)
}
