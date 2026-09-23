use super::version;
use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::mailbox::{load_owner, Context, OwnerResolution};
use crate::output::{
    CommandSchema, ErrorSchema, ExitSchema, OutputShapes, OwnerResolvedSchema, OwnerSchema,
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
            "participant",
            "post participant show | post participant bind [--workspace <room>] [--harness <slug> --key <conversation-key> | --new [--harness <slug>]] | post participant touch | post participant end | post participant list | post participant notice [--ack | --claim <pid> | --release <pid>]",
            "JSON",
            "show/list are read-only; bind is the only participant minting path and refreshes last_seen, preserves an existing lease_hours unless POST_PARTICIPANT_LEASE_HOURS is explicit (new records default to 24), clears ended_at, and commits participant.json before its by-session index under .participants.lock; touch refreshes last_seen and likewise preserves the recorded lease unless explicitly overridden; end sets ended_at idempotently",
        ),
        command(
            "identity",
            "post identity list | post identity show <name> [--voices] | post identity new <name> | post identity continue <name> [--acknowledge] | post identity leave | post identity voice add --body-file <path> | post identity voice withdraw [--lineage <name>] | post identity terms set --body-file <path>",
            "JSON",
            "list/show are read-only and voice bodies load only with --voices; new/continue/leave change only the acting participant's historical affiliation; continue requires --acknowledge when terms exist; voice and terms bodies come only from the named files",
        ),
        command(
            "send",
            "post send --to <workspace:<room>|lineage:<name>|participant:<id>|participant:<id>@<host>|bare-name> [--from <name>] [--kind letter|note|signal] [--subject <s>] [--oversize] (--body <text> | --body-file <path> | --body-file - | stdin)",
            "text; JSON with --json",
            "atomically writes the resolved address's canonical inbox plus archive/<id>.mail, then publishes an atomic routing receipt when recipients exist; workspace/lineage fan-out suppresses only the sending local participant (remote-origin mail never excludes a local id), while an explicit participant:<self> target is readable; subjects over 1 KiB fail, body forms are exclusive, and --from that disagrees with the bound reply address is refused; participant:<id>@<host> (split at the last @; an exact local participant record of that full name stays local; this host's bridge host resolves as participant:<id>) queues a letter for an enrolled peer host: the sender must be bound to a registered local room (remote_sender_unroutable otherwise), bridge/health.json must be fresh (ticked_at within 3x interval_s) and list typed-outbound-exclusion and participant-mail-v1 (bridge_unsupported when a fresh file lacks one; retryable bridge_status_unavailable when missing, malformed, or stale), and a local rule blocking this sender (or *) to * applies; the letter (to=<id>, address_kind=participant, to_host=<host>) is written only to archive/<id>.mail, never to a workspace or participant inbox or outbox/, is not routed, and the receipt says delivery.state=queued; the enrolled set is bridge/registry/hosts.json minus this host, intersected with bridge/config.json peers when non-empty, with no fallback to config peers (retryable topology_unavailable when the registry or config is missing or invalid; unknown_host lists the enrolled hosts in details.matches; no_bridge without bridge/config.json); a host-qualified error never falls back to a room, lineage, or bare id",
        ),
        command(
            "chat",
            "post chat <channel> --send [--anyway] [--re <id>] [--subject <s>] [--oversize] [--signature-ref <tag>] (--body <text> | --body-file <path> | --body-file - | stdin) | post chat <channel> [--peek | --limit <n> | --history <n> [--grep <pat>] | --since <id>] [--max-bytes <n>] [--framing auto|full|compact] | post chat <channel> --message <id> [--offset <b>] [--length <b>] --max-bytes <n> [--framing auto|full|compact] | post chat <channel> --ack <id> | post chat <channel> --discard | post chat <channel> --discard-through <id> | post chat <channel> --seen-by <id> | post chat <channel> --join [--description <text>] | post chat <channel> --leave | post chat <channel> --archive | post chat <channel> --unarchive",
            "framed text; JSON with --json",
            "--join creates the channel on first join and records the join as an event in history; --leave opts out only the acting participant and preserves its seen set; --archive/--unarchive (any bound participant, membership not required, idempotent) set or clear the host-local archive mark in channels/<name>/archive.json -- never history, never deleting -- whose append-only log records who and when; a channel stays archived until a conversational (non-event) message newer than the mark arrives, so a post resurrects it while join/profile events do not; --description (with --join) sets/updates the channel norms carrier (any member, cap 1 KiB); --send atomically writes channels/<name>/messages/<id>.msg, rejects subjects over 1 KiB, implies from --body/--body-file, requires --oversize above 32 KiB, stamps @mentions of registered rooms and optional --re parent id, and by default bounces with crossed_send only when an unseen message is ADDRESSED to the sending room -- an @mention of it, a reply (--re) to something it wrote, or any message from the owner room, whose signed bodies cannot carry a mention without invalidating the signature; unseen messages that concern nobody in particular warn on stderr with a count and deliver (--anyway overrides the refusal); a refusal previews only the targeted messages, first line each, capped, and every decision (refused|warned|anyway) appends one JSON line to <root>/crossed-send.jsonl carrying room, channel, unseen, targeted and, for an --anyway following a refusal, anyway_after_ms; a plain consuming read emits the oldest 25 unread by default (or the oldest --limit N), consumes only emitted ids, and leaves newer messages unread for the next page; --limit 0 = all; --peek retains its newest-slice glance and never advances; opt-in --max-bytes caps final stdout and admits only a contiguous prefix of complete selected messages while keeping count-window skipped separate from byte omission and reporting omitted mention count; --message returns a cursorless UTF-8 body_slice with explicit byte range/progress, verifies signature status against the complete stored body, and never consumes; --ack parses and marks exactly one id seen after stdout; --discard marks all currently unseen messages seen without emitting bodies; --discard-through <id> marks every currently unseen message at or before one message (full id or a prefix unique in that channel) seen, refuses when an unreadable unseen message sits in that range, is replay-safe (an already-seen range succeeds with advanced=false), and reports prior_cursor and cursor as max-seen-id summaries (never the model); --seen-by lists members whose seen-set contains an id (read-only); --history/--since are cursorless; --grep filters --history by case-insensitive regex; a cursor-advancing read into /dev/null is refused; a read (plain, --peek, --history/--since, --discard, --message) refuses stdin that carries input before routing or marking anything seen -- a nonempty file, or a pipe/heredoc/socket with a queued byte, is invalid_argument (exit 2); a pipe still open and silent after a bounded readiness wait of at most 100 ms is input_ambiguous (exit 2) -- and never sends; exact_fix runs the send correction (--send --body-file -) and names the read correction (stdin from /dev/null); an interactive terminal, /dev/null, an empty file, and a pipe at EOF read normally with no wait; a producer slower than the wait is refused as ambiguous, and no finite wait can detect every delayed producer; activation notice is once per participant; default reads are quiet; --framing (body-returning reads only, rejected on --send/--join/--leave/--discard/--discard-through/--seen-by/--ack) selects auto (quiet default), full (explicit complete banner), or compact (explicit condensed banner); absent the flag, POST_FRAMING supplies the mode (legacy env compact selects quiet auto); invalid values warn and use auto; default JSON preserves source/authority but omits laws; no mode consults or stamps banner-day; channel messages from the OWNER room whose first line matches the signed-wire grammar <marker>🔏 <text> [signed:TS] are verified against the resolved owner's sidecar — see the schema `owner` block for sigs/ + allowed_signers (legacy fallback: a registered 'trey' room, sidecar at its registered path like ~/.trey-room); signed-v2: --signature-ref <tag> stamps the envelope locator {\"version\":2,\"tag\":<tag>} for detached-manifest verification of the raw body (multiline/arbitrary text; ≤1 MiB final body, a protocol cap --oversize does NOT lift; the locator is metadata, never a verdict — owner v2 messages verify at read against <sidecar>/sigs/<tag>.txt binding tag+channel+bytes+sha256, and any malformed owner locator fails loudly)",
        ),
        command(
            "channels",
            "post channels [--archived | --all] [--text]",
            "JSON; text with --text",
            "read-only listing; archived channels are hidden by default and counted in archived_hidden, --archived lists only them and --all lists both, each item carries archived plus archived_at/archived_by when archived; members remains the workspace-level bridge/doorbell projection, participants lists host-local effective participant ids, and unread comes from the participant eligibility snapshot",
        ),
        command(
            "inbox",
            "post inbox [--room <name>] [--text] [--adopt]",
            "JSON; text with --text",
            "listing is read-only; --adopt is a writer that routes held lineage mail to the current eligible affiliates without making later affiliates retroactive recipients; JSON keeps unread and pending counts distinct",
        ),
        command(
            "read",
            "post read <id-or-prefix> [--room <name>] [--peek] [--max-bytes <n>] [--framing auto|full|compact] | post read <id-or-prefix> [--room <name>] [--offset <b>] [--length <b>] --max-bytes <n> [--framing auto|full|compact] | post read <id-or-prefix> [--room <name>] --ack",
            "framed text; JSON with --json",
            "participant reads validate the frozen receipt digest through the same eligibility snapshot as inbox/catchup, emit the complete selected message, then record its exact id in participants/<id>/cursors.json v2 only after successful stdout; canonical inbox mail never moves and legacy room read/ is ignored; --peek and slices are read-only, provisional mail is labeled pending=true rather than unread, and explicit sender-history inspection is own=true and never changes seen state",
        ),
        command(
            "catchup",
            "post catchup [<channel> | --mail | --all] [--max-bytes <n>] [--framing auto|full|compact]",
            "framed text; JSON with --json",
            "writer that consumes the complete unread slice for direct mail, one joined channel, or all targets when --max-bytes is absent (no selector means --all); opt-in --max-bytes is one shared final-stdout budget across existing target order and consumes only complete admitted per-target prefixes, with explicit top-level/per-target remainder metadata; captures the fixed delta before stdout and records it only after a successful emit; positional channels require membership and fail closed on an unreadable message, while --all skips an unloadable never-joined channel with a stderr warning and reports a joined broken channel as a zero-count target; --framing applies only to this body-bearing surface and is quiet in auto; explicit full/compact emit one requested banner; a non-empty admitted result redirected to /dev/null is refused",
        ),
        command(
            "search",
            "post search <pattern> [--mail | --channel <channel> | --archived] [--limit <1..=1000>] [--framing auto|full|compact]",
            "framed text; JSON with --json",
            "read-only, cursorless literal case-insensitive Unicode substring search over party-visible direct mail and joined channels; --archived instead searches every archived channel on the host, membership not required, and no mail; --mail, --channel, and --archived are mutually exclusive, membership/party filters apply before message content is opened, results are deterministic newest-first with a default limit of 100 and hard cap of 1000, and previews are sanitized and capped at 160 Unicode scalar values; no mailbox, cursor, or banner state is changed",
        ),
        command(
            "rooms",
            "post rooms [add <name> <path> | set-path <name> <path> [--dry-run]]",
            "JSON",
            "listing is read-only; add locks, validates, and atomically updates rooms.json without editing rules.json; set-path re-points an existing local room's workspace (discovery) path under the same locks and validation (canonical existing directory, refused when another room owns it), never moves mail or history and never rewrites participant records, always refuses remote placeholders in either direction, and with --dry-run reports the change without writing",
        ),
        command(
            "profile",
            "post profile [show [<participant>]] | post profile set [--name <name>] [--pfp <emoji>] | post profile clear | post profile list [--json]",
            "JSON (list: text, JSON with --json)",
            "presentation only — display name and pfp never affect identity, auth, routing, blocks, cursors, or signed-message verification, and every render path keeps the immutable [participant] and (room-id) suffixes visible; a profile belongs to ONE participant: set/clear act on the acting participant and atomically update profiles.json under the rooms lock, keyed participant:<id> (bare workspace keys are legacy, shared by everyone in the workspace, and never stamp; doctor reports them, doctor --fix migrates one to a workspace's sole participant, and a set from that workspace retires it); names are <=32 chars, refuse control/bidi/line-separator characters, and may not imitate the signed owner's room id (legacy fallback reserves 'trey'; feature-absent reserves nothing) or another room id (NFKC skeleton check); pfp is exactly one emoji grapheme, unique across participants and legacy rooms; profiles are stamped into envelopes at send time (renames never rewrite history) after re-validation, so hand-edited registry values never stamp (set also drops, with a warning, a preserved stored field that no longer validates); text bylines render the stamped profile, else the lineage, always with the participant id; a name, pfp, or clear change announces itself as a 'profile' event naming the participant in every channel the room belongs to, with the channel list resolved before the registry commit so a listing failure fails pre-commit; list is read-only and reports every registry entry with its holder, workspace, name, pfp, lease, and holds_sigil, computed with the same predicate set refuses on (a participant entry while its lease is active, a legacy entry while its room is registered), so occupancy is lease-dependent and can change before a set",
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
            "post watch [--room <name>]... [--once | --snapshot [--limit <n>]] [--interval-ms <ms>] [--reason mail|channel|mention]... [--digest] [--text]",
            "NDJSON event union (mail | unreadable | channel_message), one per line; --digest emits one digest per room/source group; text with --text",
            "a multi-address watch merges direct mail and deduplicates channel messages in one stream; a long-running watch requires a bound participant and suppresses only local-origin from_participant == self (remote-origin evidence is never own), while snapshot is the read-only unbound exception; reads envelopes plus bounded sanitized previews — never moves or alters mail, never emits a complete body, and never mutates channel seen-sets; scans are the truth source and a native filesystem watcher (inotify/FSEvents) only supplies wake hints, with a slow periodic re-registration pass and poll fallback at --interval-ms; each long-running poll renews the participant lease and participants/<id>/watch.heartbeat only after write-fence admission, while snapshot is wholly read-only and never does either; events carry reason mail|channel|mention on every type (unreadable: mail|channel); --digest groups each batch by typed address, source (mail or channel:<name>), and pending status in first-arrival order; --snapshot scans exactly once (unread direct mail plus effective-channel messages outside the seen-set) and exits 0 (empty scan emits nothing; direct-mail scan failure is a nonzero error, never a false empty; an unregistered room warns on stderr, scans nothing, and creates no directories); snapshot-only --limit <n> admits the last n underlying events before optional digest grouping and warns when earlier events are omitted, while --limit 0 is unlimited; repeatable --reason mail|channel|mention delivers only events with a selected reason (omitted = all), applied after the scan and before --limit, --digest grouping, and the --once exit check; an unreadable channel message is always reason=channel, never mention, because its body cannot be read",
        ),
        command(
            "who",
            "post who [--room <name>]... [--text]",
            "JSON; text with --text",
            "read-only participant directory: acting participant/provenance first, then all participants with lineage/workspace/watch presence, then legacy room heartbeat rows under legacy_rooms; --text labels each participant's lease state lease=active|stale|ended|no lease record (JSON keeps the state key) and adds one hint line: a lease is not attention, use post chat <channel> --seen-by <message-id>; provenance never claims to detect subagency and no PID is reported",
        ),
        command(
            "version",
            "post version [--json]",
            "one text line; JSON with --json",
            "read-only build/store/capability receipt",
        ),
        command(
            "contract",
            "post contract samples [--dir <path>] | post contract skill-manifest [--verify <path>]",
            "JSON",
            "store-free: reads no mailbox and needs no participant; samples prints the normalized output samples compiled into this binary (contract/samples/, produced by the real commands in the test suite: ids, timestamps, paths, digests, and build_sha replaced by same-format stand-ins, field presence, types, and enum values kept) as one object keyed by file name; --dir <path> creates the directory if missing and writes each sample as <path>/<name>, replacing a same-named file whole, so a consumer's contract tests run against the binary it will actually call; skill-manifest prints the sha256 of every served skill file (skills/post: SKILL.md, references/, hooks/, agents/; dot-files excluded) computed when this binary was built -- an install receipt, not a runtime check; --verify <path> checks a served skill directory against it and records the served root's kind: symlink (checked through the files it resolves to) or copy (checked as served; a file whose source carries skill-render fence markers and differs is listed as rendered_unverified and makes the verdict unverified, not match, because the rendering is not checked); exit 0 on match, 1 on drift (a changed, missing, or extra covered file) or unverified, and an error envelope when the path cannot be read",
        ),
        command(
            "bridge",
            "post bridge deliver --participant <id> --source-host <host> --mail-id <mail-id> --sha256 <hex> --file <path> [--json]",
            "JSON (post.bridge-deliver.v1), always",
            "bridge-only fenced writer; takes no participant and never touches activity. Exit 0 means a decision: exactly one object with outcome=delivered|rejected|retry. Every other exit (usage error 2, crash, missing binary) is a retry for the bridge. Order: structural checks every attempt (argument grammar, --file a regular file of at most 8 MiB, --source-host not this host's bridge host, sha256 of the file equals --sha256, envelope id/to/address_kind=participant/to_host/from_participant without '@'/from room grammar), then under the participants lock the admission record participants/<id>/imports/<mail-id>.json {v, participant, mail_id, source_host, sha256, from_participant, admitted_at}. A valid record with the same source_host and sha256 is a replay: admission checks are skipped and the inbox file is completed or verified. No record: an existing inbox file is id_collision; otherwise the admission checks run once (from must be a placeholder homed under remote/<source-host>/, the participant must exist and not be ended, the route must not be blocked), then the record is written (the admission point), then the inbox file. Terminal reasons: unknown_participant, ended_participant, blocked_route, to_mismatch, forged_from, id_collision, malformed. Retry reasons: participant_unreadable, inventory_degraded (route policy unreadable), import_record_unreadable, digest_mismatch, fenced, topology_unavailable, io_error",
        ),
        command(
            "delivery",
            "post delivery <mail-id> [--json]",
            "text; JSON (post.delivery.v1) with --json",
            "read-only; requires a bound participant and shows only letters that participant sent (anything else, and an id absent from archive/, is not_found, exit 66). Workspace, lineage, and local participant mail is state=unsupported. A participant:<id>@<host> letter is validated against its archive bytes and the bridge's evidence on this host: bridge/pmail-acked/<id>.json (exact keys v, status, origin, host, participant, id, sha256, reason, at; origin = this host, host = to_host, participant and id match, sha256 = archive digest, status delivered with reason null or rejected with a terminal reason: malformed, to_mismatch, forged_from, unknown_participant, ended_participant, blocked_route, id_collision, or, decided by the destination bridge before deliver runs, name_collision or unpublished_sender) gives received|rejected; else bridge/pmail-published/<id>.json ({v:1, id, host, sha256, commit, at}, written after the push) gives published with commit and age_s; else queued, with blocked_reason/last_error from bridge/pmail-status/<id>.json ({v:1, id, blocked_reason?, last_error?, at?}). A receipt outranks the marker. Any evidence file that exists but does not validate gives state=unknown with evidence_file and evidence_error, never a guess. bridge/pmail-conflicts/<id>.json present sets conflict=true (the first receipt stands)",
        ),
    ];
    let output_shapes = OutputShapes {
        participant: fields(&[
            "show/bind/touch/end: ok, status=bound|unbound|ended, id?, participant? (last_seen?, lease_hours, ended_at?), provenance? (explicit-bootstrap for --new/--key), fix?, participant_error?",
            "list: ok, participants, count",
            "notice: ok, notice=string|null, busy; plain query is read-only; --claim PID reserves delivery under the registry lock (busy for a live competing owner), --release PID releases only that owner, --ack records delivery; all three flags are fenced writers; dead owner claims are reclaimable",
        ]),
        identity: fields(&[
            "list: ok, lineages without voice bodies",
            "show: lineage metadata and affiliates; voice bodies only with --voices",
            "new/continue/leave: acting participant affiliation and acknowledgement state",
            "voice add/withdraw and terms set: lineage content state",
        ]),
        version: fields(&[
            "ok",
            "version",
            "build_sha",
            "store_version=2",
            "capabilities",
        ]),
        doctor: fields(&[
            "ok",
            "status",
            "root",
            "checks",
            "count",
            "fixed",
            "exit_codes",
            "participant",
            "pending",
            "participant_fix (when no participant is bound)",
            "participant_error (when ambient participant resolution failed)",
        ]),
        inbox: fields(&[
            "ok",
            "room",
            "participant",
            "unread[] (id, from, origin, reply_to_participant?, reply_to_shared, kind, subject, sent, display_name?, pfp?, sender_address?, sender_provenance?, from_participant?, from_lineage?)",
            "count",
            "skipped_unreadable",
            "unread_count",
            "pending",
            "pending_by_address{address:count}",
            "held",
        ]),
        read_json: fields(&[
            "ok",
            "framing",
            "envelope (id, from, to, kind, subject, sent, from_participant?, from_lineage?, address_kind?, display_name?, pfp?, sender_address?, sender_provenance?, origin, reply_to_participant?, reply_to_shared, pending?, address{kind,name}?)",
            "body",
            "own (when true)",
            "pending (when true)",
            "already_read (present and true only when the participant cursor contains the exact id)",
        ]),
        read_budget: fields(&[
            "ok",
            "framing",
            "envelope (id, from, to, kind, subject, sent, from_participant?, from_lineage?, address_kind?, display_name?, pfp?, sender_address?, sender_provenance?, origin, reply_to_participant?, reply_to_shared, pending?, address{kind,name}?)",
            "body (only when complete)",
            "own (when true)",
            "pending (when true)",
            "already_read (when true)",
            "count",
            "selected_count",
            "has_more",
            "byte_limit",
            "omitted? (reason, count, source, first_id, first_body_bytes, mention_count, continuation)",
        ]),
        read_slice: fields(&[
            "ok",
            "framing",
            "envelope (id, from, to, kind, subject, sent, from_participant?, from_lineage?, address_kind?, display_name?, pfp?, sender_address?, sender_provenance?, origin, reply_to_participant?, reply_to_shared, pending?, address{kind,name}?)",
            "body_slice",
            "range (start, end_exclusive)",
            "total_body_bytes",
            "body_complete",
            "next_offset",
            "continuation?",
            "already_read (when true)",
            "own (when true)",
            "pending (when true)",
            "verification_scope=stored_full_body",
            "byte_limit",
        ]),
        read_ack: fields(&["ok", "room", "id", "already_read", "acknowledged"]),
        rooms: fields(&[
            "ok",
            "rooms",
            "count",
            "set-path: ok, room, before, after, changed, dry_run",
        ]),
        schema: fields(&[
            "ok",
            "name",
            "contract_version",
            "store_version",
            "capabilities",
            "participant=unbound and participant_fix (when no participant is bound)",
            "participant_error (when ambient participant resolution failed but this read-only command remained available)",
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
        send_json: fields(&[
            "ok",
            "envelope",
            "archived",
            "delivery? ({state=queued, host}; participant:<id>@<host> sends only; never claims remote delivery)",
        ]),
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
            "selected_count (with --max-bytes)",
            "byte_limit (with --max-bytes)",
            "omitted? (reason, count, source, channel, first_id, first_body_bytes, mention_count, continuation)",
        ]),
        chat_slice: fields(&[
            "ok",
            "framing",
            "channel",
            "room",
            "message (complete stored envelope only)",
            "origin",
            "reply_to_participant?",
            "reply_to_shared",
            "body_slice",
            "range (start, end_exclusive)",
            "total_body_bytes",
            "body_complete",
            "next_offset",
            "continuation?",
            "signed_verified?",
            "verification_scope=stored_full_body",
            "byte_limit",
        ]),
        chat_ack: fields(&["ok", "channel", "room", "id", "acknowledged"]),
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
            "targets[].selected_count (with --max-bytes)",
            "targets[].has_more (with --max-bytes)",
            "targets[].framing (source, authority, laws)",
            "targets[].messages[] (mail: envelope, body; channel: id, from, channel, subject, sent, body)",
            "count",
            "selected_count (with --max-bytes)",
            "has_more (with --max-bytes)",
            "byte_limit (with --max-bytes)",
            "omitted? (reason, count, source, channel?, first_id, first_body_bytes, mention_count, remaining_targets, continuation)",
        ]),
        search: fields(&[
            "ok",
            "participant",
            "pending",
            "framing (source, authority, laws)",
            "room",
            "pattern",
            "match",
            "results[] (source, channel?, id, from, from_participant?, from_lineage?, origin, reply_to_participant?, reply_to_shared, sent, subject, preview, matched, own?, pending?, already_read?, kind? for mail)",
            "count",
            "limit",
            "truncated",
        ]),
        channels: fields(&[
            "ok",
            "channels (name, created, created_by, description?, members=workspace addresses, participants=host-local ids, messages, room, unread, archived, archived_at?, archived_by?)",
            "count",
            "archived_hidden",
            "participant",
            "pending",
        ]),
        profile: fields(&[
            "ok",
            "room",
            "profile (name?, pfp?)",
            "announced (set/clear; channels that received the change event)",
            "list: ok, profiles[] (key, participant?, workspace?, name?, pfp?, lease? (active|stale|ended|no lease record|no participant record; absent for legacy), legacy?, holds_sigil)",
        ]),
        watch: fields(&[
            "mail: event, address{kind,name}, room? (workspace only), id, from, from_participant?, from_lineage?, origin, reply_to_participant?, reply_to_shared, pending?, kind, subject, sent, reason=mail, preview?",
            "unreadable: event, address{kind,name}, room? (workspace only), id, reason=mail|channel, channel? (required for channel; no preview)",
            "channel_message: event, address{kind,name}, room? (workspace only), channel, id, from, from_participant?, from_lineage?, origin, reply_to_participant?, reply_to_shared, subject, sent, reason=channel|mention, preview?",
            "digest: event=digest, address{kind,name}, room? (workspace only), source=mail|channel:<name>, pending?, count, first_id, last_id, from, reason=mail|channel|mention|mixed, preview? (text preview precedes bounds/since suffix)",
        ]),
        delivery: fields(&[
            "ok",
            "schema=post.delivery.v1",
            "id",
            "state=queued|published|received|rejected|unknown|unsupported",
            "participant? (the letter's to)",
            "host? (the letter's to_host)",
            "sha256? (archive digest)",
            "conflict",
            "reason? (rejected: the terminal reason; unsupported: why)",
            "blocked_reason? (queued)",
            "last_error? (queued)",
            "commit? (published)",
            "published_at? (published)",
            "age_s? (published: seconds since published_at)",
            "acked_at? (received|rejected)",
            "evidence_file? (unknown)",
            "evidence_error? (unknown)",
        ]),
        bridge: fields(&[
            "deliver: ok=true, schema=post.bridge-deliver.v1, outcome=delivered|rejected|retry, reason (null for delivered), participant, mail_id, source_host, sha256 (computed by post), admitted_at (RFC3339 UTC; non-null exactly for delivered), replay, detail (null for delivered; at most 512 chars)",
        ]),
        contract: fields(&[
            "samples: ok, samples{<file name>: <sample text>}",
            "samples_dir: (samples --dir) ok, dir, samples[<file name>]",
            "skill_manifest: ok, root, covered, count, files[{path, sha256, fenced}]",
            "skill_verify: (skill-manifest --verify) ok, verdict=match|drift|unverified, served, kind=symlink|copy, resolved, checked, mismatched, missing, extra, rendered_unverified",
        ]),
        who: fields(&[
            "ok",
            "participant (status, state?, last_seen?, id?, harness?, provenance?, workspace?, lineage?, unread{address:count}, pending{address:count}, fix?)",
            "participants (id, harness, state, last_seen?, lineage?, workspace?, unread{address:count}, pending{address:count}, live_watch, watch_last_seen?)",
            "legacy_rooms (room, live_watch, last_seen?)",
            "activity_note? (stale-delivery crash gap: frozen mail is not reassigned)",
            "count",
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
        store_version: 2,
        capabilities: version::CAPABILITIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        global_flags: fields(&[
            "--json: switch send/read/chat/catchup/search from text to JSON; inbox/rooms/channels/profile/schema/doctor/who are already JSON",
            "--pretty: pretty-print JSON",
            "--room <name>: command option for inbox/read/watch/who only; it selects an address while the participant binding remains the actor (never cwd); chat and channels reject --room",
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
            exit(69, "command unavailable"),
            exit(70, "non-retryable post-commit or internal failure"),
            exit(75, "retryable I/O failure"),
            exit(77, "blocked route"),
            exit(78, "invalid configuration or mail state"),
        ],
        doctor_exit_codes: doctor_exit_codes(),
        laws: fields(&[
            crate::participant::ACTIVATION_NOTICE,
            "Blocked routes refuse before any mail write.",
            "Registered room names cannot be claimed from outside their room tree.",
            "One canonical workspace path can have only one registered room name.",
            "Every successful send has an immutable archive copy.",
            "delivered_output_failure is non-retryable after a committed direct send or channel mutation; committed room registration stdout failure is reported as success with best-effort diagnostics.",
            "Mail kinds are exactly letter, note, and signal.",
            "Only post participant bind mints a participant; read-only commands never mint or initialize mailbox state.",
            "A shell without a harness key bootstraps with participant bind --harness <slug> --key <conversation-key> or participant bind --new [--harness <slug>]; text prints one export POST_PARTICIPANT line.",
            "A participant binding, never cwd, determines the sender. Workspace context supplies the shared reply address; otherwise the participant id is the reply address.",
            "Bare send targets resolve registered workspace, then lineage, then participant; typed workspace:, lineage:, and participant: targets remove ambiguity without changing --kind.",
            "Channel messages are not mail: they carry no kind, so a signal structurally cannot occur in a channel; anything gate-grade stays 1:1 room mail.",
            "Blocked routes bar shared channel membership at join time; channels never carry what a route may not.",
            "Channel history is append-only and is its own immutable archive; nothing in messages/ is ever moved or deleted.",
            "A bound participant determines channel identity, including session-only participants without a workspace. Membership and seen state are participant-scoped; legacy members.json supplies only a workspace default with individual opt-out.",
            "Watch emits channel events as notifications only and never marks channel messages seen; only a read consumes, and only after a successful emit.",
            "A participant's own channel messages never ring it. Long-running watch requires a bound participant and suppresses only local-origin from_participant == self (remote-origin evidence is never own); snapshot is the read-only unbound exception.",
            "A message body comes from exactly one of --body, --body-file, or stdin; a body-file path that does not exist is a usage error, never a retryable I/O fault.",
            "Shell quoting happens before Post: double quotes can expand dollar-positionals such as $1 in $1.63B, and an apostrophe can terminate single quotes; use --body-file or stdin for shell-sensitive prose.",
            "Subjects over 1 KiB fail before any write with no override; longer text belongs in the body.",
            "Message bodies over 32 KiB fail before any write unless --oversize records explicit intent; complete Post watch-event NDJSON lines warn on stderr but still send.",
            "Already-read participant mail stays in its canonical inbox and remains retrievable by id or prefix when the frozen receipt names the participant or the participant is the sender; re-reading consumes nothing and reports already_read from the exact-id participant cursor. Legacy room read/ and archive copies are not participant retrieval sources.",
            "A channel read refuses to consume its unread batch into /dev/null; skipping unread messages requires --discard or --discard-through.",
            "Participant cursor writes hold the hardened participants/<id>/.cursors.lock protocol across reload, exact-set union, and atomic replace; concurrent acks cannot lose each other and a late older id remains unread.",
            "Participant cursor state is participants/<id>/cursors.json v2 with exact per-address mail and per-channel seen sets; legacy room cursor, read/, and channel-state files are read-only history and are never imported into participant cursor state.",
            "Missing or malformed participant cursor state degrades read-only loads to an empty snapshot (all eligible messages are unread) with one stderr warning; a writer refuses unsafe cursor or lock paths. Legacy room state remains untouched as rollback evidence; no import, materialization, or dual writes occur.",
            "Catchup is a fence-admitted writer; search, inbox, channels, schema, doctor without --fix, and watch --snapshot are read-only. Search is participant-visible history: frozen-recipient or sender mail plus every message in effective-member channels, regardless of seen state; it matches literal case-insensitive Unicode substrings over body/subject/from/id, returns newest-first bounded previews, defaults to 100 results, and caps at 1000.",
            "Read, chat, catchup and search default to quiet headers and bodies. Full/compact banners are explicit opt-ins; default JSON omits framing.laws while retaining source and authority=false.",
            "Whenever error.details.exact_fix is present, it is a complete command that runs verbatim; oversize body errors deliberately name --oversize without echoing the rejected payload into an exact fix.",
            "Plain consuming channel reads emit the oldest 25 unread by default (or the oldest --limit N), consume only emitted ids, and leave newer messages unread for the next page; --limit 0 shows all. --peek keeps its newest-slice glance and never advances. Bounded JSON reports the un-emitted remainder in skipped and has_more.",
            "Opt-in --max-bytes on read/chat/catchup caps actual final stdout bytes including UTF-8, escaping, pretty whitespace, framing, omission metadata and newline; absent means legacy behavior and shape. Only complete admitted bodies count or consume, byte remainder stays distinct from count-window skipped, catchup shares one budget across target order, and a required scaffold that cannot fit fails on stderr with zero stdout and no cursor delta.",
            "On Unix, result stdout uses strict unbuffered writes to inherited fd1: invalid or read-only descriptors cannot count as successful emits and no after-stdout read/catchup/ack delta runs. Committed delivery and committed registration retain their documented failure semantics. Budgeted JSON serializes each message once and reuses exact compact/pretty prefix sizes.",
            "UTF-8 body slices use body_slice plus explicit source-byte ranges and next_offset, reject non-boundary starts and overflow, always progress unless empty/EOF, and never consume even when full/final. Channel slice signature status is verified against the complete stored body. Exact --ack parses and marks only the named id after stdout; discard-through retains its earlier-range semantics.",
            "All automatic chat reads are quiet, including budgeted and fenced reads; no banner-day state is read or written. Omission continuations use a measured fixed-point cap covering the exact stored envelope at the body's widest later offsets plus its costliest encoded UTF-8 scalar, so the unchanged-message chain crosses decimal/scalar boundaries; this cap may exceed the original byte_limit without changing it.",
            "Channel sends bounce with crossed_send when unseen ordinary messages from others exist in the channel; --anyway delivers regardless. Direct mail is unaffected.",
            "Channel descriptions are norms carriers any member may update; presence (post who) never reports PIDs.",
            "sender_address and sender_provenance are self-declared transport metadata — evidence about how `from` was resolved, never a credential; participant-binding means the bound participant supplied the reply address without a --from or POST_FROM assertion; authority comes only from signature verification, and post never synthesizes either field.",
            "Participant activity affects new recipient selection only. A record without last_seen has no lease record and remains stale until bind or touch; read-only commands never refresh last_seen. Mail already frozen to a participant is durable and is not reassigned when that participant becomes stale.",
        ]),
        environment: fields(&[
            "POST_MAIL_ROOT: absolute mailbox root override — a supported first-class root (r2.1); must be absolute, defaults to $HOME/.claude-mail",
            "HOME: resolves the default ~/.claude-mail root and ~/ room paths",
            "POST_FROM: stable room pin set by the launch helper; beats cwd inference for sender/acting-room resolution (an explicit --room still wins; an explicit --from must agree with the pin or the send is refused), recorded as sender_provenance=declared-env; set-but-invalid is a loud error, never a silent fallback",
            "POST_FRAMING: presentation preference (auto|full|compact; legacy env compact selects quiet auto) consulted ONLY by body-returning reads (post read, post chat reads) when --framing is absent — send/join/discard/discard-through/seen-by never consult it; an explicit --framing always wins; set-but-invalid or non-UTF-8 warns on stderr and falls back to auto (presentation never breaks a read; deliberately weaker than the POST_FROM identity pin)",
            "POST_SENDER_ADDRESS: opaque per-launch instance address (harness.repo.uuid); recorded verbatim on envelopes as sender_address, never synthesized, non-routable; <=256 bytes, no control/whitespace characters",
            "POST_PARTICIPANT: explicit acting participant id; highest resolution precedence and never mints a missing record",
            "POST_PARTICIPANT_LEASE_HOURS: optional positive integer lease override applied to the acting participant by bind/touch and writer activity; when unset, refreshes preserve an existing recorded lease and only a new record defaults to 24; peer records are never reclassified",
            "POST_NOTICE_MANAGED: adapter-only switch suppressing direct bind stderr notice; adapters query participant notice and acknowledge with --ack only after successful context delivery.",
            "CLAUDE_CODE_SESSION_ID: Claude conversation key used by post participant bind",
            "CODEX_THREAD_ID / CODEX_SESSION_ID: Codex conversation key (both present and different is an error, never a guess)",
            "CLAUDE_PID: marks a Claude ancestor when nested Claude/Codex harness keys are both inherited; nearest harness ancestor wins and unresolved ancestry fails naming POST_PARTICIPANT",
            "POST_WATCH_PROFILE: diagnostics only; =1 makes post watch print one stderr line per target scan (post: watch profile: room=… mode=complete|wake|room mail_snapshot_ms mail_files channel_enum_ms channels channel_scan_ms channel_files events total_ms); the line format is not a contract, stdout and the store are unchanged, and any other value is off",
            "POST_HARNESS: optional harness label for POST_SENDER_ADDRESS and participant bind --new only; native Claude/Codex labels are canonical",
            "POST_ARX_GENERATION: positive migration generation for writers only; reads never parse or reject it. Missing/zero/stale/malformed declarations refuse enrolled writers before mutation; absent state plus an unset declaration preserves legacy writes. Enrolled reads are non-mutating. The enrollment-owned .post-arx.json state, existing solitary .post-arx.lock flock anchor, and actual ..post-arx.json.<pid>.<nonce>.tmp atomic temp namespace are reserved room names; no lock temp namespace is produced; the lock inode is never unlinked or recreated. Cursor state is a fence boundary: catchup requires writer admission, while search and listings remain read-only; legacy cursor state is never imported or materialized into participant state and remains untouched as rollback evidence.",
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
