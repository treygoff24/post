use super::version;
use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::mailbox::{load_owner, Context, OwnerResolution};
use crate::output::{
    CommandSchema, ErrorSchema, ExitSchema, OutputShapes, OwnerResolvedSchema, OwnerSchema,
    SchemaOutput,
};

// Fields several commands share. Each is one shape line, so the wording cannot
// drift between the commands that carry it.

/// A session with no participant record yet, reported by a read-only command.
const UNBOUND: &str = "bound? (false, together with participant=null and a hint, when the session has no participant yet: read-only commands answer with this marker and exit 0; absent when bound)";

/// The `framing` object every body-returning read carries.
const FRAMING: &str =
    "framing ({source=multiple_ai_agents, authority=false, laws? (only with --framing full)})";

/// The `envelope` object of a send receipt, the same fields a mail file carries.
const SEND_ENVELOPE: &str = "envelope ({id, from, to, kind, subject, sent, from_participant?, from_lineage?, address_kind?, to_host?, display_name?, pfp?, sender_address?, sender_provenance?})";

/// A claim that names a record that does not exist.
const PARTICIPANT_MISSING: &str = "participant_missing? ({claim=POST_PARTICIPANT|session-index, id?, message, suggested_fix, exact_fix?}; present when an explicit claim names a record that does not exist: participant show, who, doctor, and read-only listings report it here and exit 0; every other command that acts as a participant first brings back a record `participant gc` collected, as the same participant (a write reports bound_now), and fails with the participant_missing error, exit 65, only for a claim nothing ever held, its suggested_fix naming the rebind command)";

/// A write command that created the session's participant record on the spot.
const BOUND_NOW: &str = "bound_now? ({id, workspace?}; present when this write command created the session's participant record because none existed, exactly as `post participant bind --harness <h> --key <key>` would, then proceeded; text mode says so in one line)";

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
            "post participant show | post participant bind [--workspace <room>] [--harness <slug> --key <conversation-key> | --new [--harness <slug>]] | post participant touch | post participant end | post participant list | post participant gc [--apply] | post participant restore <id> | post participant notice [--ack | --claim <pid> | --release <pid>]",
            "JSON",
            "show/list are read-only; bind is the only participant minting path and refreshes last_seen, preserves an existing lease_hours unless POST_PARTICIPANT_LEASE_HOURS is explicit (new records default to 24; a `bind --new` record is ephemeral, with lease_hours 1 and ephemeral true, so it ages out and `participant gc` collects it after 24 hours instead of 7 days), clears ended_at, and commits participant.json before its by-session index under .participants.lock; touch refreshes last_seen and likewise preserves the recorded lease unless explicitly overridden; end sets ended_at idempotently; gc collects participant records that hold nothing and is a dry run unless --apply (--apply is a fenced writer; both answer {ok, applied, deleted[], archived[], kept{reason: count}} from one plan): tier 1 deletes a record that is not active, was last seen more than 7 days ago (24 hours when ephemeral), holds only scaffolding, and that nothing names, leaving a tombstone line in participants/archived.jsonl that keeps the id occupied for its key; tier 2 moves a record last seen more than 30 days ago that has state but no unread, pending, or held mail to participants-archive/<id>/; an active lease, a fresh watch heartbeat, a lineage's current holder, a doorbell subscription, an in-flight outbound bridge letter, and any unread, pending, or held mail keep the record, and --apply rechecks each candidate under the participants lock right before acting, so anything that arrived since planning keeps its participant and counts as kept.changed; a collected record comes back under the same id on the next bind for its key, when an explicit POST_PARTICIPANT names it, when a direct send or bridge delivery targets it, or on request with restore <id> (a fenced writer like gc --apply; idempotent: an id already present answers restored=false and writes nothing; an archive returns whole with its state, a tombstone is recreated with the same id, lease, workspace, and display name; an id nothing ever held, or a collected record that cannot be brought back, fails participant_missing, exit 65, and creates nothing)",
        ),
        command(
            "identity",
            "post identity list | post identity show <name> [--voices] | post identity new <name> | post identity continue <name> [--acknowledge] | post identity leave | post identity voice add --body-file <path> | post identity voice withdraw [--lineage <name>] | post identity terms set --body-file <path>",
            "JSON",
            "list/show are read-only and voice bodies load only with --voices; new/continue/leave change only the acting participant's historical affiliation; continue requires --acknowledge when terms exist; voice and terms bodies come only from the named files",
        ),
        command(
            "send",
            "post send --to <workspace:<room>|lineage:<name>|participant:<id>|participant:<id>@<host>|bare-name> [--from <name>] [--kind letter|note|signal] [--subject <s>] [--oversize] [--allow-self] (<body> | --body <text> | --body-file <path> | --body-file - | stdin)",
            "text; JSON with --json",
            "atomically writes the resolved address's canonical inbox plus archive/<id>.mail, then publishes an atomic routing receipt when recipients exist; workspace/lineage fan-out suppresses only the sending local participant (remote-origin mail never excludes a local id), while an explicit participant:<self> target is readable; --allow-self (hidden from --help; delegate's completion pings pass it) retargets a send whose --to is the sender's own room or lineage to the sender's own participant inbox, says so in the receipt (text note, and under --json a retargeted object {from, to, note}), reaches nobody else in that room, and does nothing for any other target; a bare <body> argument is the body, the same as --body (the recipient is always --to; a bare argument that looks like a path -- one token, no whitespace, containing / or ending in a file extension such as .md, .txt, or .json; a URL is text -- is refused with the --body-file command that sends it, whether or not the file exists; --body or stdin carries a literal path); prefer stdin (a quoted heredoc) or --body-file, and keep --body for short plain one-liners; a send that landed exits 0 even when its receipt cannot be written to stdout (a note on stderr says so; the mail exists exactly once), under --json stderr stays empty and anything worth a second look rides in the receipt's warnings; subjects over 1 KiB fail, body forms are exclusive, and --from that disagrees with the bound reply address is refused; participant:<id>@<host> (split at the last @; an exact local participant record of that full name stays local; this host's bridge host resolves as participant:<id>) queues a letter for an enrolled peer host: the sender must be bound to a registered local room (remote_sender_unroutable otherwise), bridge/health.json must be fresh (ticked_at at most 3x interval_s old and at most 5 s ahead) and list typed-outbound-exclusion and participant-mail-v1 (bridge_unsupported when a fresh file lacks one; retryable bridge_status_unavailable when missing, malformed, or stale), a letter over 1 MiB (the bridge's per-letter cap) is invalid_argument even with --oversize, and a local rule blocking this sender (or *) to * applies; the letter (to=<id>, address_kind=participant, to_host=<host>) is written only to archive/<id>.mail, never to a workspace or participant inbox or outbox/, is not routed, and the receipt says delivery.state=queued; the enrolled set is bridge/registry/hosts.json minus this host, intersected with bridge/config.json peers when non-empty, with no fallback to config peers (retryable topology_unavailable when the registry or config is missing or invalid; unknown_host lists the enrolled hosts in details.matches; no_bridge without bridge/config.json); a host-qualified error never falls back to a room, lineage, or bare id",
        ),
        command(
            "chat",
            "post chat <channel> --send [--re <id>] [--subject <s>] [--oversize] [--signature-ref <tag>] (--body <text> | --body-file <path> | --body-file - | stdin) | post chat <channel> [--peek | --limit <n> | --history <n> [--grep <pat>] | --since <id>] [--max-bytes <n>] [--framing auto|full|compact] | post chat <channel> --message <id> [--offset <b>] [--length <b>] --max-bytes <n> [--framing auto|full|compact] | post chat <channel> --ack <id> | post chat <channel> --discard | post chat <channel> --discard-through <id> | post chat <channel> --seen-by <id> | post chat <channel> --join [--description <text>] [--backlog] [--create] | post chat <channel> --leave | post chat <channel> --archive | post chat <channel> --unarchive",
            "framed text; JSON with --json",
            "--join creates the channel on first join (a new name is normalized to lowercase with spaces and underscores as hyphens and the receipt says normalized_from; other characters are refused with the normalized form as the fix; a name that is a near-duplicate of an existing channel is refused with `did you mean #<existing>` and the exact join command unless --create forces the new channel; a leading # is accepted and ignored) and records the join as an event in history; a join starts unread at the join instant -- messages that predate it are history, still readable via --history/--peek/--grep/--since/search -- and the response reports history_before_join plus a runnable history_hint, while --join --backlog (valid only with --join) records the minimum membership start so the whole backlog stays unread (the pre-change behavior); from an explicit member, --join --backlog records nothing and reports already_member, backlog_ignored=true, and a history_hint that runs --leave then --join --backlog; --leave opts out only the acting participant and preserves its seen set; --archive/--unarchive (any bound participant, membership not required, idempotent) set or clear the host-local archive mark in channels/<name>/archive.json -- never history, never deleting -- whose append-only log records who and when; a channel stays archived until a conversational (non-event) message newer than the mark arrives, so a post resurrects it while join/profile events do not; --description (with --join) sets/updates the channel norms carrier (any member, cap 1 KiB); --send atomically writes channels/<name>/messages/<id>.msg, rejects subjects over 1 KiB, implies from --body/--body-file, requires --oversize above 32 KiB, stamps @mentions of registered rooms and optional --re parent id, and always delivers: when unseen messages from others crossed the send, the receipt carries a `crossed` block {unseen, addressed_to_you, messages: up to 10, newest last, each id/from/display_name/sent/addressed_to_you/body} whose body is complete (the stored body byte for byte, trailing whitespace kept, so it is the same bytes signed_verified covers) for a message ADDRESSED to the sending room -- an @mention of it, a reply (--re) to something it wrote, or any message from the owner room, whose signed bodies cannot carry a mention without invalidating the signature -- and a 300-character preview otherwise (owner-room messages also carry signed_verified, and messages carry sender_address/sender_provenance when declared); the crossed messages stay unread, text mode prints them after the sent line with addressed ones first and in full, each crossed send appends one JSON line (outcome delivered_crossed) to <root>/crossed-send.jsonl carrying room, channel, unseen and targeted, unparseable unseen files are listed in the receipt's `skipped` ({id, reason}), and --anyway is a hidden no-op kept so old commands still run; a plain consuming read emits the oldest 25 unread by default (or the oldest --limit N), consumes only emitted ids, and leaves newer messages unread for the next page; --limit 0 = all; --peek retains its newest-slice glance and never advances; opt-in --max-bytes caps final stdout and admits only a contiguous prefix of complete selected messages while keeping count-window skipped separate from byte omission and reporting omitted mention count; --message returns a cursorless UTF-8 body_slice with explicit byte range/progress, verifies signature status against the complete stored body, and never consumes; --ack parses and marks exactly one id seen after stdout; --discard marks all currently unseen messages seen without emitting bodies; --discard-through <id> marks every currently unseen message at or before one message (full id or a prefix unique in that channel) seen, refuses when an unreadable unseen message sits in that range, is replay-safe (an already-seen range succeeds with advanced=false), and reports prior_cursor and cursor as max-seen-id summaries (never the model); --seen-by lists members whose seen-set contains an id (read-only; a member left out of the roster because its membership file is invalid is named in `skipped` ({id, reason}, the participant id) on stdout, and the text form prints a one-line notice); --history/--since are cursorless; --grep filters --history by case-insensitive regex; a cursor-advancing read into /dev/null is refused; a read (plain, --peek, --history/--since, --discard, --message) refuses stdin that carries input before routing or marking anything seen -- a nonempty file, or a pipe/heredoc/socket with a queued byte, is invalid_argument (exit 2); a pipe still open and silent after a bounded readiness wait of at most 100 ms is input_ambiguous (exit 2) -- and never sends; exact_fix runs the send correction (--send --body-file -) and names the read correction (stdin from /dev/null); an interactive terminal, /dev/null, an empty file, and a pipe at EOF read normally with no wait; a producer slower than the wait is refused as ambiguous, and no finite wait can detect every delayed producer; activation notice is once per participant; default reads are quiet; --framing (body-returning reads only, rejected on --send/--join/--leave/--discard/--discard-through/--seen-by/--ack) selects auto (quiet default), full (explicit complete banner), or compact (explicit condensed banner); absent the flag, POST_FRAMING supplies the mode (legacy env compact selects quiet auto); invalid values warn and use auto; default JSON preserves source/authority but omits laws; no mode consults or stamps banner-day; channel messages from the OWNER room whose first line matches the signed-wire grammar <marker>🔏 <text> [signed:TS] are verified against the resolved owner's sidecar — see the schema `owner` block for sigs/ + allowed_signers (legacy fallback: a registered 'trey' room, sidecar at its registered path like ~/.trey-room); signed-v2: --signature-ref <tag> stamps the envelope locator {\"version\":2,\"tag\":<tag>} for detached-manifest verification of the raw body (multiline/arbitrary text; ≤1 MiB final body, a protocol cap --oversize does NOT lift; the locator is metadata, never a verdict — owner v2 messages verify at read against <sidecar>/sigs/<tag>.txt binding tag+channel+bytes+sha256, and any malformed owner locator fails loudly)",
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
            "writer that consumes the complete unread slice for direct mail, one joined channel, or all targets when --max-bytes is absent (no selector means --all); holds .rename.lock shared from dispatch through its after-stdout commit, so it waits for a running rooms rename; opt-in --max-bytes is one shared final-stdout budget across existing target order and consumes only complete admitted per-target prefixes, with explicit top-level/per-target remainder metadata; captures the fixed delta before stdout and records it only after a successful emit; positional channels require membership and fail closed on an unreadable message, while --all skips an unloadable never-joined channel with a stderr warning and reports a joined broken channel as a zero-count target; --framing applies only to this body-bearing surface and is quiet in auto; explicit full/compact emit one requested banner; a non-empty admitted result redirected to /dev/null is refused",
        ),
        command(
            "search",
            "post search <pattern> [--mail | --channel <channel> | --archived] [--limit <1..=1000>] [--framing auto|full|compact]",
            "framed text; JSON with --json",
            "read-only, cursorless literal case-insensitive Unicode substring search over party-visible direct mail and joined channels; --archived instead searches every archived channel on the host, membership not required, and no mail; --mail, --channel, and --archived are mutually exclusive, membership/party filters apply before message content is opened, results are deterministic newest-first with a default limit of 100 and hard cap of 1000, and previews are sanitized and capped at 160 Unicode scalar values; no mailbox, cursor, or banner state is changed",
        ),
        command(
            "rooms",
            "post rooms [add <name> <path> | set-path <name> <path> [--dry-run] | rename <old> <new> [--dry-run]]",
            "JSON",
            "listing is read-only; add locks, validates, and atomically updates rooms.json without editing rules.json, and when the refused name is a case-folded duplicate of a remote placeholder the refusal names the owning host in details.host and carries a `post rooms add <name>-<suffix> <path>` exact_fix where a suffix is derivable, and a name a peer host publishes on this host's bridge (bridge/rooms/peers/<host>.json, or an entry for another host in bridge/rooms/owners.json; matched ASCII case-insensitively; a listed name refuses on any evidence, and the refusal says how long ago the bridge last confirmed its state when bridge/health.json is not fresh; a name the files do not list is accepted, with a warnings entry on stdout (add) or in the receipt (rename) saying peer names could not be verified and how old the evidence is when the bridge's health is stale or missing or a publication is absent or unreadable; a host without bridge/config.json is never checked) gets the same refusal from add and from rename's new name (details.host, exact_fix <name>-<this host>; resuming an interrupted rename is exempt); set-path re-points an existing local room's workspace (discovery) path under the same locks and validation (canonical existing directory, refused when another room owns it), never moves mail or history and never rewrites participant records, always refuses remote placeholders in either direction, and with --dry-run reports the change without writing; rename moves <root>/<old> to <root>/<new>, rewrites every live reference to the name (participant workspace fields, participant cursor workspace keys, channel members.json keys, bare profiles.json keys, and the address.name of each moved routing/<id>.json receipt bound to workspace:<old>), commits rooms.json last inside the rollback (any failure through the rooms.json write restores rewritten files byte-for-byte and moves the mailbox back; a failure during the rollback itself warns on stderr per file), writes <root>/rename-journal.json ({v:1, old, new, started_at}) before its first store change and removes it after the commit or a clean rollback, resumes an interrupted rename when rerun with the same pair (skipping a move that already happened, re-planning idempotently, resumed:true), refuses every other rename while the journal stands (invalid_argument with exact_fix `post rooms rename '<old>' '<new>'`), makes every writer that would create or write <root>/<old> or <root>/<new> (send to either name, legacy room mailbox and cursor writes) refuse with config_invalid and that exact_fix while creating nothing (doctor reports rooms.rename_interrupted and, when <root>/<old> exists again, rooms.rename_old_recreated) and refuses a resume whose <root>/<old> was recreated (invalid_argument listing its files in details.matches; never merges), refuses placeholders/case-only renames/an existing <root>/<new>/owner.json or rules.json naming the old room, and on a bridged host requires a fresh bridge/health.json whose local_held counters are integer 0 and a bridge/local-held/<id>.json hold for every archive letter the bridge would export for <old> (workspace-addressed, no to_host key, to == <old>, no received/published/delivered marker; retryable bridge_guard_unavailable naming the count and up to 8 ids in details.matches) — history keeps the old name; --dry-run runs every check and writes nothing",
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
            "post doctor [--fix] [--brief] [--severity warn|error]",
            "JSON; one summary line with --brief",
            "read-only unless --fix; --fix only creates the missing root, archive, and default config files (a room's inbox/ and read/ appear with its first mail and are neither reported nor created); --severity warn drops info lines and --severity error lists errors only, but status, count, ok, and the exit code always cover every check (severity_filter names the threshold and filtered_out counts the findings it hid, so `--severity error` exits 1 while a warning remains); expired participants are one info line (participants.stale, with a `post participant gc` preview), bridge/health.json attention items are warnings carrying the bridge's own fix (bridge.attention.<kind>[.<id>]), and on a bridged host (bridge/config.json present) a health file that is missing, malformed, or has no attention list is itself a bridge.health_unreadable warning with a fix (an unbridged host is silent), and a served post skill (~/.agents/skill-library/post) that differs from the one this binary was built with is a skill.drift warning; --brief prints `post doctor: ok (N checks)` or `post doctor: N findings (run post doctor for detail)` (`N findings, M of them hidden by --severity error` under a filter) with exit codes unchanged",
        ),
        command(
            "watch",
            "post watch [--room <name>]... [--own <room>]... [--once | --snapshot [--limit <n>]] [--from now] [--interval-ms <ms>] [--reason mail|channel|mention]... [--digest] [--text]",
            "NDJSON event union (mail | unreadable | channel_message), one per line; --digest emits one digest per room/source group; text with --text",
            "a multi-address watch merges direct mail and deduplicates channel messages in one stream; a long-running watch requires a bound participant and suppresses only local-origin from_participant == self (remote-origin evidence is never own), while snapshot is the read-only unbound exception; reads envelopes plus bounded sanitized previews — never moves or alters mail, never emits a complete body, and never mutates channel seen-sets; scans are the truth source and a native filesystem watcher (inotify/FSEvents) only supplies wake hints, with a slow periodic re-registration pass and poll fallback at --interval-ms; each long-running poll renews the participant lease and participants/<id>/watch.heartbeat only after write-fence admission, while snapshot is wholly read-only and never does either; events carry reason mail|channel|mention on every type (unreadable: mail|channel); --digest groups each batch by typed address, source (mail or channel:<name>), and pending status in first-arrival order; --snapshot scans exactly once (unread direct mail plus effective-channel messages outside the seen-set) and exits 0 (empty scan emits nothing; direct-mail scan failure is a nonzero error, never a false empty; an unregistered room warns on stderr, scans nothing, and creates no directories); snapshot-only --limit <n> admits the last n underlying events before optional digest grouping and warns when earlier events are omitted, while --limit 0 is unlimited; repeatable --reason mail|channel|mention delivers only events with a selected reason (omitted = all), applied after the scan and before --limit, --digest grouping, and the --once exit check; an unreadable channel message is always reason=channel, never mention, because its body cannot be read",
        ),
        command(
            "who",
            "post who [--room <name>]... [--text]",
            "JSON; text with --text",
            "read-only participant directory: acting participant/provenance first, then all participants with lineage/workspace/watch presence, then legacy room heartbeat rows under legacy_rooms; --text labels each participant's lease state lease=active|stale|ended|no lease record (JSON keeps the state key) and adds one hint line: a lease is not attention, use post chat <channel> --seen-by <message-id>; live_watch is true for a fresh `post watch` heartbeat or, when doorbell/health.json was rewritten within the last 90 s, an armed doorbell-supervisor subscription for that participant or room (doorbell_armed says which; a stale or unreadable supervisor file counts for nothing, and the doorbell field and a --text line say so); bridge_attention counts the items in bridge/health.json's attention list (`post doctor` lists them with fixes), and bridge_health says so when a bridged host's health file cannot be read; provenance never claims to detect subagency and no PID is reported",
        ),
        command(
            "version",
            "post version [--json] | post --version",
            "one text line; JSON with --json",
            "read-only build/store/capability receipt; post --version prints the same line (a `-dirty` build id marks a build from a tree with uncommitted changes)",
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
            "read-only; requires a bound participant and shows only letters that participant sent (anything else, and an id absent from archive/, is not_found, exit 66). Lineage and local participant mail is state=unsupported, and so is a workspace letter to a room on this host (reason names the room). A workspace letter to a room homed on another host answers from bridge evidence on this host, whatever the room's registration is now: bridge/room-acked/<id>.json ({v:1, id, host, room, status, reason, sha256, at}, exactly these keys, written by the sending bridge before it retires the outbox entry; id and sha256 match the archive letter, room = the letter's to, status delivered with reason null or rejected with the receiver's terminal reason) gives received|rejected; else bridge/published/<id> (plain text: the relay commit, 40 or 64 hex characters) gives published with commit; else, when the room is currently registered as a placeholder under remote/<host>/ and the letter is still in the room's inbox (where the bridge collects it), queued; a letter with no record that is not waiting there is unsupported. Both files are never pruned. An existing file that does not validate gives unknown as below. A participant:<id>@<host> letter is validated against its archive bytes and the bridge's evidence on this host: bridge/pmail-acked/<id>.json (exact keys v, status, origin, host, participant, id, sha256, reason, at; origin = this host, host = to_host, participant and id match, sha256 = archive digest, status delivered with reason null or rejected with a terminal reason: malformed, to_mismatch, forged_from, unknown_participant, ended_participant, blocked_route, id_collision, or, decided by the destination bridge before deliver runs, name_collision or unpublished_sender) gives received|rejected; else bridge/pmail-published/<id>.json ({v:1, id, host, sha256, commit, at}, written after the push) gives published with commit and age_s; else queued, with blocked_reason/last_error from bridge/pmail-status/<id>.json ({v:1, id, blocked_reason?, last_error?, at?}). A receipt outranks the marker. Any evidence file that exists but does not validate gives state=unknown with evidence_file and evidence_error, never a guess; so does a marker whose at is more than 5 s in the future, and a pmail-conflicts file with no pmail-acked receipt. bridge/pmail-conflicts/<id>.json present sets conflict=true (the first receipt stands)",
        ),
    ];
    let output_shapes = OutputShapes {
        participant: fields(&[
            "show/bind/touch/end: ok, status=bound|unbound|missing|archived|ended (show answers bound; unbound when no record answers to the session and no claim was made; missing when a claim names a record that does not exist, together with participant_missing, still exit 0; archived when `participant gc` moved the record aside, which `post participant bind` restores; ended after `end`), bound? (show only: false unless a record answers), id?, participant? (the record: version, id, harness, conversation_key_digest, created, last_seen?, lease_hours (24 by default, 1 on a `bind --new` record), workspace?, workspace_path?, lineage?, lineage_since?, ended_at?, ephemeral? (true only on `bind --new` records; absent otherwise)), provenance? (explicit-bootstrap for --new/--key), fix?, participant_error?",
            PARTICIPANT_MISSING,
            "list: ok, participants, count",
            "restore (post participant restore <id>): ok, id, restored (false when the record was already present and nothing was written), from? (archive: the whole record came back, state included; tombstone: recreated with the same id, lease, workspace, and display name; absent when restored is false), participant (the record); an id nothing ever held, or a collected record that cannot be brought back, is the participant_missing error, exit 65, and creates nothing",
            "gc (post participant gc, a dry run unless --apply): ok, applied (false on the dry run), deleted[] (ids removed, each leaving a tombstone line in participants/archived.jsonl), archived[] (ids moved to participants-archive/), kept{reason: count; reasons: active, no_last_seen, recent, live_watch, lineage, subscribed, outbound_in_flight, named_by_receipt, pending_mail, unread_mail, unreadable_state, and changed (--apply only: the record changed between the plan and the apply)}; the dry run lists what --apply would do",
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
            "build_sha (short commit id; a -dirty suffix marks a build from a tree with uncommitted changes to tracked files; unknown outside a git checkout)",
            "store_version=2",
            "capabilities",
        ]),
        doctor: fields(&[
            "ok",
            "status=healthy|degraded|broken (healthy: no findings; degraded: warnings only; broken: an error; info lines never count)",
            "root",
            "checks[] (id, severity=info|warning|error, path, message, fixable, suggested_fix)",
            "count (findings: checks that are not info)",
            "fixed",
            "exit_codes[] (code, meaning)",
            "severity_filter?(warn|error; present only under --severity, which trims the listed checks but not status, count, or ok)",
            "filtered_out? (integer; present only under --severity: findings the filter hid, so count = findings listed in checks + filtered_out)",
            "participant ({status=bound|unbound|missing, id?, provenance?, workspace?, lineage?, fix? (unbound and missing)})",
            "pending{address:count}",
            "participant_fix (when no participant is bound)",
            "participant_error (when ambient participant resolution failed)",
            "bound? (false, present only together with participant_missing; the participant's status is then missing, and neither status, count, nor the exit code changes)",
            PARTICIPANT_MISSING,
        ]),
        inbox: fields(&[
            "ok",
            "room",
            "participant",
            UNBOUND,
            "unread[] (id, from, origin, reply_to_participant?, reply_to_shared, kind, subject, sent, display_name?, pfp?, sender_address?, sender_provenance?, from_participant?, from_lineage?)",
            "count",
            "skipped_unreadable",
            "unread_count",
            "pending",
            "pending_by_address{address:count}",
            "held",
            "hint? (with bound=false)",
            BOUND_NOW,
        ]),
        read_json: fields(&[
            "ok",
            FRAMING,
            BOUND_NOW,
            UNBOUND,
            "envelope (id, from, to, kind, subject, sent, from_participant?, from_lineage?, address_kind?, display_name?, pfp?, sender_address?, sender_provenance?, origin, reply_to_participant?, reply_to_shared, pending?, address{kind,name}?)",
            "body",
            "own (when true)",
            "pending (when true)",
            "already_read (present and true only when the participant cursor contains the exact id)",
        ]),
        read_budget: fields(&[
            "ok",
            FRAMING,
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
            FRAMING,
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
            "rooms[] (name, path, blocked[] (the rules.json rules whose target is this room: from, to, reason))",
            "count",
            "warnings? (add only: strings, present when the name was accepted without fresh evidence that no peer host publishes it)",
            "set-path: ok, room, before, after, changed, dry_run",
            "rename: ok, old, new, path, mailbox_moved, rewritten, warnings, resumed, dry_run",
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
            SEND_ENVELOPE,
            "archived",
            "delivery? ({state=queued, host}; participant:<id>@<host> sends only; never claims remote delivery)",
            "cross_host? ({status=queued, host}; a workspace send whose room is a bridge placeholder for a room on another host: the letter is queued for the bridge and `post delivery <id>` tracks it; absent for a room on this host; text mode adds one stdout line naming the host and that command)",
            "retargeted? ({from, to, note}; present only when --allow-self sent to the sender's own participant inbox instead of the room or lineage named: from is what --to resolved to, to is where it went, note says why)",
            "warnings? (strings; present only when non-empty: the send landed and something deserves a second look, such as a routing receipt that could not be written or a body that looks like pasted watch output)",
            BOUND_NOW,
        ]),
        chat_join: fields(&[
            "ok",
            "channel",
            "room",
            "created",
            "already_member",
            "backlog_ignored",
            "event_id",
            "history_before_join",
            "history_hint",
            "normalized_from? (present only when the stored channel name is a normalized form of the name typed: lowercase, spaces and underscores as hyphens)",
            BOUND_NOW,
        ]),
        chat_send: fields(&[
            "ok",
            "message",
            "cross_host{status=queued|local_only|unconfirmed,reason?} (queued eligible; local_only lasting; unconfirmed health unavailable or bridge predates roomless relay, do not resend)",
            "crossed? ({unseen, addressed_to_you, messages[] (id, from, display_name?, sent, addressed_to_you, body, signed_verified?, sender_address?, sender_provenance?; at most 10, newest last; the whole body for a message addressed to the sender, a 300-character preview otherwise; signed_verified only on signed-looking owner-room messages, the other two only when the sender declared them)}; present only when unseen messages from others crossed the send, which always delivers; the crossed messages stay unread)",
            "skipped? ([{id, reason}]: unparseable channel message files the crossing check left out)",
            "warnings? (strings; present only when non-empty: the send landed and something deserves a second look, such as a body that looks like pasted watch output)",
            BOUND_NOW,
        ]),
        chat_read: fields(&[
            "ok",
            FRAMING,
            "channel",
            "room",
            "peek",
            "messages[] (id, from, channel, subject, sent, body, origin, reply_to_shared, reply_to_participant?, from_participant?, from_host?, from_lineage?, address_kind?, event? (join, profile, or another system event kind; absent on ordinary messages), display_name?, pfp?, re?, mentions?, signature_ref?, sender_address?, sender_provenance?, signed_verified?)",
            "count",
            "skipped (un-emitted remainder; omitted when 0)",
            "skipped_files? ([{id, reason}]: unparseable message files the read left out, reported once; a bad file never fails the read)",
            "has_more",
            "selected_count (with --max-bytes)",
            "byte_limit (with --max-bytes)",
            "omitted? (reason, count, source, channel, first_id, first_body_bytes, mention_count, continuation)",
            UNBOUND,
            BOUND_NOW,
        ]),
        chat_slice: fields(&[
            "ok",
            FRAMING,
            "channel",
            "room",
            "message (complete stored envelope only: id, from, subject, sent)",
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
            "targets[].messages[] (mail: envelope {id, from, to, kind, subject, sent, origin, reply_to_participant?, reply_to_shared, address{kind,name}?, and the optional stamped sender fields listed under read_json}, body; channel: id, from, channel, subject, sent, body)",
            "count",
            "selected_count (with --max-bytes)",
            "has_more (with --max-bytes)",
            "byte_limit (with --max-bytes)",
            "omitted? (reason, count, source, channel?, first_id, first_body_bytes, mention_count, remaining_targets, continuation)",
            "skipped? ([{id, reason, channel?}]: unparseable channel message files the catch-up left out, reported once)",
            BOUND_NOW,
        ]),
        search: fields(&[
            "ok",
            "participant",
            "pending",
            "framing (source, authority, laws)",
            "room",
            "pattern",
            "match",
            "results[] (source, channel?, id, from, from_participant?, from_host? for channel, from_lineage?, origin, reply_to_participant?, reply_to_shared, sent, subject, preview, matched, own?, pending?, already_read?, kind? for mail)",
            "count",
            "limit",
            "truncated",
            "skipped? ([{id, reason, channel?}]: unparseable channel message files the search left out, reported once)",
            UNBOUND,
        ]),
        channels: fields(&[
            "ok",
            "channels (name, created, created_by, description?, members=workspace addresses, participants=host-local ids, messages, room, unread, archived, archived_at?, archived_by?)",
            "count",
            "archived_hidden",
            "participant",
            "pending",
            "skipped? ([{id, reason, channel?}]: unparseable channel message files the listing left out, reported once; also a channel member left out of the participants lists for an invalid membership file, as {id: participant id, reason})",
            UNBOUND,
        ]),
        profile: fields(&[
            "ok",
            "room",
            "profile (name?, pfp?)",
            "announced (set/clear; channels that received the change event)",
            "list: ok, profiles[] (key, participant?, workspace?, name?, pfp?, lease? (active|stale|ended|no lease record|no participant record; absent for legacy), legacy?, holds_sigil)",
        ]),
        watch: fields(&[
            "mail: event, address{kind,name}, room? (workspace only), id, from, from_participant?, from_lineage?, origin, reply_to_participant?, reply_to_shared, pending?, kind, subject, sent, reason=mail, preview?, display_name?, pfp?, sender_provenance?, sender_address? (all four only when the sender declared them), cursor_unusable? (true when the acting participant's cursor state could not be read, so everything reads as unseen)",
            "unreadable: event, address{kind,name}, room? (workspace only), id, reason=mail|channel, channel? (required for channel; no preview)",
            "channel_message: event, address{kind,name}, room? (workspace only), channel, id, from, from_participant?, from_host?, from_lineage?, origin, reply_to_participant?, reply_to_shared, subject, sent, reason=channel|mention, preview?, display_name?, pfp?, sender_provenance?, sender_address? (all four only when the sender declared them), cursor_unusable? (as on mail)",
            "unbound: event=unbound, participant=null, bound=false, hint (the single line `watch --snapshot` prints when the session has no participant and no --room is named, so a hook reading the stream gets an answer, never a cwd-derived room; text mode prints the hint as prose)",
            "digest: event=digest, address{kind,name}, room? (workspace only), source=mail|channel:<name>, pending?, count, first_id, last_id, from, reason=mail|channel|mention|mixed, preview? (text preview precedes bounds/since suffix), cursor_unusable? (as on mail)",
        ]),
        delivery: fields(&[
            "ok",
            "schema=post.delivery.v1",
            "id",
            "state=queued|published|received|rejected|unknown|unsupported",
            "participant? (the letter's to; participant letters)",
            "room? (the letter's to; workspace letters)",
            "host? (the letter's to_host; for a workspace letter, the room's host from the bridge record or the current placeholder)",
            "sha256? (archive digest)",
            "conflict",
            "reason? (rejected: the terminal reason; unsupported: why)",
            "blocked_reason? (queued)",
            "last_error? (queued)",
            "commit? (published)",
            "published_at? (published; participant letters)",
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
            "participant (status=bound|unbound|missing, state?, last_seen?, id?, harness?, provenance?, workspace?, lineage?, unread{address:count}, pending{address:count}, fix?)",
            "participants (id, harness, state, last_seen?, lineage?, workspace?, unread{address:count}, pending{address:count}, live_watch (a fresh `post watch` heartbeat or an armed doorbell subscription), watch_last_seen?, doorbell_armed? (present and true only when the doorbell supervisor has this participant armed))",
            "legacy_rooms (room, live_watch, last_seen?, doorbell_armed? (present and true only when the doorbell supervisor has this room armed))",
            "activity_note? (stale-delivery crash gap: frozen mail is not reassigned)",
            "count",
            "skipped? ([{id, reason}]: participant records too damaged to read, so missing from participants; present only when there is one, and `post participant list` carries the same key; --text prints one `skipped:` line)",
            "bridge_attention? (integer: how many items bridge/health.json lists under attention; present only when nonzero, and `post doctor` lists each with its fix)",
            "bridge_health? ({reason, fix}; present only on a bridged host whose bridge/health.json is missing, malformed, or has no attention list, when the absent bridge_attention means nothing; `post doctor` reports it as bridge.health_unreadable)",
            "doorbell? (fresh|stale|unreadable: what live_watch could see of doorbell/health.json; present only when that file exists; stale and unreadable count for nothing)",
            "bound? (false, present only together with participant_missing; the participant's status is then missing, with the repair in fix)",
            PARTICIPANT_MISSING,
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
            "--json: print JSON instead of text. Commands that print text by default (send, read, chat, catchup, search, version, delivery, profile) print one JSON object; the rest already print JSON, and the ones with a text flag (inbox, channels, who, watch, doctor --brief) print their human form only when it is given; watch always streams NDJSON",
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
            "A send that landed never exits nonzero: when its receipt cannot be written to stdout, the mail is on disk exactly once, the exit code is 0, and one line on stderr says the change was committed; resending would duplicate it. delivered_output_failure remains the non-retryable answer after other committed channel mutations, and committed room registration stdout failure is reported as success with best-effort diagnostics. A read-only command whose reader closed the pipe (`post who --text | head`) stops quietly with its own exit code, not an io_error.",
            "--json output is one JSON document on stdout with nothing on stderr, so `2>&1 | jq` parses it; anything degraded or worth a second look rides in the document (send: warnings). Commands run with --text or --brief report their failures as prose on stderr instead of a JSON object; every other failure is the error_shape envelope on stderr.",
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
            "A message body comes from exactly one of --body, --body-file, or stdin (a bare argument to `post send` is the body, the same as --body; there is no positional file, and a path-shaped bare argument is refused with the --body-file fix); a body-file path that does not exist is a usage error, never a retryable I/O fault.",
            "Shell quoting happens before Post: double quotes can expand dollar-positionals such as $1 in $1.63B, and an apostrophe can terminate single quotes; use --body-file or stdin for shell-sensitive prose.",
            "Subjects over 1 KiB fail before any write with no override; longer text belongs in the body.",
            "Message bodies over 32 KiB fail before any write unless --oversize records explicit intent; a body of complete Post watch-event NDJSON lines still sends, with a warning in the receipt (warnings under --json).",
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
            "Channel sends always deliver; when unseen ordinary messages from others exist in the channel the receipt carries a `crossed` block describing them and they stay unread. Direct mail is unaffected.",
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
            "POST_NOTICE_MANAGED: adapter-only switch suppressing the direct bind notice (without it, a bind answering in JSON carries the activation notice once per participant as a top-level notice field and keeps stderr empty; the text bootstrap prints it once on stderr); adapters query participant notice and acknowledge with --ack only after successful context delivery.",
            "POST_AUTO_GC: =0 disables the automatic participant cleanup; otherwise post participant bind runs the same pass as post participant gc --apply at most once per 24 hours per store, silently (a new store gets a stamp first and its first run a day later). Each run appends a JSON line to .auto-gc.log at the store root naming what it removed; post participant restore <id> brings any of it back",
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
