use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::output::{self, CommandSchema, ErrorSchema, ExitSchema, OutputShapes, SchemaOutput};

pub(super) fn run(pretty: bool) -> AppResult<CommandResult> {
    let commands = vec![
        command(
            "send",
            "post send --to <room> [--from <name>] [--kind letter|note|signal] [--subject <s>] (--body <text> | --body-file <path> | stdin)",
            "text; JSON with --json",
            "atomically writes <room>/inbox/<id>.mail then archive/<id>.mail; the three body forms are mutually exclusive alternatives, and omitting all of them reads stdin",
        ),
        command(
            "chat",
            "post chat <channel> [--peek | --discard] | post chat <channel> --join | post chat <channel> --send [--subject <s>] (--body <text> | --body-file <path> | stdin)",
            "framed text; JSON with --json",
            "--join creates the channel on first join and records the join as an event in history; --send atomically writes channels/<name>/messages/<id>.msg and implies from --body/--body-file; a plain read advances the reader's own cursor only after a successful emit; --peek never advances; --discard advances without emitting bodies; a cursor-advancing read into /dev/null is refused",
        ),
        command(
            "channels",
            "post channels",
            "JSON",
            "read-only listing of channels, members, and message counts",
        ),
        command(
            "inbox",
            "post inbox [--room <name>] [--text]",
            "JSON; text with --text",
            "creates missing mailbox inbox/read directories; does not alter mail",
        ),
        command(
            "read",
            "post read <id-or-prefix> [--room <name>] [--peek]",
            "framed text; JSON with --json",
            "moves inbox mail to read unless --peek; a prefix matching no unread mail falls back to the room's read store and then to archive copies addressed to that room, served with already_read=true and consuming nothing",
        ),
        command(
            "rooms",
            "post rooms [add <name> <path>]",
            "JSON",
            "listing is read-only; add locks, validates, and atomically updates rooms.json without editing rules.json",
        ),
        command(
            "schema",
            "post schema",
            "JSON",
            "none after first-run initialization",
        ),
        command(
            "doctor",
            "post doctor [--fix]",
            "JSON",
            "read-only unless --fix; --fix only creates missing directories/defaults",
        ),
        command(
            "watch",
            "post watch [--room <name>] [--once | --snapshot] [--interval-ms <ms>] [--text]",
            "NDJSON event union (mail | unreadable | channel_message), one per line; text with --text",
            "creates missing mailbox inbox/read directories for registered rooms; reads direct-mail and joined-channel envelopes only — never moves or alters mail, never emits body content, never advances channel cursors; --snapshot scans exactly once and exits 0 (empty scan emits nothing; direct-mail scan failure is a nonzero error, never a false empty; an unregistered room warns on stderr, scans nothing, and creates no directories)",
        ),
    ];
    let output_shapes = OutputShapes {
        doctor: fields(&[
            "ok",
            "status",
            "root",
            "checks",
            "count",
            "fixed",
            "exit_codes",
        ]),
        inbox: fields(&["ok", "room", "unread", "count", "skipped_unreadable"]),
        read_json: fields(&[
            "ok",
            "framing",
            "envelope",
            "body",
            "already_read (present and true only when served from the read/archive store)",
        ]),
        rooms: fields(&["ok", "rooms", "count"]),
        schema: fields(&[
            "ok",
            "name",
            "contract_version",
            "global_flags",
            "commands",
            "output_shapes",
            "error_shape",
            "error_codes",
            "exit_codes",
            "doctor_exit_codes",
            "laws",
            "environment",
        ]),
        send_json: fields(&["ok", "envelope", "archived"]),
        chat_join: fields(&[
            "ok",
            "channel",
            "room",
            "created",
            "already_member",
            "event_id",
        ]),
        chat_send: fields(&["ok", "message"]),
        chat_read: fields(&[
            "ok", "framing", "channel", "room", "peek", "messages", "count",
        ]),
        chat_discard: fields(&["ok", "channel", "room", "discarded", "cursor"]),
        channels: fields(&["ok", "channels", "count"]),
        watch: fields(&[
            "mail: event, room, id, from, kind, subject, sent",
            "unreadable: event, room, id",
            "channel_message: event, channel, id, from, subject, sent",
        ]),
    };
    let errors = ErrorCode::ALL
        .iter()
        .map(|code| ErrorSchema {
            code: code.as_str().to_owned(),
            exit: code.exit_code(),
            retryable: code.retryable(),
        })
        .collect();
    let output = SchemaOutput {
        ok: true,
        name: "post".to_owned(),
        contract_version: "1".to_owned(),
        global_flags: fields(&[
            "--json: switch send/read/chat from text to JSON; inbox/rooms/channels/schema/doctor are already JSON",
            "--pretty: pretty-print JSON",
            "--room <name>: command option for inbox/read/watch only; chat and channels derive identity from cwd and reject --room",
        ]),
        commands,
        output_shapes,
        error_shape: fields(&[
            "ok=false",
            "error.code",
            "error.message",
            "error.details",
            "error.retryable",
            "error.suggested_fix",
        ]),
        error_codes: errors,
        exit_codes: vec![
            exit(0, "success, including empty results"),
            exit(2, "usage or argument error"),
            exit(65, "validation error"),
            exit(66, "message not found"),
            exit(70, "non-retryable post-commit or internal failure"),
            exit(75, "retryable I/O failure"),
            exit(77, "blocked route"),
            exit(78, "invalid configuration or mail state"),
        ],
        doctor_exit_codes: doctor_exit_codes(),
        laws: fields(&[
            output::LAW_DATA,
            output::LAW_AUTHORITY,
            output::LAW_PERMISSION,
            output::LAW_VERIFY,
            "Blocked routes refuse before any mail write.",
            "Registered room names cannot be claimed from outside their room tree.",
            "One canonical workspace path can have only one registered room name.",
            "Every successful send has an immutable archive copy.",
            "delivered_output_failure is non-retryable after a committed direct send or channel mutation; committed room registration stdout failure is reported as success with best-effort diagnostics.",
            "Mail kinds are exactly letter, note, and signal.",
            "Channel messages are not mail: they carry no kind, so a signal structurally cannot occur in a channel; anything gate-grade stays 1:1 room mail.",
            "Blocked routes bar shared channel membership at join time; channels never carry what a route may not.",
            "Channel history is append-only and is its own immutable archive; nothing in messages/ is ever moved or deleted.",
            "Channel identity is inferred from cwd; membership and cursors require a registered room, and joins are recorded in the channel history itself.",
            "Watch emits channel events as notifications only and never advances any cursor; only a read advances, and only after a successful emit.",
            "A room's own channel messages are never news to it: they never ring its own watch, and a send advances the sender's cursor past their own message when they were already caught up.",
            "A message body comes from exactly one of --body, --body-file, or stdin; a body-file path that does not exist is a usage error, never a retryable I/O fault.",
            "Already-read mail stays retrievable by id or prefix from the read store and from archive copies addressed to that room; re-reading consumes nothing and reports already_read.",
            "A channel read never advances the cursor into /dev/null; skipping unread messages requires --discard.",
            "Every error that suggests a command suggests one that runs verbatim.",
        ]),
        environment: fields(&[
            "POST_MAIL_ROOT: absolute mailbox root override; intended for tests",
            "HOME: resolves the default ~/.claude-mail root and ~/ room paths",
        ]),
    };
    CommandResult::json(&output, pretty)
}

pub(super) fn doctor_exit_codes() -> Vec<ExitSchema> {
    vec![
        exit(0, "healthy"),
        exit(1, "findings present"),
        exit(3, "--fix failed"),
    ]
}

fn command(name: &str, usage: &str, default_output: &str, side_effects: &str) -> CommandSchema {
    CommandSchema {
        name: name.to_owned(),
        usage: usage.to_owned(),
        default_output: default_output.to_owned(),
        side_effects: side_effects.to_owned(),
    }
}

fn fields(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn exit(code: i32, meaning: &str) -> ExitSchema {
    ExitSchema {
        code,
        meaning: meaning.to_owned(),
    }
}
