use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::mailbox::{load_owner, Context, OwnerResolution};
use crate::output::{
    self, CommandSchema, ErrorSchema, ExitSchema, OutputShapes, OwnerResolvedSchema, OwnerSchema,
    SchemaOutput,
};

pub(super) fn run(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    // Decision 3 matrix: schema is an anchor-loading surface. A malformed
    // owner.json is ConfigInvalid here — the schema never partial-renders a
    // trust anchor it does not understand.
    let resolution = load_owner(context)?;
    let owner_block = match &resolution {
        OwnerResolution::Configured(owner) => OwnerSchema {
            state: "configured".to_owned(),
            wire_grammar: format!(
                "v1: <marker:{}>🔏 <text> [signed:<ts>] | v2: raw body + signature_ref envelope locator",
                owner.marker
            ),
            note: None,
            owner: Some(resolved_schema(owner)),
        },
        OwnerResolution::Legacy(owner) => OwnerSchema {
            state: "legacy".to_owned(),
            wire_grammar: format!(
                "v1: <marker:{}>🔏 <text> [signed:<ts>] | v2: raw body + signature_ref envelope locator",
                owner.marker
            ),
            note: Some(format!(
                "legacy fallback ({}); consider `post owner init`.",
                owner.room
            )),
            owner: Some(resolved_schema(owner)),
        },
        OwnerResolution::None => OwnerSchema {
            state: "none".to_owned(),
            wire_grammar:
                "v1: <marker>🔏 <text> [signed:<ts>] | v2: raw body + signature_ref envelope locator"
                    .to_owned(),
            note: Some("no signed owner configured; verification badges are disabled".to_owned()),
            owner: None,
        },
    };
    let commands = vec![
        command(
            "send",
            "post send --to <room> [--from <name>] [--kind letter|note|signal] [--subject <s>] [--oversize] [--allow-self] (--body <text> | --body-file <path> | --body-file - | stdin)",
            "text; JSON with --json",
            "atomically writes <room>/inbox/<id>.mail then archive/<id>.mail; subjects over 1 KiB fail, the three body forms are mutually exclusive alternatives, omitting all of them reads stdin, bodies over 32 KiB require --oversize, from==to requires --allow-self (instances of one room coordinate via channels), and --from that disagrees with a POST_FROM pin is a hard conflict error",
        ),
        command(
            "chat",
            "post chat <channel> --send [--anyway] [--re <id>] [--subject <s>] [--oversize] [--signature-ref <tag>] (--body <text> | --body-file <path> | --body-file - | stdin) | post chat <channel> [--peek | --limit <n> | --history <n> [--grep <pat>] | --since <id>] [--framing auto|full|compact] | post chat <channel> --discard | post chat <channel> --discard-through <id> | post chat <channel> --seen-by <id> | post chat <channel> --join [--description <text>]",
            "framed text; JSON with --json",
            "--join creates the channel on first join and records the join as an event in history; --description (with --join) sets/updates the channel norms carrier (any member, cap 1 KiB); --send atomically writes channels/<name>/messages/<id>.msg, rejects subjects over 1 KiB, implies from --body/--body-file, requires --oversize above 32 KiB, stamps @mentions of registered rooms and optional --re parent id, and by default bounces with crossed_send only when an unseen message is ADDRESSED to the sending room -- an @mention of it, a reply (--re) to something it wrote, or any message from the owner room, whose signed bodies cannot carry a mention without invalidating the signature; unseen messages that concern nobody in particular warn on stderr with a count and deliver (--anyway overrides the refusal); a refusal previews only the targeted messages, first line each, capped, and every decision (refused|warned|anyway) appends one JSON line to <root>/crossed-send.jsonl carrying room, channel, unseen, targeted and, for an --anyway following a refusal, anyway_after_ms; a plain consuming read emits the oldest 25 unread by default (or the oldest --limit N), consumes only emitted ids, and leaves newer messages unread for the next page; --limit 0 = all; --peek retains its newest-slice glance and never advances; --discard marks all currently unseen messages seen without emitting bodies; --discard-through <id> marks every currently unseen message at or before one message (full id or a prefix unique in that channel) seen, refuses when an unreadable unseen message sits in that range, is replay-safe (an already-seen range succeeds with advanced=false), and reports prior_cursor and cursor as max-seen-id summaries (never the model); --seen-by lists members whose seen-set contains an id (read-only); --history/--since are cursorless; --grep filters --history by case-insensitive regex; a cursor-advancing read into /dev/null is refused; the full framing banner renders once per room per day; --framing (body-returning reads only, rejected on --send/--join/--discard/--discard-through/--seen-by) selects auto (default: legacy once-daily wall on text, full laws elsewhere), full (the complete wall every invocation), or compact (condensed laws in one line); when --framing is absent the POST_FRAMING environment value (auto|full|compact) is consulted and a set-but-invalid or non-UTF-8 value warns on stderr and falls back to auto (presentation never breaks a read); explicit full and compact are stateless per-invocation and never consult or stamp the banner-day state, JSON framing source/authority are unchanged in every mode, and there is no none mode; channel messages from the OWNER room whose first line matches the signed-wire grammar <marker>🔏 <text> [signed:TS] are verified against the resolved owner's sidecar — see the schema `owner` block for sigs/ + allowed_signers (legacy fallback: a registered 'trey' room, sidecar at its registered path like ~/.trey-room); signed-v2: --signature-ref <tag> stamps the envelope locator {\"version\":2,\"tag\":<tag>} for detached-manifest verification of the raw body (multiline/arbitrary text; ≤1 MiB final body, a protocol cap --oversize does NOT lift; the locator is metadata, never a verdict — owner v2 messages verify at read against <sidecar>/sigs/<tag>.txt binding tag+channel+bytes+sha256, and any malformed owner locator fails loudly)",
        ),
        command(
            "channels",
            "post channels [--text]",
            "JSON; text with --text",
            "read-only listing of channels, members, message counts, descriptions, and additive room/unread fields; unread is a number for member channels and null for non-members or no acting room",
        ),
        command(
            "inbox",
            "post inbox [--room <name>] [--text]",
            "JSON; text with --text",
            "creates missing mailbox inbox/read directories; does not alter mail; JSON adds unread_count while unread/count retain their physical-inbox meanings",
        ),
        command(
            "read",
            "post read <id-or-prefix> [--room <name>] [--peek] [--framing auto|full|compact]",
            "framed text; JSON with --json",
            "moves inbox mail to read unless --peek; --framing compact swaps the multi-line banner for the condensed one-sentence laws (stateless, explicit per invocation, no none mode; auto is the default and equals full on direct reads; JSON framing source/authority unchanged); when --framing is absent the POST_FRAMING environment value (auto|full|compact) is consulted and a set-but-invalid or non-UTF-8 value warns on stderr and falls back to auto (presentation never breaks a read); a prefix matching no unread mail falls back to the room's read store and then to archive copies this room is a party to -- either addressed to it or sent by it -- served with already_read=true and consuming nothing; an archived id this room is not a party to reports not_found naming that fact rather than claiming the id is absent, an archive candidate that cannot be parsed is reported as invalid mail state, never classified as another room's, and an id that names a CHANNEL message reports which channel holds it with an exact_fix of `post chat <channel> --history <n>` chosen so the named message is rendered",
        ),
        command(
            "catchup",
            "post catchup [<channel> | --mail | --all] [--framing auto|full|compact]",
            "framed text; JSON with --json",
            "writer that consumes the complete unread slice for direct mail, one joined channel, or all targets (no selector means --all); captures the fixed delta before stdout and records it only after a successful emit; positional channels require membership and fail closed on an unreadable message, while --all skips an unloadable never-joined channel with a stderr warning and reports a joined broken channel as a zero-count target; --framing applies only to this body-bearing surface and emits one banner above all non-empty sections (auto is compact); a non-empty result redirected to /dev/null is refused",
        ),
        command(
            "search",
            "post search <pattern> [--mail | --channel <channel>] [--limit <1..=1000>] [--framing auto|full|compact]",
            "framed text; JSON with --json",
            "read-only, cursorless literal case-insensitive Unicode substring search over party-visible direct mail and joined channels; --mail and --channel are mutually exclusive, membership/party filters apply before message content is opened, results are deterministic newest-first with a default limit of 100 and hard cap of 1000, and previews are sanitized and capped at 160 Unicode scalar values; no mailbox, cursor, or banner state is changed",
        ),
        command(
            "rooms",
            "post rooms [add <name> <path>]",
            "JSON",
            "listing is read-only; add locks, validates, and atomically updates rooms.json without editing rules.json",
        ),
        command(
            "profile",
            "post profile [show [<room>]] | post profile set [--name <name>] [--pfp <emoji>] | post profile clear",
            "JSON",
            "presentation only — display name and pfp never affect identity, auth, routing, blocks, cursors, or signed-message verification, and every render path keeps the immutable (room-id) suffix visible; set/clear act on the cwd-resolved registered room and atomically update profiles.json under the rooms lock; names are <=32 chars, refuse control/bidi/line-separator characters, and may not imitate the signed owner's room id (legacy fallback reserves 'trey'; feature-absent reserves nothing) or another room id (NFKC skeleton check); pfp is exactly one emoji grapheme, unique across rooms; profiles are stamped into envelopes at send time (renames never rewrite history) after re-validation, so hand-edited registry values and unregistered --from senders never stamp (set also drops, with a warning, a preserved stored field that no longer validates); a name, pfp, or clear change announces itself as a 'profile' event in every channel the room belongs to, with the channel list resolved before the registry commit so a listing failure fails pre-commit",
        ),
        command(
            "owner",
            "post owner init --room <name> [--marker <glyph>] [--label <text>] [--sidecar-dir <abs>] [--allowed-signers <abs>] [--principal <principal>] [--namespace <namespace>] | post owner show",
            "JSON",
            "declares or prints the signed owner, the trust anchor whose channel messages carry verification badges; init is create-only and atomic — an identical existing owner.json is an idempotent success, a different or malformed one is config_invalid (differing fields in details.reason), a symlinked owner.json is refused, and nothing is ever replaced or repaired; every explicit value and the room registration are validated under the rooms lock, then <sidecar>/sigs/ is created; show resolves the feature states configured|legacy|none (no owner.json + a registered 'trey' room synthesizes the pre-A0a owner; neither = none; a malformed owner.json is config_invalid, never a partial render); post only verifies with ssh-keygen against allowed_signers — post never generates keys, porch signs",
        ),
        command(
            "schema",
            "post schema",
            "JSON",
            "none after first-run initialization",
        ),
        command(
            "doctor",
            "post doctor [--fix] [--brief]",
            "JSON; one summary line with --brief",
            "read-only unless --fix; --fix only creates missing directories/defaults; --brief prints `post doctor: ok (N checks)` or `post doctor: N findings (run post doctor for detail)` with exit codes unchanged",
        ),
        command(
            "watch",
            "post watch [--room <name>]... [--once | --snapshot [--limit <n>]] [--interval-ms <ms>] [--digest] [--text]",
            "NDJSON event union (mail | unreadable | channel_message), one per line; --digest emits one digest per room/source group; text with --text",
            "creates missing mailbox inbox/read directories for registered rooms; a multi-room watch merges direct mail and deduplicates channel messages in one stream; --own <room> (repeatable, default empty) declares identities the watcher IS, whose channel messages never ring it in any scanned room -- watching a room never implies owning it, so an observer keeps receiving rooms it selected but did not declare; reads envelopes only — never moves or alters mail, never emits body content, never mutates channel seen-sets; scans are the truth source and a native filesystem watcher (inotify/FSEvents) only supplies wake hints, with a slow periodic re-registration pass and poll fallback at --interval-ms; each long-running poll touches <room>/watch.heartbeat for presence (snapshot never does); events carry reason mail|channel|mention on every type (unreadable: mail|channel); --digest groups each batch by room and source (mail or channel:<name>) in first-arrival order; --snapshot scans exactly once (unread direct mail plus joined-channel messages outside the seen-set) and exits 0 (empty scan emits nothing; direct-mail scan failure is a nonzero error, never a false empty; an unregistered room warns on stderr, scans nothing, and creates no directories); snapshot-only --limit <n> admits the last n underlying events before optional digest grouping and warns when earlier events are omitted, while --limit 0 is unlimited",
        ),
        command(
            "who",
            "post who [--room <name>]... [--text]",
            "JSON; text with --text",
            "read-only presence: for each registered room, whether a watch heartbeat is live and the last-seen stamp; never reports PIDs or process info. A live heartbeat means SOME local process is watching that room, not that the room's own agent is alive: any caller may watch any room, so this answers 'is anyone watching' and cannot answer 'is that agent up'",
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
        inbox: fields(&[
            "ok",
            "room",
            "unread",
            "count",
            "skipped_unreadable",
            "unread_count",
        ]),
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
            "owner (state: configured|legacy|none, wire_grammar, note?, owner?: {room, sidecar_dir, allowed_signers, principal, namespace, marker, label})",
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
            "ok",
            "framing",
            "channel",
            "room",
            "peek",
            "messages",
            "count",
            "skipped (un-emitted remainder; omitted when 0)",
            "has_more",
        ]),
        chat_discard: fields(&["ok", "channel", "room", "discarded", "cursor (max seen id, a summary of the underlying seen-set)"]),
        chat_discard_through: fields(&[
            "ok",
            "channel",
            "room",
            "target",
            "prior_cursor",
            "cursor (max seen id, a summary of the underlying seen-set)",
            "advanced",
            "discarded",
        ]),
        catchup: fields(&[
            "ok",
            "room",
            "targets[] (source=mail|channel, framing, messages[], count; channel targets also include channel)",
            "targets[].framing (source, authority, laws)",
            "targets[].messages[] (mail: envelope, body; channel: id, from, channel, subject, sent, body)",
            "count",
        ]),
        search: fields(&[
            "ok",
            "framing (source, authority, laws)",
            "room",
            "pattern",
            "match",
            "results[] (source, channel?, id, from, sent, subject, preview, matched, kind? for mail)",
            "count",
            "limit",
            "truncated",
        ]),
        channels: fields(&[
            "ok",
            "channels (name, created, created_by, description?, members, messages, room, unread)",
            "count",
        ]),
        profile: fields(&[
            "ok",
            "room",
            "profile (name?, pfp?)",
            "announced (set/clear; channels that received the change event)",
        ]),
        watch: fields(&[
            "mail: event, room, id, from, kind, subject, sent, reason=mail, preview? [, display_name, pfp, sender_address, sender_provenance]",
            "unreadable: event, room, id, reason=mail|channel (no preview)",
            "channel_message: event, channel, id, from, subject, sent, reason=channel|mention, preview? [, display_name, pfp, sender_address, sender_provenance]",
            "digest: event=digest, room, source=mail|channel:<name>, count, first_id, last_id, from, reason=mail|channel|mention|mixed, preview? (text preview precedes bounds/since suffix)",
        ]),
        who: fields(&["ok", "rooms (room, live_watch, last_seen?)", "count"]),
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
            "--json: switch send/read/chat/catchup/search from text to JSON; inbox/rooms/channels/profile/schema/doctor/who are already JSON",
            "--pretty: pretty-print JSON",
            "--room <name>: command option for inbox/read/watch/who only; chat and channels derive identity from cwd and reject --room",
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
        owner: owner_block,
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
            "Channel identity is inferred from cwd; membership and per-room channel state require a registered room, and joins are recorded in the channel history itself.",
            "Watch emits channel events as notifications only and never marks channel messages seen; only a read consumes, and only after a successful emit.",
            "A room's own channel messages are never news to it: they never ring its own watch, and a send records the sender's own message id as seen unconditionally. Selecting a room to watch is not the same as being it: a watcher may declare identities with --own (repeatable), and a message from any declared-own room is suppressed across every room that watch scans, while a room that is only watched still rings.",
            "A message body comes from exactly one of --body, --body-file, or stdin; a body-file path that does not exist is a usage error, never a retryable I/O fault.",
            "Shell quoting happens before Post: double quotes can expand dollar-positionals such as $1 in $1.63B, and an apostrophe can terminate single quotes; use --body-file or stdin for shell-sensitive prose.",
            "Subjects over 1 KiB fail before any write with no override; longer text belongs in the body.",
            "Message bodies over 32 KiB fail before any write unless --oversize records explicit intent; complete Post watch-event NDJSON lines warn on stderr but still send.",
            "Already-read mail stays retrievable by id or prefix from the read store and from archive copies this room sent or was sent; re-reading consumes nothing and reports already_read. Archive party-filtering is a convenience boundary, not an authorization one: `from` is a self-declared field, so a room registered later under a name a past message declared can read that message.",
            "A channel read refuses to consume its unread batch into /dev/null; skipping unread messages requires --discard or --discard-through.",
            "Every consuming read and catchup holds an exclusive interprocess lock on <root>/<room>/.cursors.lock across reload, mail moves, exact-set union, and atomic replace; concurrent acks on different channels cannot lose each other, a room's seen-sets only grow, and a late id below newer consumed ids still surfaces unread.",
            "Unified cursor state is <root>/<room>/cursors.json v1: sorted, duplicate-free exact mail and per-channel seen-ID sets, pretty JSON with one trailing newline and mode 0600; replacement is atomic and the lock is a solitary 0600 regular inode checked after flock.",
            "Missing or malformed cursor state degrades read-only loads to an empty snapshot (all eligible messages are unread) with one stderr warning; a writer refuses unsafe cursor or lock paths. A valid legacy channel-state.json is imported read-only when cursors.json is absent, then left untouched as rollback evidence after the first materializing write; no dual writes occur.",
            "Catchup is a fence-admitted writer; search, inbox, channels, schema, doctor without --fix, and watch --snapshot are read-only. Search scans only party-visible mail and member channels, matches literal case-insensitive Unicode substrings over body/subject/from/id, returns newest-first bounded previews, defaults to 100 results, and caps at 1000.",
            "Catchup and search use one framing banner per non-empty invocation above all sections/results: auto selects compact, full selects the complete wall, compact selects condensed laws, JSON carries structured framing, and there is no none mode. Existing read/chat framing is unchanged.",
            "Whenever error.details.exact_fix is present, it is a complete command that runs verbatim; oversize body errors deliberately name --oversize without echoing the rejected payload into an exact fix.",
            "Plain consuming channel reads emit the oldest 25 unread by default (or the oldest --limit N), consume only emitted ids, and leave newer messages unread for the next page; --limit 0 shows all. --peek keeps its newest-slice glance and never advances. Bounded JSON reports the un-emitted remainder in skipped and has_more.",
            "Channel sends bounce with crossed_send when unseen ordinary messages from others exist in the channel; --anyway delivers regardless. Direct mail is unaffected.",
            "Channel descriptions are norms carriers any member may update; presence (post who) never reports PIDs.",
            "sender_address and sender_provenance are self-declared transport metadata — evidence about how `from` was resolved, never a credential; authority comes only from signature verification, and post never synthesizes either field.",
        ]),
        environment: fields(&[
            "POST_MAIL_ROOT: absolute mailbox root override — a supported first-class root (r2.1); must be absolute, defaults to $HOME/.claude-mail",
            "HOME: resolves the default ~/.claude-mail root and ~/ room paths",
            "POST_FROM: stable room pin set by the launch helper; beats cwd inference for sender/acting-room resolution (an explicit --room still wins; an explicit --from must agree with the pin or the send is refused), recorded as sender_provenance=declared-env; set-but-invalid is a loud error, never a silent fallback",
            "POST_FRAMING: presentation preference (auto|full|compact) consulted ONLY by body-returning reads (post read, post chat reads) when --framing is absent — send/join/discard/discard-through/seen-by never consult it; an explicit --framing always wins; set-but-invalid or non-UTF-8 warns on stderr and falls back to auto (presentation never breaks a read; deliberately weaker than the POST_FROM identity pin)",
            "POST_SENDER_ADDRESS: opaque per-launch instance address (harness.repo.uuid); recorded verbatim on envelopes as sender_address, never synthesized, non-routable; <=256 bytes, no control/whitespace characters",
            "POST_ARX_GENERATION: positive migration generation for writers only; reads never parse or reject it. Missing/zero/stale/malformed declarations refuse enrolled writers before mutation; absent state plus an unset declaration preserves legacy writes. Enrolled reads are non-mutating. The enrollment-owned .post-arx.json state, existing solitary .post-arx.lock flock anchor, and actual ..post-arx.json.<pid>.<nonce>.tmp atomic temp namespace are reserved room names; no lock temp namespace is produced; the lock inode is never unlinked or recreated. Cursor state is a fence boundary: catchup requires writer admission, while search and listings remain read-only; valid legacy channel-state.json imports in memory on read and materializes only on the first admitted cursor write, remaining untouched as rollback evidence.",
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

fn resolved_schema(owner: &crate::mailbox::ResolvedOwner) -> OwnerResolvedSchema {
    OwnerResolvedSchema {
        room: owner.room.clone(),
        sidecar_dir: owner.sidecar_dir.display().to_string(),
        allowed_signers: owner.allowed_signers.display().to_string(),
        principal: owner.principal.clone(),
        namespace: owner.namespace.clone(),
        marker: owner.marker.clone(),
        label: owner.label.clone(),
    }
}
