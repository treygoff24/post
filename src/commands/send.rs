use super::{CommandResult, Context};
use crate::cli::SendArgs;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::output::{self, Envelope, SendOutput};
use crate::{
    closest_room, encode_mail, exclusive_atomic_write, local_timestamp, new_mail_id,
    validate_envelope,
};
use serde_json::json;
use std::fs;
use std::io::{self, IsTerminal, Read};

pub fn run(
    context: &Context,
    args: SendArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    run_with_body(context, args, json_output, pretty, read_body)
}

fn run_with_body<F>(
    context: &Context,
    mut args: SendArgs,
    json_output: bool,
    pretty: bool,
    read_body: F,
) -> AppResult<CommandResult>
where
    F: FnOnce(Option<String>, Option<&std::path::Path>) -> AppResult<String>,
{
    let rooms = context.load_rooms()?;
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

    let body = read_body(args.body.take(), args.file.as_deref())?;
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

    let rules = context.load_rules(&rooms)?;
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

    let (id_timestamp, sent) = local_timestamp()?;
    let archive = context.root.join("archive");
    fs::create_dir_all(&archive)
        .map_err(|error| AppError::io("create archive directory", &archive, error))?;
    let (inbox, _) = context.mailbox_dirs(&args.to)?;
    let mut delivered = None;
    for attempt in 0..256 {
        let id = new_mail_id(&id_timestamp, attempt)?;
        let envelope = Envelope {
            id: id.clone(),
            from: sender.clone(),
            to: args.to.clone(),
            kind: args.kind,
            subject: args.subject.clone(),
            sent: sent.clone(),
        };
        validate_envelope(std::path::Path::new("<generated mail>"), &envelope)?;
        let payload = encode_mail(&envelope, &body)?;
        let archive_path = archive.join(format!("{id}.mail"));
        let inbox_path = inbox.join(format!("{id}.mail"));
        match exclusive_atomic_write(&archive_path, &payload) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::io(
                    "exclusively write archive mail",
                    &archive_path,
                    error,
                ));
            }
        }
        match exclusive_atomic_write(&inbox_path, &payload) {
            Ok(()) => {
                delivered = Some(envelope);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::io(
                    "exclusively write inbox mail",
                    &inbox_path,
                    error,
                ));
            }
        }
    }
    let envelope = delivered.ok_or_else(|| {
        AppError::new(
            ErrorCode::IoError,
            "could not allocate a unique message id after 256 attempts",
            "Retry the same send command; if this repeats, run `post doctor`.",
        )
    })?;

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
    Ok(CommandResult::committed(rendered))
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

#[cfg(test)]
mod tests {
    use super::run_with_body;
    use crate::cli::{MailKind, SendArgs};
    use crate::{Context, DEFAULT_ROOMS_JSON};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn body_after_rule_add_is_refused_before_any_mail_write() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should follow Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("post-send-order-{nonce}"));
        fs::create_dir_all(&root).expect("create send-order root");
        fs::write(root.join("rooms.json"), DEFAULT_ROOMS_JSON).expect("write rooms config");
        fs::write(root.join("rules.json"), r#"{"blocked":[]}"#).expect("write rules config");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let result = run_with_body(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("race-test".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: None,
                file: None,
            },
            false,
            false,
            |_, _| {
                fs::write(
                    root.join("rules.json"),
                    r#"{"blocked":[{"from":"race-test","to":"claude-space","reason":"added while body was read"}]}"#,
                )
                .expect("add rule while body reader is open");
                Ok("body from controllable stream".to_owned())
            },
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("rule added during body read must block delivery"),
        };
        assert_eq!(error.code.as_str(), "blocked_route");
        assert!(!root.join("archive").exists());
        assert!(!root.join("claude-space/inbox").exists());
        let cleanup = std::process::Command::new("trash")
            .arg(&root)
            .status()
            .expect("run recoverable test cleanup");
        assert!(cleanup.success(), "trash should clean send-order root");
    }
}
