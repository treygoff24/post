use crate::cli::{ParticipantArgs, ParticipantCommand, ParticipantShowArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::Context;
use crate::participant::{self, KeyResolution, Participant, Resolved};
use serde::Serialize;

#[derive(Serialize)]
struct ParticipantOutput {
    ok: bool,
    /// `bound`, `unbound`, `missing` (a claim names a record that does not
    /// exist), `archived` (post put the record away; `bind` restores it),
    /// `ended`.
    status: &'static str,
    /// `participant show` only: whether a record answers to the claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    bound: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    participant: Option<Participant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provenance: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    participant_error: Option<String>,
    /// A claimed identity that resolves to nothing, reported as a field:
    /// `participant show` is a diagnostic surface and exits 0 either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    participant_missing: Option<MissingReport>,
    /// `participant bind` only, the one time a participant is told what Post
    /// is: the activation notice, carried here so a JSON answer keeps stderr
    /// empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<&'static str>,
}

/// `participant_missing`, as a diagnostic surface reports it.
#[derive(Serialize)]
pub(super) struct MissingReport {
    /// `POST_PARTICIPANT` or `session-index`.
    claim: &'static str,
    id: Option<String>,
    pub(super) message: String,
    pub(super) suggested_fix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exact_fix: Option<String>,
}

impl MissingReport {
    /// The command that repairs the claim, as a `fix` field carries it: the
    /// exact command when the error names one, else the general bind line.
    pub(super) fn fix(&self) -> String {
        self.exact_fix
            .clone()
            .unwrap_or_else(|| UNBOUND_FIX.to_owned())
    }

    pub(super) fn from_error(error: &AppError) -> Self {
        let explicit = error
            .details
            .input
            .as_deref()
            .is_some_and(|input| input.starts_with("POST_PARTICIPANT="));
        Self {
            claim: if explicit {
                "POST_PARTICIPANT"
            } else {
                "session-index"
            },
            id: error.details.id.clone(),
            message: error.message.clone(),
            suggested_fix: error.suggested_fix.clone(),
            exact_fix: error.details.exact_fix.clone(),
        }
    }
}

#[derive(Serialize)]
struct ParticipantListOutput {
    ok: bool,
    participants: Vec<Participant>,
    count: usize,
    /// Damaged records left out of `participants`, with the reason. Present
    /// only when there is one; stderr also carries a warning, but a caller
    /// that discards stderr must still see the roster has a hole in it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<participant::SkippedParticipant>,
}

pub(super) fn run(
    context: &Context,
    args: ParticipantArgs,
    json: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    match args.command {
        ParticipantCommand::Notice {
            ack,
            claim,
            release,
        } => {
            let (participant, _) = participant::require(context)?;
            let _lock = if ack || claim.is_some() || release.is_some() {
                Some(participant::lock(context)?)
            } else {
                None
            };
            if ack {
                participant::acknowledge_notice(&participant)?;
            }
            if let Some(pid) = release {
                participant::release_notice(&participant, pid)?;
            }
            let pending = !ack && release.is_none() && participant::notice_pending(&participant);
            let busy = pending
                && claim
                    .map(|pid| participant::claim_notice(&participant, pid))
                    .transpose()?
                    == Some(false);
            CommandResult::json(
                &serde_json::json!({
                    "ok": true,
                    "busy": busy,
                    "notice": if pending && !busy {
                        Some(participant::ACTIVATION_NOTICE)
                    } else { None },
                }),
                pretty,
            )
        }
        ParticipantCommand::Show(args) => show(context, args, pretty),
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
                        crate::mailbox::env_var_os("POST_HARNESS")
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
            let participant = participant::bind(
                context,
                &cwd,
                args.workspace.as_deref(),
                bootstrap,
                args.fresh,
            )?;
            crate::cursor_state::routing::route_for_participant(context, &participant)?;
            // Only the bootstrap's text form (one `export` line) is not a JSON
            // document; every other bind answer is, and carries the notice.
            let text_export = bootstrap.is_some() && !json;
            let notice = if std::env::var_os("POST_NOTICE_MANAGED").is_none()
                && participant::emit_activation_notice(context, &participant, text_export)?
                && !text_export
            {
                Some(participant::ACTIVATION_NOTICE)
            } else {
                None
            };
            if text_export {
                let result =
                    CommandResult::success(format!("export POST_PARTICIPANT={}\n", participant.id));
                super::participant_auto_gc::maybe_run(context);
                return Ok(result);
            }
            let provenance = if bootstrap.is_some() {
                Some("explicit-bootstrap")
            } else {
                participant::resolve(context)?
                    .provenance()
                    .map(|p| p.as_str())
            };
            let id = participant.id.clone();
            let result = CommandResult::json(
                &ParticipantOutput {
                    ok: true,
                    status: "bound",
                    bound: None,
                    id: Some(id),
                    participant: Some(participant),
                    provenance,
                    fix: None,
                    participant_error: None,
                    participant_missing: None,
                    notice,
                },
                pretty,
            )?;
            super::participant_auto_gc::maybe_run(context);
            Ok(result)
        }
        ParticipantCommand::Touch => lifecycle(context, false, pretty),
        ParticipantCommand::End => lifecycle(context, true, pretty),
        ParticipantCommand::Gc(args) => super::participant_gc::run(context, args.apply, pretty),
        ParticipantCommand::Restore(args) => restore(context, &args.id, pretty),
        ParticipantCommand::List => {
            let (participants, skipped) = participant::list_with_skipped(context)?;
            participant::warn_skipped(&skipped);
            let count = participants.len();
            CommandResult::json(
                &ParticipantListOutput {
                    ok: true,
                    participants,
                    count,
                    skipped,
                },
                pretty,
            )
        }
    }
}

fn lifecycle(context: &Context, end: bool, pretty: bool) -> AppResult<CommandResult> {
    let (current, provenance) = participant::require(context)?;
    let participant = if end {
        participant::end(context, &current.id)?
    } else {
        participant::touch(context, &current.id)?
    };
    if !end {
        crate::cursor_state::routing::route_for_participant(context, &participant)?;
    }
    let id = participant.id.clone();
    CommandResult::json(
        &ParticipantOutput {
            ok: true,
            status: if end { "ended" } else { "bound" },
            bound: None,
            id: Some(id),
            participant: Some(participant),
            provenance: Some(provenance.as_str()),
            fix: None,
            participant_error: None,
            participant_missing: None,
            notice: None,
        },
        pretty,
    )
}

#[derive(Serialize)]
struct RestoreOutput {
    ok: bool,
    id: String,
    /// False when the record was already there and nothing changed.
    restored: bool,
    /// `archive` (the whole record, state included) or `tombstone` (recreated
    /// from what the tombstone kept; it held no state). Absent when nothing
    /// was restored.
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<&'static str>,
    participant: Participant,
}

/// `participant restore <id>`: the same restore an explicit claim and a bridge
/// delivery use, on its own. Idempotent; an id nothing holds is
/// `participant_missing` and creates nothing.
fn restore(context: &Context, id: &str, pretty: bool) -> AppResult<CommandResult> {
    let restored = participant::restore(context, id)?;
    CommandResult::json(
        &RestoreOutput {
            ok: true,
            id: restored.participant.id.clone(),
            restored: restored.from.is_some(),
            from: restored.from,
            participant: restored.participant,
        },
        pretty,
    )
}

const UNBOUND_FIX: &str = "run: post participant bind";

fn show(context: &Context, args: ParticipantShowArgs, pretty: bool) -> AppResult<CommandResult> {
    let output = match (args.harness.as_deref(), args.key.as_deref()) {
        (Some(harness), Some(key)) => show_key(context, harness, key)?,
        _ => show_acting(context),
    };
    CommandResult::json(&output, pretty)
}

fn unbound_output(status: &'static str, fix: Option<String>) -> ParticipantOutput {
    ParticipantOutput {
        ok: true,
        status,
        bound: Some(false),
        id: None,
        participant: None,
        provenance: None,
        fix,
        participant_error: None,
        participant_missing: None,
        notice: None,
    }
}

fn bound_output(participant: Participant, provenance: &'static str) -> ParticipantOutput {
    ParticipantOutput {
        ok: true,
        status: "bound",
        bound: Some(true),
        id: Some(participant.id.clone()),
        participant: Some(participant),
        provenance: Some(provenance),
        fix: None,
        participant_error: None,
        participant_missing: None,
        notice: None,
    }
}

fn missing_output(error: &AppError) -> ParticipantOutput {
    let report = MissingReport::from_error(error);
    ParticipantOutput {
        id: report.id.clone(),
        fix: report
            .exact_fix
            .clone()
            .or_else(|| Some(UNBOUND_FIX.to_owned())),
        participant_error: Some(error.message.clone()),
        participant_missing: Some(report),
        notice: None,
        ..unbound_output("missing", None)
    }
}

/// The acting participant, as the environment claims it. Never mints.
fn show_acting(context: &Context) -> ParticipantOutput {
    match participant::resolve(context) {
        Ok(Resolved::Bound {
            participant,
            provenance,
        }) => bound_output(*participant, provenance.as_str()),
        Ok(Resolved::Unbound) => unbound_output("unbound", Some(UNBOUND_FIX.to_owned())),
        Err(error) if error.code == ErrorCode::ParticipantMissing => missing_output(&error),
        Err(error) => ParticipantOutput {
            participant_error: Some(error.message),
            ..unbound_output("unbound", Some(UNBOUND_FIX.to_owned()))
        },
    }
}

/// The record a harness conversation key maps to, without minting one and
/// without reading the environment's claim: what a hook asks at session start.
fn show_key(context: &Context, harness: &str, key: &str) -> AppResult<ParticipantOutput> {
    Ok(match participant::lookup_key(context, harness, key)? {
        KeyResolution::Live(participant) => bound_output(*participant, "key-lookup"),
        KeyResolution::Dangling { indexed } => missing_output(&AppError::participant_missing(
            crate::error::MissingClaim::SessionIndex {
                harness,
                id: &indexed,
            },
            true,
        )),
        KeyResolution::Archived { id } => ParticipantOutput {
            id: Some(id),
            ..unbound_output("archived", Some("post participant bind".to_owned()))
        },
        KeyResolution::Unbound => unbound_output("unbound", Some(UNBOUND_FIX.to_owned())),
    })
}
