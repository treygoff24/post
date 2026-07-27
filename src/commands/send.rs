use crate::cli::SendArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{
    closest_room, encode_mail, exclusive_atomic_write, local_timestamp, new_mail_id,
    validate_envelope, Context,
};
use crate::model::{Envelope, RoomMap};
use crate::output::{self, SendOutput};
use std::fs;
use std::io::{self, IsTerminal, Read};

pub(super) fn run(
    context: &Context,
    args: SendArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    run_with_body(context, args, json_output, pretty, read_body)
}

fn run_with_body<F>(
    context: &Context,
    args: SendArgs,
    json_output: bool,
    pretty: bool,
    read_body: F,
) -> AppResult<CommandResult>
where
    F: FnOnce(BodySource<'_>) -> AppResult<String>,
{
    run_with_body_and_id(context, args, json_output, pretty, read_body, new_mail_id)
}

fn run_with_body_and_id<F, G>(
    context: &Context,
    mut args: SendArgs,
    json_output: bool,
    pretty: bool,
    read_body: F,
    mut next_id: G,
) -> AppResult<CommandResult>
where
    F: FnOnce(BodySource<'_>) -> AppResult<String>,
    G: FnMut(&str, u64) -> AppResult<String>,
{
    let rooms = context.load_rooms()?;
    // Built before `sender` is consumed, so a body-input fix can echo the
    // exact flags this invocation used.
    let fix_prefix = send_fix_prefix(&args);
    let sender = match args.sender {
        Some(sender) => sender,
        None => {
            let inferred = context.infer_from_cwd(&rooms)?;
            // Sender identity is derived from cwd, so a prepared command run
            // from the wrong tree posts as that tree's room. Name the resolved
            // sender on stderr before anything is written; the success receipt
            // is otherwise the first place it appears, which is too late.
            eprintln!(
                "post: sending as '{inferred}' (identity inferred from cwd); pass --from <NAME> to send as someone else"
            );
            inferred
        }
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
        .input(args.to.clone())
        .reason("recipient is absent from rooms.json");
        if let Some(room) = suggestion {
            error = error.did_you_mean(room);
        }
        return Err(error);
    }

    let inline = args.body.take();
    let body = read_body(BodySource {
        inline,
        body_file: args.body_file.as_deref(),
        file: args.file.as_deref(),
        fix_prefix: fix_prefix.clone(),
    })?;
    if body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!(
                "Retry with `{fix_prefix} --body '<text>'` or pass a non-empty body file/stdin."
            ),
        )
        .exact_fix(format!("{fix_prefix} --body '<text>'"))
        .input("message body")
        .reason("empty or whitespace-only"));
    }

    let (id_timestamp, sent) = local_timestamp()?;
    let archive = context.root.join("archive");
    let mut inbox = None;
    let mut delivered = None;
    for attempt in 0..256 {
        let id = next_id(&id_timestamp, attempt)?;
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
        if inbox.is_none() {
            ensure_route_allowed(context, &rooms, &sender, &args.to)?;
            fs::create_dir_all(&archive)
                .map_err(|error| AppError::io("create archive directory", &archive, error))?;
            inbox = Some(context.mailbox_dirs(&args.to)?.0);
        }
        let inbox = inbox.as_ref().expect("mailbox was initialized");
        let archive_path = archive.join(format!("{id}.mail"));
        let inbox_path = inbox.join(format!("{id}.mail"));
        ensure_route_allowed(context, &rooms, &sender, &args.to)?;
        match exclusive_atomic_write(&inbox_path, &payload) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::io(
                    "exclusively write inbox mail",
                    &inbox_path,
                    error,
                ));
            }
        }
        match exclusive_atomic_write(&archive_path, &payload) {
            Ok(()) => {
                delivered = Some(envelope);
                break;
            }
            Err(error) => {
                return Err(AppError::delivered_unarchived(
                    &id,
                    &inbox_path,
                    &archive_path,
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

fn ensure_route_allowed(
    context: &Context,
    rooms: &RoomMap,
    sender: &str,
    recipient: &str,
) -> AppResult<()> {
    let rules = context.load_rules(rooms)?;
    let Some(rule) = rules
        .blocked
        .iter()
        .find(|rule| rule.matches_route(sender, recipient))
    else {
        return Ok(());
    };
    Err(AppError::new(
        ErrorCode::BlockedRoute,
        format!("route {sender} -> {recipient} is blocked: {}", rule.reason),
        "Do not route around this block. Ask the human operator to review rules.json.",
    )
    .input(format!("{sender} -> {recipient}"))
    .reason(rule.reason.clone())
    .rule(rule.clone()))
}

/// The three mutually exclusive body sources plus the verbatim command prefix
/// used to build executable fixes. Every fix this module emits has to run
/// as-is: the recurring papercut was a suggested fix the parser then rejected.
pub(super) struct BodySource<'a> {
    pub inline: Option<String>,
    pub body_file: Option<&'a std::path::Path>,
    pub file: Option<&'a std::path::Path>,
    pub fix_prefix: String,
}

/// Quote `value` for a POSIX shell so a fix stays executable when the body
/// carries spaces or quotes.
pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Rebuild the invocation that got us here, so a fix can append the corrected
/// body flag and still be copy-pasteable.
pub(super) fn send_fix_prefix(args: &SendArgs) -> String {
    let mut prefix = format!("post send --to {}", shell_quote(&args.to));
    if let Some(sender) = &args.sender {
        prefix.push_str(&format!(" --from {}", shell_quote(sender)));
    }
    if !args.subject.is_empty() {
        prefix.push_str(&format!(" --subject {}", shell_quote(&args.subject)));
    }
    prefix
}

pub(super) fn read_body(source: BodySource<'_>) -> AppResult<String> {
    if let Some(body) = source.inline {
        return Ok(body);
    }
    if let Some(path) = source.body_file.or(source.file) {
        return read_body_file(path, &source.fix_prefix);
    }
    if io::stdin().is_terminal() {
        let fix = format!("{} --body '<text>'", source.fix_prefix);
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "message body is missing and stdin is a terminal; post never prompts or waits for interactive input",
            format!("Run `{fix}`, or pass `--body-file <PATH>`, or pipe the body on stdin."),
        )
        .exact_fix(fix)
        .input("stdin")
        .reason("interactive terminal input is not allowed"));
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

fn read_body_file(path: &std::path::Path, fix_prefix: &str) -> AppResult<String> {
    let display = path.display().to_string();
    match fs::read_to_string(path) {
        Ok(body) => Ok(body),
        // The recurring mistake is inline message text landing in the body
        // FILE slot. A path that does not exist is a usage error, not a
        // retryable I/O fault, so it reports as invalid_argument and spells
        // out the corrected command in full.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let fix = format!("{fix_prefix} --body {}", shell_quote(&display));
            Err(AppError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "message body file '{display}' does not exist; that argument is a path to a body FILE, not inline message text"
                ),
                format!("If you meant to send that as text, run `{fix}`."),
            )
            .exact_fix(fix)
            .input(display.clone())
            .path(display)
            .reason("body file path does not exist"))
        }
        Err(error) => Err(
            AppError::io("read UTF-8 message body file", path, error).exact_fix(format!(
                "{fix_prefix} --body-file {}",
                shell_quote(&display)
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{run_with_body, run_with_body_and_id};
    use crate::cli::SendArgs;
    use crate::mailbox::{Context, DEFAULT_ROOMS_JSON};
    use crate::model::MailKind;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    fn test_context(label: &str) -> (std::path::PathBuf, Context) {
        let root = test_root(&format!("send-{label}"));
        fs::write(root.join("rooms.json"), DEFAULT_ROOMS_JSON).expect("write rooms config");
        fs::write(root.join("rules.json"), r#"{"blocked":[]}"#).expect("write rules config");
        (
            root.clone(),
            Context {
                root: root.clone(),
                home: root,
            },
        )
    }

    #[test]
    fn body_after_rule_add_is_refused_before_any_mail_write() {
        let (root, context) = test_context("order");
        let result = run_with_body(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("race-test".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: None,
                body_file: None,
                file: None,
            },
            false,
            false,
            |_| {
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
        trash_test_root(&root);
    }

    #[test]
    fn inbox_id_collision_retries_before_writing_any_archive_copy() {
        let (root, context) = test_context("collision");
        let (inbox, _) = context
            .mailbox_dirs("claude-space")
            .expect("create recipient mailbox");
        let collision_id = "20260715-120000-aaaaaa";
        let fresh_id = "20260715-120000-bbbbbb";
        fs::write(inbox.join(format!("{collision_id}.mail")), "existing mail")
            .expect("create colliding inbox mail");
        let mut ids = [collision_id, fresh_id].into_iter();

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("collision-test".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new mail".to_owned()),
                body_file: None,
                file: None,
            },
            false,
            false,
            |source| Ok(source.inline.expect("inline body")),
            |_, _| Ok(ids.next().expect("test provides two ids").to_owned()),
        )
        .expect("send should retry the colliding id");

        assert!(result.stdout.contains(fresh_id));
        assert_eq!(
            fs::read_to_string(inbox.join(format!("{collision_id}.mail")))
                .expect("read colliding inbox mail"),
            "existing mail"
        );
        assert!(!root.join(format!("archive/{collision_id}.mail")).exists());
        assert!(inbox.join(format!("{fresh_id}.mail")).is_file());
        assert!(root.join(format!("archive/{fresh_id}.mail")).is_file());
        trash_test_root(&root);
    }

    #[test]
    fn id_collision_retry_rechecks_a_new_blocking_rule() {
        let (root, context) = test_context("collision-rule");
        let (inbox, _) = context
            .mailbox_dirs("claude-space")
            .expect("create recipient mailbox");
        let collision_id = "20260715-120000-111111";
        let fresh_id = "20260715-120000-222222";
        fs::write(inbox.join(format!("{collision_id}.mail")), "existing mail")
            .expect("create colliding inbox mail");

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("rule-race".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new mail".to_owned()),
                body_file: None,
                file: None,
            },
            false,
            false,
            |source| Ok(source.inline.expect("inline body")),
            |_, attempt| {
                if attempt == 1 {
                    fs::write(
                        root.join("rules.json"),
                        r#"{"blocked":[{"from":"rule-race","to":"claude-space","reason":"added after collision"}]}"#,
                    )
                    .expect("add blocking rule before retry");
                }
                Ok(if attempt == 0 { collision_id } else { fresh_id }.to_owned())
            },
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("new blocking rule must stop the retry"),
        };
        assert_eq!(error.code.as_str(), "blocked_route");
        assert_eq!(
            fs::read_to_string(inbox.join(format!("{collision_id}.mail"))).unwrap(),
            "existing mail"
        );
        assert!(!inbox.join(format!("{fresh_id}.mail")).exists());
        assert_eq!(
            fs::read_dir(root.join("archive"))
                .expect("list archive")
                .count(),
            0
        );
        trash_test_root(&root);
    }

    #[test]
    fn archive_id_collision_preserves_old_archive_and_reports_committed_delivery() {
        let (root, context) = test_context("archive-collision");
        let id = "20260715-120000-a1c1d1";
        let archive = root.join("archive");
        fs::create_dir_all(&archive).expect("create archive fixture");
        let archive_path = archive.join(format!("{id}.mail"));
        fs::write(&archive_path, "immutable old archive").expect("create archive collision");

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("archive-collision".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new delivery".to_owned()),
                body_file: None,
                file: None,
            },
            false,
            false,
            |source| Ok(source.inline.expect("inline body")),
            |_, _| Ok(id.to_owned()),
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("archive collision must report delivered_unarchived"),
        };
        assert_eq!(error.code.as_str(), "delivered_unarchived");
        assert!(!error.retryable);
        assert_eq!(
            fs::read_to_string(&archive_path).unwrap(),
            "immutable old archive"
        );
        assert!(root.join(format!("claude-space/inbox/{id}.mail")).is_file());
        trash_test_root(&root);
    }

    #[test]
    fn exhausting_all_message_ids_leaves_existing_mail_untouched_and_is_retryable() {
        let (root, context) = test_context("id-exhaustion");
        let (inbox, _) = context
            .mailbox_dirs("claude-space")
            .expect("create recipient mailbox");
        let id = "20260715-120000-eeeeee";
        let inbox_path = inbox.join(format!("{id}.mail"));
        fs::write(&inbox_path, "existing inbox mail").expect("create collision fixture");

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("exhaustion-test".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new mail".to_owned()),
                body_file: None,
                file: None,
            },
            false,
            false,
            |source| Ok(source.inline.expect("inline body")),
            |_, _| Ok(id.to_owned()),
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("256 collisions must exhaust id allocation"),
        };
        assert_eq!(error.code.as_str(), "io_error");
        assert!(error.retryable);
        assert_eq!(
            fs::read_to_string(&inbox_path).unwrap(),
            "existing inbox mail"
        );
        assert_eq!(fs::read_dir(&inbox).expect("list inbox").count(), 1);
        assert_eq!(
            fs::read_dir(root.join("archive"))
                .expect("list archive")
                .count(),
            0
        );
        trash_test_root(&root);
    }
}
