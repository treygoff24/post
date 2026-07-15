use super::{CommandResult, Context};
use crate::cli::SendArgs;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::output::{self, Envelope, SendOutput};
use crate::{atomic_write, closest_room, encode_mail, local_timestamp, new_mail_id};
use serde_json::json;
use std::fs;
use std::io::{self, IsTerminal, Read};

pub fn run(
    context: &Context,
    args: SendArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    let sender = match args.sender {
        Some(sender) => sender,
        None => context.infer_from_cwd(&rooms)?,
    };
    context.ensure_sender_allowed(&sender, &rooms)?;

    if !rooms.contains_key(&args.to) {
        let suggestion = closest_room(&args.to, &rooms);
        let mut error = AppError::new(
            ErrorCode::UnknownRoom,
            match suggestion {
                Some(room) => format!(
                    "recipient room '{}' is unknown; did you mean '{room}'?",
                    args.to
                ),
                None => format!("recipient room '{}' is unknown", args.to),
            },
            "Run `post rooms`, then retry with `post send --to <registered-room> ...`.",
        )
        .detail("input", args.to.clone())
        .detail("reason", "recipient is absent from rooms.json");
        if let Some(room) = suggestion {
            error = error.detail("did_you_mean", room);
        }
        return Err(error);
    }

    if let Some(rule) = rules.blocked.iter().find(|rule| {
        (rule.from == "*" || rule.from == sender) && (rule.to == "*" || rule.to == args.to)
    }) {
        return Err(AppError::new(
            ErrorCode::BlockedRoute,
            format!("route {sender} -> {} is blocked: {}", args.to, rule.reason),
            "Do not route around this block. Ask the human operator to review rules.json.",
        )
        .detail("input", format!("{sender} -> {}", args.to))
        .detail("reason", rule.reason.clone())
        .detail(
            "rule",
            json!({"from": rule.from, "to": rule.to, "reason": rule.reason}),
        ));
    }

    let body = read_body(args.body, args.file.as_deref())?;
    if body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!(
                "Retry with `post send --to {} --body '<text>'` or pass a non-empty FILE/stdin.",
                args.to
            ),
        )
        .detail("input", "message body")
        .detail("reason", "empty or whitespace-only"));
    }

    let (id_timestamp, sent) = local_timestamp()?;
    let archive = context.root.join("archive");
    fs::create_dir_all(&archive)
        .map_err(|error| AppError::io("create archive directory", &archive, error))?;
    let (inbox, _) = context.mailbox_dirs(&args.to)?;
    let (id, inbox_path, archive_path) = (0..256)
        .find_map(|attempt| {
            let id = new_mail_id(&id_timestamp, attempt).ok()?;
            let inbox_path = inbox.join(format!("{id}.mail"));
            let archive_path = archive.join(format!("{id}.mail"));
            (!inbox_path.exists() && !archive_path.exists()).then_some((
                id,
                inbox_path,
                archive_path,
            ))
        })
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::IoError,
                "could not allocate a unique message id after 256 attempts",
                "Retry the same send command; if this repeats, run `post doctor`.",
            )
        })?;
    let envelope = Envelope {
        id,
        from: sender,
        to: args.to,
        kind: args.kind,
        subject: args.subject,
        sent,
    };
    let payload = encode_mail(&envelope, &body)?;

    // Archive first: a failed delivery can leave an observable archive, but never
    // a delivered message without the immutable archive required by the contract.
    atomic_write(&archive_path, &payload)?;
    atomic_write(&inbox_path, &payload)?;

    let rendered = if json_output {
        output::json(
            &SendOutput {
                ok: true,
                envelope,
                archived: true,
            },
            pretty,
        )?
    } else {
        format!(
            "post: sent {} {} {} -> {}\n",
            envelope.kind, envelope.id, envelope.from, envelope.to
        )
    };
    Ok(CommandResult::success(rendered))
}

fn read_body(body: Option<String>, file: Option<&std::path::Path>) -> AppResult<String> {
    if let Some(body) = body {
        return Ok(body);
    }
    if let Some(path) = file {
        return fs::read_to_string(path).map_err(|error| {
            AppError::io("read UTF-8 message body file", path, error).detail(
                "exact_fix",
                "Pass an existing UTF-8 FILE, use `--body <text>`, or pipe the body on stdin.",
            )
        });
    }
    if io::stdin().is_terminal() {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "message body is missing and stdin is a terminal; post never prompts or waits for interactive input",
            "Pass `--body '<text>'`, pass a UTF-8 FILE, or pipe the body on stdin.",
        )
        .detail("input", "stdin")
        .detail("reason", "interactive terminal input is not allowed"));
    }
    let mut body = String::new();
    io::stdin()
        .lock()
        .read_to_string(&mut body)
        .map_err(|error| {
            AppError::io(
                "read message body from stdin",
                std::path::Path::new("<stdin>"),
                error,
            )
        })?;
    Ok(body)
}
