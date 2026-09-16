#!/bin/sh
# Live smoke for an installed post binary against a throwaway mail root.
# Usage: scripts/smoke-installed.sh /path/to/post
# Red proof: /tmp/post-before-participants.64fZu4/post (verified Post 0.9.0).
# Covers: fresh-root doctor bootstrap, watch backlog ring, --from now,
# --from/--snapshot conflict, digest --since fencepost round-trip, and the
# installed participant/lineage acceptance protocol from docs/PARTICIPANTS.md.
# shellcheck disable=SC2016,SC2030,SC2031,SC2329
set -eu
[ "$#" -eq 1 ] || {
    printf 'usage: %s /path/to/post\n' "$0" >&2
    exit 2
}
BIN="$1"
case "$BIN" in
    /*) ;;
    *) BIN="$(pwd)/$BIN" ;;
esac

setup_prerequisites() {
    if [ ! -x "$BIN" ]; then
        printf 'FAIL SETUP: post binary is not executable: %s\n' "$BIN"
        return 1
    fi
    for setup_tool in jq python3 mktemp pgrep; do
        if ! command -v "$setup_tool" >/dev/null 2>&1; then
            printf 'FAIL SETUP: required command is unavailable: %s\n' "$setup_tool"
            return 1
        fi
    done
}

setup_prerequisites || exit 1

# Portable bounded process wait. Poll first so the final wait only reaps an
# already-exited child. A timed-out child is terminated, then killed if needed.
wait_for_pid() {
    wait_pid=$1
    wait_polls=${2:-100}
    wait_attempt=0
    while kill -0 "$wait_pid" 2>/dev/null && [ "$wait_attempt" -lt "$wait_polls" ]; do
        sleep 0.1
        wait_attempt=$((wait_attempt + 1))
    done
    if ! kill -0 "$wait_pid" 2>/dev/null; then
        wait "$wait_pid"
        return $?
    fi

    kill "$wait_pid" 2>/dev/null || true
    wait_attempt=0
    while kill -0 "$wait_pid" 2>/dev/null && [ "$wait_attempt" -lt 20 ]; do
        sleep 0.1
        wait_attempt=$((wait_attempt + 1))
    done
    if kill -0 "$wait_pid" 2>/dev/null; then
        kill -9 "$wait_pid" 2>/dev/null || true
        wait_attempt=0
        while kill -0 "$wait_pid" 2>/dev/null && [ "$wait_attempt" -lt 20 ]; do
            sleep 0.1
            wait_attempt=$((wait_attempt + 1))
        done
    fi
    if ! kill -0 "$wait_pid" 2>/dev/null; then
        wait "$wait_pid" 2>/dev/null || true
    fi
    return 124
}

stop_pid() {
    [ -n "$1" ] || return 0
    for stop_pid_child in $(pgrep -P "$1" 2>/dev/null); do
        stop_pid "$stop_pid_child"
    done
    kill "$1" 2>/dev/null || true
    wait_for_pid "$1" 20 >/dev/null 2>&1 || true
}

remove_temp_tree() {
    temp_tree=$1
    created_tree=$2
    if [ -z "$temp_tree" ] || [ "$temp_tree" = / ] || [ "$temp_tree" != "$created_tree" ]; then
        printf 'post smoke: refusing to remove unregistered temp path %s\n' "$temp_tree" >&2
        return 1
    fi
    [ ! -d "$temp_tree" ] || rm -rf -- "$temp_tree"
}

# Execute the binary directly in a bounded child. `exec` makes the recorded PID
# the process that can block; wait_for_pid therefore cannot strand a grandchild.
run_bounded_exec() {
    bounded_cwd=$1
    bounded_stdout=$2
    bounded_stderr=$3
    bounded_polls=$4
    shift 4
    (
        cd "$bounded_cwd" || exit 125
        exec "$@"
    ) >"$bounded_stdout" 2>"$bounded_stderr" &
    RUN_BOUNDED_PID=$!
    wait_for_pid "$RUN_BOUNDED_PID" "$bounded_polls"
    bounded_rc=$?
    RUN_BOUNDED_PID=
    return "$bounded_rc"
}

# A read-only proof must notice newly-created empty directories as well as byte
# changes. Python supplies the already-required hashing implementation, and any
# stat/read/hash error makes the manifest command fail instead of comparing two
# empty pipelines.
store_manifest() {
    python3 - "$1" <<'PY'
import hashlib
import json
import os
import stat
import sys

root = os.path.abspath(sys.argv[1])

def kind(mode):
    if stat.S_ISDIR(mode):
        return "directory"
    if stat.S_ISREG(mode):
        return "file"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISFIFO(mode):
        return "fifo"
    if stat.S_ISSOCK(mode):
        return "socket"
    if stat.S_ISCHR(mode):
        return "character-device"
    if stat.S_ISBLK(mode):
        return "block-device"
    return "other"

def visit(path, relative):
    metadata = os.lstat(path)
    entry = {
        "path": relative,
        "type": kind(metadata.st_mode),
        "mode": format(stat.S_IMODE(metadata.st_mode), "04o"),
        "mtime_ns": metadata.st_mtime_ns,
    }
    if stat.S_ISREG(metadata.st_mode):
        digest = hashlib.sha256()
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        entry["sha256"] = digest.hexdigest()
    elif stat.S_ISLNK(metadata.st_mode):
        entry["target"] = os.readlink(path)
    print(json.dumps(entry, sort_keys=True, separators=(",", ":")))
    if stat.S_ISDIR(metadata.st_mode):
        with os.scandir(path) as children:
            names = sorted(child.name for child in children)
        for name in names:
            child_relative = name if relative == "." else f"{relative}/{name}"
            visit(os.path.join(path, name), child_relative)

visit(root, ".")
PY
}

# This scenario deliberately accumulates every named result. A pre-participant
# binary should therefore leave a useful red-proof transcript instead of dying
# at the first unknown subcommand.
participants_smoke() (
    set +e
    PS_BIN=$1
    PS_BASE=$(mktemp -d 2>&1)
    PS_MKTEMP_RC=$?
    if [ "$PS_MKTEMP_RC" -ne 0 ] || [ -z "$PS_BASE" ] || [ ! -d "$PS_BASE" ]; then
        printf 'FAIL SETUP-participants: mktemp failed: %s\n' "$PS_BASE"
        exit 1
    fi
    PS_CREATED_ROOT=$PS_BASE
    if [ ! -w "$PS_BASE" ]; then
        printf 'FAIL SETUP-participants: temp root is not writable: %s\n' "$PS_BASE"
        remove_temp_tree "$PS_BASE" "$PS_CREATED_ROOT" || true
        exit 1
    fi
    PS_ROOT="$PS_BASE/mail"
    PS_WORK="$PS_BASE/workspace"
    PS_A_KEY=aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa
    PS_B_KEY=bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb
    PS_C_KEY=cccccccc-cccc-4ccc-8ccc-cccccccccccc
    PS_FAILURES=0
    ROW_REASON=
    PS_FIFO_READER_PID=
    PS_FIFO_WRITER_PID=
    PS_WATCH_PID=
    RUN_BOUNDED_PID=
    A_ID=claude-missing0000
    B_ID=codex-missing0000
    C_ID=smoke-missing0000
    export POST_MAIL_ROOT="$PS_ROOT"
    unset POST_FROM POST_SENDER_ADDRESS POST_FRAMING POST_ARX_GENERATION \
        POST_PARTICIPANT POST_HARNESS POST_PARTICIPANT_LEASE_HOURS \
        CLAUDE_CODE_SESSION_ID CLAUDE_PID CODEX_THREAD_ID \
        CODEX_SESSION_ID 2>/dev/null || true

    cleanup_participants() {
        stop_pid "$PS_FIFO_READER_PID"
        stop_pid "$PS_FIFO_WRITER_PID"
        stop_pid "$PS_WATCH_PID"
        stop_pid "$RUN_BOUNDED_PID"
        if remove_temp_tree "$PS_BASE" "$PS_CREATED_ROOT"; then
            printf 'participants smoke root removed: %s\n' "$PS_BASE"
        else
            printf 'participants smoke root retained after cleanup failure: %s\n' "$PS_BASE" >&2
        fi
    }
    stop_participants_on_signal() {
        signal_rc=$1
        trap - EXIT INT TERM
        cleanup_participants
        exit "$signal_rc"
    }
    trap cleanup_participants EXIT
    trap 'stop_participants_on_signal 130' INT
    trap 'stop_participants_on_signal 143' TERM

    mkdir -p "$PS_WORK"
    "$PS_BIN" doctor --fix >/dev/null 2>"$PS_BASE/doctor.err"
    "$PS_BIN" rooms add smoke "$PS_WORK" >/dev/null 2>"$PS_BASE/rooms.err"

    clear_identity() {
        unset POST_PARTICIPANT POST_HARNESS POST_SENDER_ADDRESS \
            POST_ARX_GENERATION POST_PARTICIPANT_LEASE_HOURS \
            CLAUDE_CODE_SESSION_ID CLAUDE_PID CODEX_THREAD_ID \
            CODEX_SESSION_ID 2>/dev/null || true
    }
    as_a() (
        clear_identity
        export CLAUDE_CODE_SESSION_ID="$PS_A_KEY"
        cd "$PS_WORK" || exit 1
        "$PS_BIN" "$@"
    )
    as_b() (
        clear_identity
        export CODEX_THREAD_ID="$PS_B_KEY"
        export CODEX_SESSION_ID="$PS_B_KEY"
        cd "$PS_WORK" || exit 1
        "$PS_BIN" "$@"
    )
    as_c() (
        clear_identity
        export POST_PARTICIPANT="$C_ID"
        cd "$PS_WORK" || exit 1
        "$PS_BIN" "$@"
    )
    as_participant() (
        participant=$1
        shift
        clear_identity
        export POST_PARTICIPANT="$participant"
        cd "$PS_WORK" || exit 1
        "$PS_BIN" "$@"
    )
    as_participant_with_lease() (
        participant=$1
        lease_hours=$2
        shift 2
        clear_identity
        export POST_PARTICIPANT="$participant"
        export POST_PARTICIPANT_LEASE_HOURS="$lease_hours"
        cd "$PS_WORK" || exit 1
        "$PS_BIN" "$@"
    )
    unbound() (
        clear_identity
        cd "$PS_WORK" || exit 1
        "$PS_BIN" "$@"
    )
    bind_explicit() (
        key=$1
        clear_identity
        cd "$PS_WORK" || exit 1
        "$PS_BIN" participant bind --harness smoke --key "$key" \
            --workspace smoke --json
    )
    capture_json() {
        output=$1
        shift
        "$@" >"$output" 2>"$output.err"
        capture_rc=$?
        if [ "$capture_rc" -ne 0 ]; then
            capture_stderr=$(tr '\n' ' ' <"$output.err")
            capture_stdout=$(tr '\n' ' ' <"$output")
            ROW_REASON="command failed ($capture_rc): stderr=[$capture_stderr] stdout=[$capture_stdout]"
            return 1
        fi
        if ! jq -e . "$output" >/dev/null 2>&1; then
            ROW_REASON="stdout was not JSON: $(tr '\n' ' ' <"$output")"
            return 1
        fi
        return 0
    }
    assert_jq() {
        assertion_file=$1
        assertion_filter=$2
        shift 2
        if ! jq -e "$@" "$assertion_filter" "$assertion_file" >/dev/null 2>&1; then
            assertion_value=$(jq -c . "$assertion_file" 2>/dev/null || printf '<invalid-json>')
            ROW_REASON="JSON assertion failed: $assertion_filter; actual=$assertion_value"
            return 1
        fi
        return 0
    }
    record_row() {
        row_name=$1
        row_description=$2
        row_function=$3
        ROW_REASON=
        if "$row_function"; then
            printf 'ok %s: %s\n' "$row_name" "$row_description"
        else
            [ -n "$ROW_REASON" ] || ROW_REASON="row returned failure without a diagnostic (smoke bug)"
            printf 'FAIL %s: %s\n' "$row_name" "$ROW_REASON"
            PS_FAILURES=$((PS_FAILURES + 1))
        fi
    }
    mail_id_from() {
        jq -r '.envelope.id // empty' "$1"
    }
    write_workspace_receipt() {
        receipt_id=$1
        receipt_recipient=$2
        receipt_mail="$PS_ROOT/smoke/inbox/$receipt_id.mail"
        receipt_dir="$PS_ROOT/smoke/routing"
        receipt_digest=$(python3 - "$receipt_mail" <<'PY'
import hashlib
import pathlib
import sys

print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
        ) || {
            ROW_REASON="could not hash routed fixture $receipt_id"
            return 1
        }
        mkdir -p "$receipt_dir" || {
            ROW_REASON="could not create routing fixture directory"
            return 1
        }
        jq -n --arg id "$receipt_id" --arg digest "$receipt_digest" \
            --arg recipient "$receipt_recipient" \
            '{version:1,message:$id,digest:$digest,address:{kind:"workspace",name:"smoke"},recipients:[$recipient],excluded:[],routed_at:"2026-09-16T08:00:00Z",routed_by:$recipient}' \
            >"$receipt_dir/$receipt_id.json" || {
            ROW_REASON="could not write routing fixture $receipt_id"
            return 1
        }
    }
    manifest() {
        manifest_root=$1
        manifest_output=$2
        if ! store_manifest "$manifest_root" >"$manifest_output"; then
            ROW_REASON="could not capture read-only store manifest"
            return 1
        fi
    }

    # Bind once up front so later rows can still run and report if one binding
    # is absent. A and B continue to resolve through their native harness keys;
    # C is deliberately exercised through POST_PARTICIPANT after this bind.
    as_a participant bind --workspace smoke --json >"$PS_BASE/bind-a.json" \
        2>"$PS_BASE/bind-a.err"
    BIND_A_RC=$?
    as_b participant bind --workspace smoke --json >"$PS_BASE/bind-b.json" \
        2>"$PS_BASE/bind-b.err"
    BIND_B_RC=$?
    bind_explicit "$PS_C_KEY" >"$PS_BASE/bind-c.json" 2>"$PS_BASE/bind-c.err"
    BIND_C_RC=$?
    parsed=$(jq -r '.participant.id // empty' "$PS_BASE/bind-a.json" 2>/dev/null)
    [ -n "$parsed" ] && A_ID=$parsed
    parsed=$(jq -r '.participant.id // empty' "$PS_BASE/bind-b.json" 2>/dev/null)
    [ -n "$parsed" ] && B_ID=$parsed
    parsed=$(jq -r '.participant.id // empty' "$PS_BASE/bind-c.json" 2>/dev/null)
    [ -n "$parsed" ] && C_ID=$parsed

    row_01() {
        if [ "$BIND_A_RC" -ne 0 ]; then
            ROW_REASON="A participant bind unavailable: $(tr '\n' ' ' <"$PS_BASE/bind-a.err")"
            return 1
        elif [ "$BIND_B_RC" -ne 0 ]; then
            ROW_REASON="B participant bind unavailable: $(tr '\n' ' ' <"$PS_BASE/bind-b.err")"
            return 1
        elif [ "$BIND_C_RC" -ne 0 ]; then
            ROW_REASON="C participant bind unavailable: $(tr '\n' ' ' <"$PS_BASE/bind-c.err")"
            return 1
        fi
        [ "$A_ID" != "$B_ID" ] && [ "$A_ID" != "$C_ID" ] && [ "$B_ID" != "$C_ID" ] || {
            ROW_REASON="bindings were not distinct"
            return 1
        }
        capture_json "$PS_BASE/row01-who.json" as_a who --json || return 1
        assert_jq "$PS_BASE/row01-who.json" \
            '([.participants[].id] | index($a)) != null and ([.participants[].id] | index($b)) != null and ([.participants[].id] | index($c)) != null' \
            --arg a "$A_ID" --arg b "$B_ID" --arg c "$C_ID"
    }

    row_02() {
        capture_json "$PS_BASE/row02-send.json" as_a send --to workspace:smoke \
            --subject row02 --body "A to the shared workspace" --json || return 1
        id=$(mail_id_from "$PS_BASE/row02-send.json")
        [ -n "$id" ] || { ROW_REASON="send returned no envelope id"; return 1; }
        capture_json "$PS_BASE/row02-b-before.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row02-b-before.json" \
            '([.unread[]?.id] | index($id)) != null and .unread_count == 1 and .count == 1 and .pending == 0' \
            --arg id "$id" || return 1
        capture_json "$PS_BASE/row02-a-before.json" as_a inbox --json || return 1
        assert_jq "$PS_BASE/row02-a-before.json" \
            '([.unread[]?.id] | index($id)) == null and .unread_count == 0 and .count == 0 and .pending == 0' \
            --arg id "$id" || return 1
        capture_json "$PS_BASE/row02-b-read.json" as_b read "$id" --json || return 1
        capture_json "$PS_BASE/row02-a-read.json" as_a read "$id" --json || return 1
        assert_jq "$PS_BASE/row02-a-read.json" \
            '.envelope.id == $id and .own == true' --arg id "$id" || return 1
        [ -f "$PS_ROOT/smoke/inbox/$id.mail" ] || { ROW_REASON="canonical inbox file moved"; return 1; }
        b_cursor="$PS_ROOT/participants/$B_ID/cursors.json"
        [ -f "$b_cursor" ] || { ROW_REASON="B cursor missing after consumption"; return 1; }
        assert_jq "$b_cursor" \
            '([.mail["workspace:smoke"].seen[]?] | index($id)) != null' \
            --arg id "$id" || return 1
        a_cursor="$PS_ROOT/participants/$A_ID/cursors.json"
        if [ -e "$a_cursor" ]; then
            assert_jq "$a_cursor" \
                '([.mail // {} | .[] | .seen[]?] | index($id)) == null' \
                --arg id "$id" || return 1
        fi
        capture_json "$PS_BASE/row02-b-after.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row02-b-after.json" \
            '([.unread[]?.id] | index($id)) == null and .unread_count == 0 and .count == 0' \
            --arg id "$id"
    }

    row_03() {
        capture_json "$PS_BASE/row03-send.json" as_c send --to workspace:smoke \
            --subject row03 --body "third-party fan-out" --json || return 1
        id=$(mail_id_from "$PS_BASE/row03-send.json")
        capture_json "$PS_BASE/row03-a-before.json" as_a inbox --json || return 1
        capture_json "$PS_BASE/row03-b-before.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row03-a-before.json" \
            '([.unread[]?.id] | index($id)) != null and .unread_count == 1 and .pending == 0' \
            --arg id "$id" || return 1
        assert_jq "$PS_BASE/row03-b-before.json" \
            '([.unread[]?.id] | index($id)) != null and .unread_count == 1 and .pending == 0' \
            --arg id "$id" || return 1
        bind_explicit row03-late >"$PS_BASE/row03-late-bind.json" 2>"$PS_BASE/row03-late-bind.err" || { ROW_REASON="late participant bind failed"; return 1; }
        late=$(jq -r '.participant.id // empty' "$PS_BASE/row03-late-bind.json")
        capture_json "$PS_BASE/row03-late-inbox.json" as_participant "$late" inbox --json || return 1
        assert_jq "$PS_BASE/row03-late-inbox.json" \
            '([.unread[]?.id] | index($id)) == null' --arg id "$id" || return 1
        capture_json "$PS_BASE/row03-a-read.json" as_a read "$id" --json || return 1
        capture_json "$PS_BASE/row03-a-after.json" as_a inbox --json || return 1
        assert_jq "$PS_BASE/row03-a-after.json" \
            '([.unread[]?.id] | index($id)) == null and .unread_count == 0' \
            --arg id "$id" || return 1
        capture_json "$PS_BASE/row03-b-still.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row03-b-still.json" \
            '([.unread[]?.id] | index($id)) != null and .unread_count == 1' \
            --arg id "$id" || return 1
        capture_json "$PS_BASE/row03-b-read.json" as_b read "$id" --json || return 1
        assert_jq "$PS_BASE/row03-b-read.json" \
            '.envelope.id == $id' --arg id "$id" || return 1
        b_cursor="$PS_ROOT/participants/$B_ID/cursors.json"
        [ -f "$b_cursor" ] || { ROW_REASON="B cursor missing after row03 consumption"; return 1; }
        assert_jq "$b_cursor" \
            '([.mail["workspace:smoke"].seen[]?] | index($id)) != null' \
            --arg id "$id" || return 1
        capture_json "$PS_BASE/row03-b-after.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row03-b-after.json" \
            '([.unread[]?.id] | index($id)) == null and .unread_count == 0' \
            --arg id "$id" || return 1
        receipt="$PS_ROOT/smoke/routing/$id.json"
        [ -f "$receipt" ] || { ROW_REASON="routing receipt missing"; return 1; }
        assert_jq "$receipt" \
            '(.recipients | sort) == ([$a,$b] | sort) and ([.recipients[]] | index($c)) == null and ([.recipients[]] | index($late)) == null and .address == {kind:"workspace",name:"smoke"}' \
            --arg a "$A_ID" --arg b "$B_ID" --arg c "$C_ID" --arg late "$late"
    }

    row_04() {
        bind_explicit row04-a >"$PS_BASE/row04-a-bind.json" 2>/dev/null || { ROW_REASON="row04 A bind failed"; return 1; }
        bind_explicit row04-b >"$PS_BASE/row04-b-bind.json" 2>/dev/null || { ROW_REASON="row04 B bind failed"; return 1; }
        bind_explicit row04-c >"$PS_BASE/row04-c-bind.json" 2>/dev/null || { ROW_REASON="row04 C bind failed"; return 1; }
        a=$(jq -r '.participant.id' "$PS_BASE/row04-a-bind.json")
        b=$(jq -r '.participant.id' "$PS_BASE/row04-b-bind.json")
        c=$(jq -r '.participant.id' "$PS_BASE/row04-c-bind.json")
        capture_json "$PS_BASE/row04-new.json" as_participant "$a" identity new ember --json || return 1
        capture_json "$PS_BASE/row04-continue.json" as_participant "$b" identity continue ember --json || return 1
        capture_json "$PS_BASE/row04-c-send.json" as_participant "$c" send --to ember \
            --subject row04-c --body "lineage third-party" --json || return 1
        cid=$(mail_id_from "$PS_BASE/row04-c-send.json")
        assert_jq "$PS_BASE/row04-c-send.json" \
            '.envelope.from_participant == $c and (.envelope | has("from_lineage") | not) and .envelope.address_kind == "lineage"' \
            --arg c "$c" || return 1
        capture_json "$PS_BASE/row04-a-inbox.json" as_participant "$a" inbox --json || return 1
        capture_json "$PS_BASE/row04-b-inbox.json" as_participant "$b" inbox --json || return 1
        assert_jq "$PS_BASE/row04-a-inbox.json" '([.unread[]?.id] | index($id)) != null' --arg id "$cid" || return 1
        assert_jq "$PS_BASE/row04-b-inbox.json" '([.unread[]?.id] | index($id)) != null' --arg id "$cid" || return 1
        capture_json "$PS_BASE/row04-a-read.json" as_participant "$a" read "$cid" --json || return 1
        capture_json "$PS_BASE/row04-b-read.json" as_participant "$b" read "$cid" --json || return 1
        capture_json "$PS_BASE/row04-a-send.json" as_participant "$a" send --to lineage:ember \
            --subject row04-a --body "sibling lineage mail" --json || return 1
        aid=$(mail_id_from "$PS_BASE/row04-a-send.json")
        assert_jq "$PS_BASE/row04-a-send.json" \
            '.envelope.from_participant == $a and .envelope.from_lineage == "ember" and .envelope.address_kind == "lineage"' \
            --arg a "$a" || return 1
        capture_json "$PS_BASE/row04-a-after.json" as_participant "$a" inbox --json || return 1
        capture_json "$PS_BASE/row04-b-after.json" as_participant "$b" inbox --json || return 1
        assert_jq "$PS_BASE/row04-a-after.json" '([.unread[]?.id] | index($id)) == null' --arg id "$aid" || return 1
        assert_jq "$PS_BASE/row04-b-after.json" \
            '([.unread[]? | select(.id==$id and .from_participant==$a and .from_lineage=="ember")] | length) == 1' \
            --arg id "$aid" --arg a "$a"
    }

    row_05() {
        bind_explicit row05-a >"$PS_BASE/row05-a-bind.json" 2>/dev/null || { ROW_REASON="row05 A bind failed"; return 1; }
        bind_explicit row05-b >"$PS_BASE/row05-b-bind.json" 2>/dev/null || { ROW_REASON="row05 B bind failed"; return 1; }
        bind_explicit row05-c >"$PS_BASE/row05-c-bind.json" 2>/dev/null || { ROW_REASON="row05 C bind failed"; return 1; }
        a=$(jq -r '.participant.id' "$PS_BASE/row05-a-bind.json")
        b=$(jq -r '.participant.id' "$PS_BASE/row05-b-bind.json")
        c=$(jq -r '.participant.id' "$PS_BASE/row05-c-bind.json")
        capture_json "$PS_BASE/row05-new.json" as_participant "$c" identity new dormant --json || return 1
        capture_json "$PS_BASE/row05-c-leave.json" as_participant "$c" identity leave --json || return 1
        capture_json "$PS_BASE/row05-send.json" as_participant "$c" send --to lineage:dormant \
            --subject row05 --body "held lineage mail" --json || return 1
        id=$(mail_id_from "$PS_BASE/row05-send.json")
        receipt="$PS_ROOT/lineages/dormant/routing/$id.json"
        [ ! -e "$receipt" ] || { ROW_REASON="receipt existed before adopt"; return 1; }
        capture_json "$PS_BASE/row05-a-continue.json" as_participant "$a" identity continue dormant --json || return 1
        [ ! -e "$receipt" ] || { ROW_REASON="identity continue adopted held lineage mail"; return 1; }
        capture_json "$PS_BASE/row05-a-pending.json" as_participant "$a" inbox --json || return 1
        assert_jq "$PS_BASE/row05-a-pending.json" \
            '.pending_by_address["lineage:dormant"] == 1 and ([.unread[]?.id] | index($id)) == null' \
            --arg id "$id" || return 1
        [ ! -e "$receipt" ] || { ROW_REASON="plain inbox adopted held lineage mail"; return 1; }
        capture_json "$PS_BASE/row05-adopt.json" as_participant "$a" inbox --adopt --json || return 1
        [ -f "$receipt" ] || { ROW_REASON="adopt did not publish a receipt"; return 1; }
        assert_jq "$receipt" '.recipients == [$a]' --arg a "$a" || return 1
        capture_json "$PS_BASE/row05-a-inbox.json" as_participant "$a" inbox --json || return 1
        assert_jq "$PS_BASE/row05-a-inbox.json" '([.unread[]?.id] | index($id)) != null' --arg id "$id" || return 1
        capture_json "$PS_BASE/row05-b-continue.json" as_participant "$b" identity continue dormant --json || return 1
        capture_json "$PS_BASE/row05-b-inbox.json" as_participant "$b" inbox --json || return 1
        assert_jq "$PS_BASE/row05-b-inbox.json" '([.unread[]?.id] | index($id)) == null' --arg id "$id"
    }

    row_06() {
        voice="$PS_BASE/orchard-voice.md"
        terms="$PS_BASE/orchard-terms.md"
        printf '%s\n' 'ORCHARD-VOICE-OPT-IN' >"$voice"
        printf '%s\n' 'Review these continuation terms.' >"$terms"
        bind_explicit row06-founder >"$PS_BASE/row06-founder-bind.json" 2>/dev/null || { ROW_REASON="row06 founder bind failed"; return 1; }
        bind_explicit row06-viewer >"$PS_BASE/row06-viewer-bind.json" 2>/dev/null || { ROW_REASON="row06 viewer bind failed"; return 1; }
        founder=$(jq -r '.participant.id' "$PS_BASE/row06-founder-bind.json")
        viewer=$(jq -r '.participant.id' "$PS_BASE/row06-viewer-bind.json")
        capture_json "$PS_BASE/row06-new.json" as_participant "$founder" identity new orchard --json || return 1
        capture_json "$PS_BASE/row06-voice.json" as_participant "$founder" identity voice add --body-file "$voice" --json || return 1
        capture_json "$PS_BASE/row06-terms.json" as_participant "$founder" identity terms set --body-file "$terms" --json || return 1
        assert_jq "$PS_ROOT/participants/$viewer/participant.json" '.lineage == null' || return 1
        capture_json "$PS_BASE/row06-list.json" as_participant "$viewer" identity list --json || return 1
        ! grep -q 'ORCHARD-VOICE-OPT-IN' "$PS_BASE/row06-list.json" || { ROW_REASON="identity list loaded voice text"; return 1; }
        capture_json "$PS_BASE/row06-show.json" as_participant "$viewer" identity show orchard --json || return 1
        ! grep -q 'ORCHARD-VOICE-OPT-IN' "$PS_BASE/row06-show.json" || { ROW_REASON="identity show loaded voice text"; return 1; }
        capture_json "$PS_BASE/row06-show-voices.json" as_participant "$viewer" identity show orchard --voices --json || return 1
        assert_jq "$PS_BASE/row06-show-voices.json" \
            '([.rendered_voices[]? | select(contains("ORCHARD-VOICE-OPT-IN") and contains("carries no authority") and contains("authored by participant " + $founder))] | length) == 1' \
            --arg founder "$founder" || return 1
        as_participant "$viewer" identity continue orchard --json >"$PS_BASE/row06-refused.json" 2>"$PS_BASE/row06-refused.err"
        refused_rc=$?
        [ "$refused_rc" -eq 2 ] || { ROW_REASON="terms continuation was not refused with exit 2"; return 1; }
        assert_jq "$PS_BASE/row06-refused.json" \
            '.ok == false and .code == "terms_acknowledgement_required" and .terms == "Review these continuation terms.\n"' || return 1
        assert_jq "$PS_ROOT/participants/$viewer/participant.json" '.lineage == null' || return 1
        capture_json "$PS_BASE/row06-ack.json" as_participant "$viewer" identity continue orchard --acknowledge --json || return 1
        capture_json "$PS_BASE/row06-leave.json" as_participant "$viewer" identity leave --json || return 1
        assert_jq "$PS_ROOT/participants/$viewer/participant.json" '.lineage == null' || return 1
        assert_jq "$PS_ROOT/participants/$founder/participant.json" '.lineage == "orchard"'
    }

    row_07() {
        channel=workspace-default
        mkdir -p "$PS_ROOT/channels/$channel/messages" || { ROW_REASON="could not create default-member channel fixture"; return 1; }
        printf '%s\n' '{"name":"workspace-default","created":"2026-09-16 00:00:00 +0000","created_by":"smoke"}' \
            >"$PS_ROOT/channels/$channel/channel.json"
        printf '{"smoke":"2026-09-16 00:00:00 +0000"}\n' >"$PS_ROOT/channels/$channel/members.json"
        capture_json "$PS_BASE/row07-seed.json" as_c chat "$channel" --send --anyway --body "default member seed" --json || return 1
        capture_json "$PS_BASE/row07-b-read.json" as_b chat "$channel" --limit 0 --json || return 1
        cursor="$PS_ROOT/participants/$B_ID/cursors.json"
        [ -f "$cursor" ] || { ROW_REASON="B participant cursor missing before leave"; return 1; }
        assert_jq "$cursor" '.channels[$channel].seen | length > 0' --arg channel "$channel" || return 1
        cp "$cursor" "$PS_BASE/row07-cursor-before.json" 2>/dev/null
        cp "$PS_ROOT/channels/$channel/members.json" "$PS_BASE/row07-members-before.json" || { ROW_REASON="could not snapshot default members"; return 1; }
        capture_json "$PS_BASE/row07-leave.json" as_b chat "$channel" --leave --json || return 1
        cmp -s "$cursor" "$PS_BASE/row07-cursor-before.json" || { ROW_REASON="leave changed B seen state"; return 1; }
        cmp -s "$PS_ROOT/channels/$channel/members.json" "$PS_BASE/row07-members-before.json" || { ROW_REASON="leave changed workspace-default members"; return 1; }
        capture_json "$PS_BASE/row07-a-send.json" as_a chat "$channel" --send --anyway --body "A remains joined" --json || return 1
        capture_json "$PS_BASE/row07-rebind.json" as_b participant bind --workspace smoke --json || return 1
        cmp -s "$PS_ROOT/channels/$channel/members.json" "$PS_BASE/row07-members-before.json" || { ROW_REASON="rebind changed workspace-default members"; return 1; }
        as_b chat "$channel" --send --anyway --body "B must remain left" --json \
            >"$PS_BASE/row07-b-send.json" 2>"$PS_BASE/row07-b-send.err"
        /usr/bin/tail -n 1 "$PS_BASE/row07-b-send.err" >"$PS_BASE/row07-b-send-error.json"
        assert_jq "$PS_BASE/row07-b-send-error.json" \
            '.ok == false and .error.code == "not_a_member"' || return 1
        assert_jq "$PS_ROOT/participants/$B_ID/channels.json" \
            '([.left[]] | index($channel)) != null and ([.joined[]?] | index($channel)) == null' \
            --arg channel "$channel"
    }

    closed_pipe_case() {
        suffix=$1
        bind_explicit "pipe-$suffix" >"$PS_BASE/$suffix-bind.json" 2>"$PS_BASE/$suffix-bind.err" || {
            ROW_REASON="could not bind closed-pipe recipient"
            return 1
        }
        pipe_id=$(jq -r '.participant.id // empty' "$PS_BASE/$suffix-bind.json")
        [ -n "$pipe_id" ] || { ROW_REASON="closed-pipe bind returned no id"; return 1; }
        capture_json "$PS_BASE/$suffix-prime-send.json" as_c send --to workspace:smoke \
            --subject "$suffix-prime" --body prime --json || return 1
        prime_id=$(mail_id_from "$PS_BASE/$suffix-prime-send.json")
        capture_json "$PS_BASE/$suffix-prime-read.json" as_participant "$pipe_id" read "$prime_id" --json || return 1
        cursor="$PS_ROOT/participants/$pipe_id/cursors.json"
        [ -f "$cursor" ] || { ROW_REASON="prime read created no participant cursor"; return 1; }
        big="$PS_BASE/$suffix-big.txt"
        dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\000' x >"$big"
        capture_json "$PS_BASE/$suffix-big-send.json" as_c send --to workspace:smoke \
            --subject "$suffix-big" --body-file "$big" --oversize --json || return 1
        big_id=$(mail_id_from "$PS_BASE/$suffix-big-send.json")
        cp "$cursor" "$PS_BASE/$suffix-cursor-before.json"
        fifo="$PS_BASE/$suffix.fifo"
        mkfifo "$fifo" || { ROW_REASON="could not create closed-pipe fixture"; return 1; }
        first_byte="$PS_BASE/$suffix-first-byte"
        dd if="$fifo" of="$first_byte" bs=1 count=1 2>/dev/null &
        PS_FIFO_READER_PID=$!
        (
            clear_identity
            export POST_PARTICIPANT="$pipe_id"
            cd "$PS_WORK" || exit 125
            exec "$PS_BIN" read "$big_id" --json
        ) >"$fifo" 2>"$PS_BASE/$suffix-read.err" &
        PS_FIFO_WRITER_PID=$!
        if ! wait_for_pid "$PS_FIFO_READER_PID" 50; then
            ROW_REASON="closed-pipe reader timed out"
            PS_FIFO_READER_PID=
            stop_pid "$PS_FIFO_WRITER_PID"
            PS_FIFO_WRITER_PID=
            return 1
        fi
        PS_FIFO_READER_PID=
        wait_for_pid "$PS_FIFO_WRITER_PID" 50
        read_rc=$?
        if [ "$read_rc" -eq 124 ]; then
            ROW_REASON="closed-pipe writer timed out"
            PS_FIFO_WRITER_PID=
            return 1
        fi
        PS_FIFO_WRITER_PID=
        [ -s "$first_byte" ] || { ROW_REASON="closed-pipe reader observed no stdout byte"; return 1; }
        [ "$read_rc" -eq 75 ] 2>/dev/null || { ROW_REASON="closed pipe read expected output-failure exit 75, got ${read_rc:-missing}"; return 1; }
        assert_jq "$PS_BASE/$suffix-read.err" \
            '.ok == false and .error.code == "io_error" and .error.details.operation == "write stdout" and (.error.details.reason | test("broken pipe|epipe"; "i"))' || return 1
        cmp -s "$cursor" "$PS_BASE/$suffix-cursor-before.json" || { ROW_REASON="failed stdout advanced cursor bytes"; return 1; }
        ! jq -e --arg id "$big_id" '[.. | strings] | index($id) != null' "$cursor" >/dev/null || {
            ROW_REASON="failed stdout recorded the message id"
            return 1
        }
        capture_json "$PS_BASE/$suffix-positive-read.json" as_participant "$pipe_id" read "$big_id" --json || return 1
        assert_jq "$cursor" \
            '([.mail["workspace:smoke"].seen[]?] | index($id)) != null' \
            --arg id "$big_id" || return 1
        return 0
    }

    row_08() {
        inbox="$PS_ROOT/smoke/inbox"
        mkdir -p "$inbox"
        newer=20990916-030300-bbb222
        older=20990916-030200-aaa111
        printf '%s\n---\n%s\n' \
            "{\"id\":\"$newer\",\"from\":\"smoke\",\"to\":\"smoke\",\"kind\":\"note\",\"subject\":\"newer\",\"sent\":\"2026-09-16 03:03:00 -0500\",\"from_participant\":\"$C_ID\",\"address_kind\":\"workspace\"}" \
            newer >"$inbox/$newer.mail"
        write_workspace_receipt "$newer" "$A_ID" || return 1
        capture_json "$PS_BASE/row08-newer-read.json" as_a read "$newer" --json || return 1
        printf '%s\n---\n%s\n' \
            "{\"id\":\"$older\",\"from\":\"smoke\",\"to\":\"smoke\",\"kind\":\"note\",\"subject\":\"older\",\"sent\":\"2026-09-16 03:02:00 -0500\",\"from_participant\":\"$C_ID\",\"address_kind\":\"workspace\"}" \
            older >"$inbox/$older.mail"
        write_workspace_receipt "$older" "$A_ID" || return 1
        capture_json "$PS_BASE/row08-newer-reread.json" as_a read "$newer" --peek --json || return 1
        assert_jq "$PS_BASE/row08-newer-reread.json" \
            '.envelope.id == $id and .already_read == true' --arg id "$newer" || return 1
        capture_json "$PS_BASE/row08-before.json" as_a inbox --json || return 1
        assert_jq "$PS_BASE/row08-before.json" \
            '.pending_by_address["workspace:smoke"] == 0 and ([.unread[]?.id] | index($id)) != null and .unread_count == 1 and .count == 1' \
            --arg id "$older" || return 1
        capture_json "$PS_BASE/row08-older-peek.json" as_a read "$older" --peek --json || return 1
        assert_jq "$PS_BASE/row08-older-peek.json" \
            '.envelope.id == $id and (has("pending") | not) and (has("already_read") | not)' \
            --arg id "$older" || return 1
        capture_json "$PS_BASE/row08-older-read.json" as_a read "$older" --json || return 1
        assert_jq "$PS_BASE/row08-older-read.json" 'has("already_read") | not' || return 1
        closed_pipe_case row08-pipe
    }

    row_09_durable_read_suppression() {
        capture_json "$PS_BASE/row09-send.json" as_c send --to workspace:smoke \
            --subject row09 --body "watch restart" --json || return 1
        id=$(mail_id_from "$PS_BASE/row09-send.json")
        [ -n "$id" ] || { ROW_REASON="watch send returned no envelope id"; return 1; }
        (
            clear_identity
            export CLAUDE_CODE_SESSION_ID="$PS_A_KEY"
            cd "$PS_WORK" || exit 125
            exec "$PS_BIN" watch --room smoke --once --json
        ) >"$PS_BASE/row09-a-first.ndjson" 2>"$PS_BASE/row09-a-first.err" &
        PS_WATCH_PID=$!
        if ! wait_for_pid "$PS_WATCH_PID" 100; then
            ROW_REASON="A watch --once failed or timed out"
            PS_WATCH_PID=
            return 1
        fi
        PS_WATCH_PID=
        if ! jq -e -s --arg id "$id" \
            '([.[] | select(.id==$id and .address=={kind:"workspace",name:"smoke"})] | length) == 1' \
            "$PS_BASE/row09-a-first.ndjson" >/dev/null 2>&1; then
            ROW_REASON="A was not notified exactly once with the typed workspace address"
            return 1
        fi
        capture_json "$PS_BASE/row09-a-read.json" as_a read "$id" --json || return 1
        if ! run_bounded_exec "$PS_WORK" "$PS_BASE/row09-a-restart.ndjson" \
            "$PS_BASE/row09-a-restart.err" 100 env \
            CLAUDE_CODE_SESSION_ID="$PS_A_KEY" "$PS_BIN" watch --room smoke --snapshot --json; then
            ROW_REASON="A restarted snapshot failed or timed out"
            return 1
        fi
        if ! jq -e -s --arg id "$id" '([.[] | select(.id==$id)] | length) == 0' \
            "$PS_BASE/row09-a-restart.ndjson" >/dev/null 2>&1; then
            ROW_REASON="restarted watcher rang A's durably consumed id"
            return 1
        fi
        if ! run_bounded_exec "$PS_WORK" "$PS_BASE/row09-b.ndjson" \
            "$PS_BASE/row09-b.err" 100 env CODEX_THREAD_ID="$PS_B_KEY" \
            CODEX_SESSION_ID="$PS_B_KEY" "$PS_BIN" watch --room smoke --snapshot --json; then
            ROW_REASON="B snapshot failed or timed out"
            return 1
        fi
        if ! jq -e -s --arg id "$id" \
            '([.[] | select(.id==$id and .address=={kind:"workspace",name:"smoke"})] | length) == 1' \
            "$PS_BASE/row09-b.ndjson" >/dev/null 2>&1; then
            ROW_REASON="B was not independently notified exactly once"
            return 1
        fi
    }

    row_09_unread_restart_rering() {
        capture_json "$PS_BASE/row09-unread-send.json" as_c send --to workspace:smoke \
            --subject row09-unread --body "unread watcher restart" --json || return 1
        id=$(mail_id_from "$PS_BASE/row09-unread-send.json")
        [ -n "$id" ] || { ROW_REASON="unread restart send returned no envelope id"; return 1; }
        restart_index=1
        while [ "$restart_index" -le 2 ]; do
            output="$PS_BASE/row09-unread-watch-$restart_index.ndjson"
            error="$PS_BASE/row09-unread-watch-$restart_index.err"
            if ! run_bounded_exec "$PS_WORK" "$output" "$error" 100 env \
                CLAUDE_CODE_SESSION_ID="$PS_A_KEY" "$PS_BIN" watch --room smoke --once --json; then
                ROW_REASON="unread watcher invocation $restart_index failed or timed out"
                return 1
            fi
            if ! jq -e -s --arg id "$id" \
                '([.[] | select(.id==$id and .address=={kind:"workspace",name:"smoke"})] | length) == 1' \
                "$output" >/dev/null 2>&1; then
                ROW_REASON="unread id did not ring exactly once on watcher invocation $restart_index"
                return 1
            fi
            restart_index=$((restart_index + 1))
        done
        capture_json "$PS_BASE/row09-unread-still.json" as_a inbox --json || return 1
        assert_jq "$PS_BASE/row09-unread-still.json" \
            '([.unread[]?.id] | index($id)) != null' --arg id "$id"
    }

    row_10() {
        expected=${POST_SMOKE_EXPECT_CAPABILITIES:-participants,lineages,routing-receipts,cursors-v2}
        expected_store=${POST_SMOKE_EXPECT_STORE_VERSION:-2}
        expected_sha=${POST_SMOKE_EXPECT_BUILD_SHA:-}
        capture_json "$PS_BASE/row10-version.json" unbound version --json || return 1
        if ! jq -e --arg expected "$expected" --argjson store "$expected_store" --arg sha "$expected_sha" \
            '($expected | split(",") | sort) as $want | (.capabilities | sort) == $want and .store_version == $store and (.build_sha | type == "string" and length > 0 and . != "unknown") and ($sha == "" or .build_sha == $sha)' \
            "$PS_BASE/row10-version.json" >/dev/null 2>&1; then
            actual=$(jq -c '{capabilities,store_version,build_sha}' "$PS_BASE/row10-version.json" 2>/dev/null)
            ROW_REASON="version mismatch: expected capabilities=$expected store_version=$expected_store build_sha=${expected_sha:-<known>}; actual=$actual"
            return 1
        fi
    }

    lifecycle_touch() {
        bind_explicit lifecycle-touch >"$PS_BASE/lifecycle-touch-bind.json" 2>"$PS_BASE/lifecycle-touch-bind.err" || {
            ROW_REASON="touch participant bind failed"
            return 1
        }
        id=$(jq -r '.participant.id // empty' "$PS_BASE/lifecycle-touch-bind.json")
        participant_file="$PS_ROOT/participants/$id/participant.json"
        seeded=2000-01-01T00:00:00Z
        if ! jq --arg seeded "$seeded" '.last_seen=$seeded | .lease_hours=7' \
            "$participant_file" >"$participant_file.tmp"; then
            ROW_REASON="could not seed stored lease for preservation check"
            return 1
        fi
        mv "$participant_file.tmp" "$participant_file" || { ROW_REASON="could not publish stored lease for preservation check"; return 1; }
        capture_json "$PS_BASE/lifecycle-touch-preserve.json" as_participant "$id" participant touch --json || return 1
        preserve_reason=
        assert_jq "$PS_BASE/lifecycle-touch-preserve.json" \
            '.participant.id == $id and .participant.last_seen > $seeded and .participant.lease_hours == 7' \
            --arg id "$id" --arg seeded "$seeded" || preserve_reason=$ROW_REASON
        assert_jq "$participant_file" \
            '.last_seen > $seeded and .lease_hours == 7' --arg seeded "$seeded" || {
            [ -n "$preserve_reason" ] || preserve_reason=$ROW_REASON
        }
        preserved_seen=$(jq -er '.last_seen' "$participant_file") || {
            ROW_REASON="preservation check left no last_seen value"
            return 1
        }
        if ! jq '.lease_hours=7' "$participant_file" >"$participant_file.tmp"; then
            ROW_REASON="could not reseed stored lease for env override check"
            return 1
        fi
        mv "$participant_file.tmp" "$participant_file" || { ROW_REASON="could not publish stored lease for env override check"; return 1; }
        capture_json "$PS_BASE/lifecycle-touch-override.json" as_participant_with_lease "$id" 3 participant touch --json || return 1
        assert_jq "$PS_BASE/lifecycle-touch-override.json" \
            '.participant.id == $id and .participant.last_seen >= $preserved and .participant.lease_hours == 3' \
            --arg id "$id" --arg preserved "$preserved_seen" || return 1
        assert_jq "$participant_file" \
            '.last_seen >= $preserved and .lease_hours == 3' --arg preserved "$preserved_seen" || return 1
        capture_json "$PS_BASE/lifecycle-touch-who.json" as_participant "$id" who --json || return 1
        assert_jq "$PS_BASE/lifecycle-touch-who.json" \
            '.participant.id == $id and .participant.state == "active"' --arg id "$id" || return 1
        if [ -n "$preserve_reason" ]; then
            ROW_REASON=$preserve_reason
            return 1
        fi
    }

    lifecycle_who() {
        bind_explicit lifecycle-stale >"$PS_BASE/lifecycle-stale.json" 2>/dev/null || { ROW_REASON="stale bind failed"; return 1; }
        bind_explicit lifecycle-ended >"$PS_BASE/lifecycle-ended.json" 2>/dev/null || { ROW_REASON="ended bind failed"; return 1; }
        bind_explicit lifecycle-missing >"$PS_BASE/lifecycle-missing.json" 2>/dev/null || { ROW_REASON="missing-lease bind failed"; return 1; }
        stale=$(jq -r '.participant.id' "$PS_BASE/lifecycle-stale.json")
        ended=$(jq -r '.participant.id' "$PS_BASE/lifecycle-ended.json")
        missing=$(jq -r '.participant.id' "$PS_BASE/lifecycle-missing.json")
        stale_file="$PS_ROOT/participants/$stale/participant.json"
        missing_file="$PS_ROOT/participants/$missing/participant.json"
        jq '.last_seen="2000-01-01T00:00:00Z" | .lease_hours=1' "$stale_file" >"$stale_file.tmp" && mv "$stale_file.tmp" "$stale_file"
        jq 'del(.last_seen)' "$missing_file" >"$missing_file.tmp" && mv "$missing_file.tmp" "$missing_file"
        capture_json "$PS_BASE/lifecycle-end.json" as_participant "$ended" participant end --json || return 1
        capture_json "$PS_BASE/lifecycle-who.json" as_a who --json || return 1
        assert_jq "$PS_BASE/lifecycle-who.json" \
            '([.participants[] | select(.id==$a and .state=="active")] | length)==1 and ([.participants[] | select(.id==$s and .state=="stale")] | length)==1 and ([.participants[] | select(.id==$e and .state=="ended")] | length)==1 and ([.participants[] | select(.id==$m and .state=="no lease record")] | length)==1' \
            --arg a "$A_ID" --arg s "$stale" --arg e "$ended" --arg m "$missing"
    }

    lifecycle_fanout() {
        bind_explicit lifecycle-active-fanout >"$PS_BASE/lifecycle-active-fanout.json" 2>/dev/null || { ROW_REASON="active fan-out bind failed"; return 1; }
        bind_explicit lifecycle-stale-fanout >"$PS_BASE/lifecycle-stale-fanout.json" 2>/dev/null || { ROW_REASON="stale fan-out bind failed"; return 1; }
        bind_explicit lifecycle-ended-fanout >"$PS_BASE/lifecycle-ended-fanout.json" 2>/dev/null || { ROW_REASON="ended fan-out bind failed"; return 1; }
        bind_explicit lifecycle-missing-fanout >"$PS_BASE/lifecycle-missing-fanout.json" 2>/dev/null || { ROW_REASON="missing-lease fan-out bind failed"; return 1; }
        active=$(jq -r '.participant.id' "$PS_BASE/lifecycle-active-fanout.json")
        stale=$(jq -r '.participant.id' "$PS_BASE/lifecycle-stale-fanout.json")
        ended=$(jq -r '.participant.id' "$PS_BASE/lifecycle-ended-fanout.json")
        missing=$(jq -r '.participant.id' "$PS_BASE/lifecycle-missing-fanout.json")
        stale_file="$PS_ROOT/participants/$stale/participant.json"
        missing_file="$PS_ROOT/participants/$missing/participant.json"
        stale_seen=$(python3 - <<'PY'
from datetime import datetime, timedelta, timezone
print((datetime.now(timezone.utc) - timedelta(hours=2)).strftime("%Y-%m-%dT%H:%M:%SZ"))
PY
)
        jq --arg seen "$stale_seen" '.last_seen=$seen | .lease_hours=1' \
            "$stale_file" >"$stale_file.tmp" && mv "$stale_file.tmp" "$stale_file"
        jq 'del(.last_seen)' "$missing_file" >"$missing_file.tmp" && mv "$missing_file.tmp" "$missing_file"
        capture_json "$PS_BASE/lifecycle-fanout-end.json" as_participant "$ended" participant end --json || return 1
        capture_json "$PS_BASE/lifecycle-fanout-send.json" as_c send --to workspace:smoke \
            --subject lifecycle-fanout --body "active recipients only" --json || return 1
        id=$(mail_id_from "$PS_BASE/lifecycle-fanout-send.json")
        receipt="$PS_ROOT/smoke/routing/$id.json"
        [ -f "$receipt" ] || { ROW_REASON="lifecycle fan-out receipt missing"; return 1; }
        assert_jq "$receipt" \
            '([.recipients[]] | index($active)) != null and ([.recipients[]] | index($stale)) == null and ([.recipients[]] | index($ended)) == null and ([.recipients[]] | index($missing)) == null' \
            --arg active "$active" --arg stale "$stale" --arg ended "$ended" --arg missing "$missing" || return 1
        capture_json "$PS_BASE/lifecycle-active-inbox.json" as_participant "$active" inbox --json || return 1
        assert_jq "$PS_BASE/lifecycle-active-inbox.json" \
            '([.unread[]?.id] | index($id)) != null' --arg id "$id"
    }

    lifecycle_frozen() {
        bind_explicit lifecycle-frozen >"$PS_BASE/lifecycle-frozen-bind.json" 2>/dev/null || { ROW_REASON="frozen recipient bind failed"; return 1; }
        frozen=$(jq -r '.participant.id' "$PS_BASE/lifecycle-frozen-bind.json")
        capture_json "$PS_BASE/lifecycle-frozen-send.json" as_c send --to workspace:smoke \
            --subject lifecycle-frozen --body "frozen delivery" --json || return 1
        id=$(mail_id_from "$PS_BASE/lifecycle-frozen-send.json")
        receipt="$PS_ROOT/smoke/routing/$id.json"
        [ -f "$receipt" ] || { ROW_REASON="frozen-delivery receipt missing"; return 1; }
        assert_jq "$receipt" '([.recipients[]] | index($id)) != null' --arg id "$frozen" || return 1
        capture_json "$PS_BASE/lifecycle-frozen-end.json" as_participant "$frozen" participant end --json || return 1
        participant_file="$PS_ROOT/participants/$frozen/participant.json"
        cp "$participant_file" "$PS_BASE/lifecycle-frozen-ended.json" || { ROW_REASON="could not snapshot ended participant"; return 1; }
        capture_json "$PS_BASE/lifecycle-frozen-read.json" as_participant "$frozen" read "$id" --json || return 1
        assert_jq "$PS_BASE/lifecycle-frozen-read.json" '.envelope.id == $id' --arg id "$id" || return 1
        cmp -s "$participant_file" "$PS_BASE/lifecycle-frozen-ended.json" || { ROW_REASON="consuming read reactivated or changed ended participant"; return 1; }
        assert_jq "$participant_file" '.ended_at != null' || return 1
        cursor="$PS_ROOT/participants/$frozen/cursors.json"
        [ -f "$cursor" ] || { ROW_REASON="ended participant cursor missing after consumption"; return 1; }
        assert_jq "$cursor" \
            '([.mail["workspace:smoke"].seen[]?] | index($id)) != null' \
            --arg id "$id" || return 1
        capture_json "$PS_BASE/lifecycle-frozen-inbox.json" as_participant "$frozen" inbox --json || return 1
        assert_jq "$PS_BASE/lifecycle-frozen-inbox.json" \
            '([.unread[]?.id] | index($id)) == null' --arg id "$id" || return 1
        capture_json "$PS_BASE/lifecycle-frozen-who.json" as_a who --json || return 1
        assert_jq "$PS_BASE/lifecycle-frozen-who.json" \
            '([.participants[] | select(.id==$id and .state=="ended")] | length) == 1' \
            --arg id "$frozen"
    }

    unbound_watch() {
        before="$PS_BASE/unbound-watch-before.manifest"
        after="$PS_BASE/unbound-watch-after.manifest"
        manifest "$PS_ROOT" "$before" || return 1
        if ! run_bounded_exec "$PS_WORK" "$PS_BASE/unbound-watch-plain.ndjson" \
            "$PS_BASE/unbound-watch-plain.err" 100 "$PS_BIN" watch --room smoke --snapshot; then
            ROW_REASON="unbound plain snapshot failed or timed out"
            return 1
        fi
        if ! run_bounded_exec "$PS_WORK" "$PS_BASE/unbound-watch-json.ndjson" \
            "$PS_BASE/unbound-watch-json.err" 100 "$PS_BIN" watch --room smoke --snapshot --json; then
            ROW_REASON="unbound JSON snapshot failed or timed out"
            return 1
        fi
        if ! python3 - "$PS_BASE/unbound-watch-plain.ndjson" "$PS_BASE/unbound-watch-json.ndjson" <<'PY'
import json
import sys

for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as handle:
        for number, raw in enumerate(handle, 1):
            if not raw.endswith("\n"):
                raise SystemExit(f"{path}:{number}: missing NDJSON newline")
            value = json.loads(raw)
            address = value.get("address")
            if not isinstance(value.get("event"), str):
                raise SystemExit(f"{path}:{number}: event is not a string")
            if not isinstance(address, dict) or address.get("kind") not in {"workspace", "lineage", "participant"} or not isinstance(address.get("name"), str):
                raise SystemExit(f"{path}:{number}: typed address missing")
PY
        then
            ROW_REASON="unbound snapshot was not strict typed NDJSON"
            return 1
        fi
        manifest "$PS_ROOT" "$after" || return 1
        cmp -s "$before" "$after" || { ROW_REASON="unbound snapshot mutated store metadata or bytes"; return 1; }
    }

    unbound_version() {
        before="$PS_BASE/unbound-version-before.manifest"
        after="$PS_BASE/unbound-version-after.manifest"
        manifest "$PS_ROOT" "$before" || return 1
        capture_json "$PS_BASE/unbound-version.json" unbound version --json || return 1
        assert_jq "$PS_BASE/unbound-version.json" '.version | type == "string"' || return 1
        manifest "$PS_ROOT" "$after" || return 1
        cmp -s "$before" "$after" || { ROW_REASON="unbound version mutated store metadata or bytes"; return 1; }
    }

    unbound_show() {
        before="$PS_BASE/unbound-show-before.manifest"
        after="$PS_BASE/unbound-show-after.manifest"
        manifest "$PS_ROOT" "$before" || return 1
        capture_json "$PS_BASE/unbound-show.json" unbound participant show --json || return 1
        assert_jq "$PS_BASE/unbound-show.json" \
            '.ok == true and .status == "unbound" and (.fix | contains("participant bind")) and (has("participant_error") | not)' || return 1
        manifest "$PS_ROOT" "$after" || return 1
        cmp -s "$before" "$after" || { ROW_REASON="unbound participant show mutated store metadata or bytes"; return 1; }
    }

    own_channel_leave() {
        channel=own-channel-leave
        capture_json "$PS_BASE/own-a-join.json" as_a chat "$channel" --join --json || return 1
        capture_json "$PS_BASE/own-b-join.json" as_b chat "$channel" --join --json || return 1
        capture_json "$PS_BASE/own-a-seed.json" as_a chat "$channel" --send --anyway --body seed --json || return 1
        capture_json "$PS_BASE/own-b-read.json" as_b chat "$channel" --limit 0 --json || return 1
        cursor="$PS_ROOT/participants/$B_ID/cursors.json"
        cp "$cursor" "$PS_BASE/own-cursor-before.json" 2>/dev/null || { ROW_REASON="own-channel cursor missing"; return 1; }
        assert_jq "$cursor" '.channels[$channel].seen | length > 0' --arg channel "$channel" || return 1
        capture_json "$PS_BASE/own-b-leave.json" as_b chat "$channel" --leave --json || return 1
        cmp -s "$cursor" "$PS_BASE/own-cursor-before.json" || { ROW_REASON="own-channel leave changed seen state"; return 1; }
        capture_json "$PS_BASE/own-a-after.json" as_a chat "$channel" --send --anyway --body "A still joined" --json || return 1
        as_b chat "$channel" --send --anyway --body "B is left" --json \
            >"$PS_BASE/own-b-after.json" 2>"$PS_BASE/own-b-after.err"
        /usr/bin/tail -n 1 "$PS_BASE/own-b-after.err" >"$PS_BASE/own-b-after-error.json"
        assert_jq "$PS_BASE/own-b-after-error.json" \
            '.ok == false and .error.code == "not_a_member"' || return 1
        assert_jq "$PS_ROOT/participants/$B_ID/channels.json" \
            '([.left[]] | index($channel)) != null and ([.joined[]?] | index($channel)) == null' \
            --arg channel "$channel"
    }

    identity_withdraw() {
        bind_explicit withdraw-actor >"$PS_BASE/withdraw-actor.json" 2>/dev/null || { ROW_REASON="withdraw actor bind failed"; return 1; }
        bind_explicit withdraw-observer >"$PS_BASE/withdraw-observer.json" 2>/dev/null || { ROW_REASON="withdraw observer bind failed"; return 1; }
        actor=$(jq -r '.participant.id' "$PS_BASE/withdraw-actor.json")
        observer=$(jq -r '.participant.id' "$PS_BASE/withdraw-observer.json")
        capture_json "$PS_BASE/withdraw-new.json" as_participant "$actor" identity new withdrawal --json || return 1
        printf '%s\n' 'WITHDRAW-ME-FIRST' >"$PS_BASE/withdraw-voice.md"
        capture_json "$PS_BASE/withdraw-add.json" as_participant "$actor" identity voice add --body-file "$PS_BASE/withdraw-voice.md" --json || return 1
        printf '%s\n' 'WITHDRAW-ME-SECOND' >"$PS_BASE/withdraw-voice.md"
        capture_json "$PS_BASE/withdraw-revise.json" as_participant "$actor" identity voice add --body-file "$PS_BASE/withdraw-voice.md" --json || return 1
        voice_dir="$PS_ROOT/lineages/withdrawal/voices"
        [ -f "$voice_dir/$actor.md" ] || { ROW_REASON="current voice missing before withdraw"; return 1; }
        [ -f "$voice_dir/$actor.history/1.md" ] || { ROW_REASON="voice history missing before withdraw"; return 1; }
        capture_json "$PS_BASE/withdraw.json" as_participant "$actor" identity voice withdraw --json || return 1
        [ ! -e "$voice_dir/$actor.md" ] || { ROW_REASON="withdraw left current voice"; return 1; }
        [ ! -e "$voice_dir/$actor.history" ] || { ROW_REASON="withdraw left voice history"; return 1; }
        [ -f "$voice_dir/$actor.gap" ] || { ROW_REASON="withdrawal gap missing"; return 1; }
        if /usr/bin/grep -R -q -- 'WITHDRAW-ME' "$PS_ROOT/lineages/withdrawal"; then
            ROW_REASON="withdrawn voice content remained in the lineage tree"
            return 1
        fi
        assert_jq "$voice_dir/$actor.gap" '.cleanup_pending == false' || return 1
        capture_json "$PS_BASE/withdraw-show.json" as_participant "$observer" identity show withdrawal --voices --json || return 1
        assert_jq "$PS_BASE/withdraw-show.json" \
            '.withdrawn_voices == 1 and ([.rendered_voices[]] | index("[post] one voice withdrawn")) != null and ([.rendered_voices[] | select(contains("WITHDRAW-ME"))] | length) == 0'
    }

    identity_terms() {
        bind_explicit terms-founder >"$PS_BASE/terms-founder.json" 2>/dev/null || { ROW_REASON="terms founder bind failed"; return 1; }
        bind_explicit terms-continuer >"$PS_BASE/terms-continuer.json" 2>/dev/null || { ROW_REASON="terms continuer bind failed"; return 1; }
        founder=$(jq -r '.participant.id' "$PS_BASE/terms-founder.json")
        continuer=$(jq -r '.participant.id' "$PS_BASE/terms-continuer.json")
        printf '%s\n' 'Any participant may continue after review.' >"$PS_BASE/terms.md"
        capture_json "$PS_BASE/terms-new.json" as_participant "$founder" identity new terms-check --json || return 1
        capture_json "$PS_BASE/terms-set.json" as_participant "$founder" identity terms set --body-file "$PS_BASE/terms.md" --json || return 1
        as_participant "$continuer" identity continue terms-check --json >"$PS_BASE/terms-refused.json" 2>"$PS_BASE/terms-refused.err"
        [ $? -eq 2 ] || { ROW_REASON="terms did not require acknowledgement"; return 1; }
        assert_jq "$PS_BASE/terms-refused.json" \
            '.code == "terms_acknowledgement_required" and (.exact_fix | contains("--acknowledge")) and .terms == "Any participant may continue after review.\n"' || return 1
        assert_jq "$PS_ROOT/participants/$continuer/participant.json" '.lineage == null' || return 1
        capture_json "$PS_BASE/terms-ack.json" as_participant "$continuer" identity continue terms-check --acknowledge --json || return 1
        assert_jq "$PS_ROOT/participants/$continuer/participant.json" '.lineage == "terms-check"'
    }

    output_failure() {
        closed_pipe_case output-failure
    }

    record_row P13-01 "distinct participants share one workspace and appear in who" row_01
    record_row P13-02 "workspace self-suppression and B-only consumption preserve the canonical file" row_02
    record_row P13-03 "third-party workspace delivery freezes both recipients and consumes independently" row_03
    record_row P13-04 "lineage fan-out reaches affiliates and suppresses only the sender" row_04
    record_row P13-05 "held lineage mail adopts once and excludes later affiliates" row_05
    record_row P13-06 "voice loading is opt-in, terms require acknowledgement, and leave is individual" row_06
    record_row P13-07 "workspace-default channel leave survives a SessionStart-style rebind" row_07
    record_row P13-08 "late older routed ids stay unread and failed output records no seen id" row_08
    record_row P13-09-durable-read-suppression "consumed ids stay suppressed after watcher restart while independently eligible frozen recipient B still rings" row_09_durable_read_suppression
    record_row P13-09-unread-restart-rering "an unconsumed id rings again after watcher restart" row_09_unread_restart_rering
    record_row P13-10 "version advertises the expected installed capabilities" row_10
    record_row LIFECYCLE-touch "participant touch preserves the stored lease without an override and reapplies an explicit override" lifecycle_touch
    record_row LIFECYCLE-who "who labels active, stale, ended, and no lease record" lifecycle_who
    record_row LIFECYCLE-fanout "new workspace fan-out includes active participants and excludes ended, stale, and no-lease actors" lifecycle_fanout
    record_row LIFECYCLE-frozen "a consuming frozen-receipt read does not reactivate an ended participant" lifecycle_frozen
    record_row UNBOUND-watch "fully unbound snapshot output is NDJSON or empty and read-only" unbound_watch
    record_row UNBOUND-version "version --json works fully unbound and read-only" unbound_version
    record_row UNBOUND-show "participant show reports the unbound payload without mutation" unbound_show
    record_row CHANNEL-own-leave "an explicit leave preserves the leaver cursor and the peer membership" own_channel_leave
    record_row IDENTITY-withdraw "voice withdraw removes content and leaves only the anonymous gap" identity_withdraw
    record_row IDENTITY-terms "continue refuses terms until the explicit acknowledge path" identity_terms
    record_row OUTPUT-closed-pipe "closed-pipe read failure leaves the cursor byte-identical" output_failure

    [ "$PS_FAILURES" -eq 0 ]
)

legacy_smoke() (
    set +e
    BIN=$1
    BASE=$(mktemp -d 2>&1)
    LEGACY_MKTEMP_RC=$?
    if [ "$LEGACY_MKTEMP_RC" -ne 0 ] || [ -z "$BASE" ] || [ ! -d "$BASE" ]; then
        printf 'FAIL SETUP-legacy: mktemp failed: %s\n' "$BASE"
        exit 1
    fi
    LEGACY_CREATED_ROOT=$BASE
    if [ ! -w "$BASE" ]; then
        printf 'FAIL SETUP-legacy: temp root is not writable: %s\n' "$BASE"
        remove_temp_tree "$BASE" "$LEGACY_CREATED_ROOT" || true
        exit 1
    fi
    export POST_MAIL_ROOT="$BASE/mail"
    unset POST_FROM POST_SENDER_ADDRESS POST_FRAMING POST_ARX_GENERATION \
        POST_PARTICIPANT POST_HARNESS POST_PARTICIPANT_LEASE_HOURS \
        CLAUDE_CODE_SESSION_ID CLAUDE_PID CODEX_THREAD_ID \
        CODEX_SESSION_ID 2>/dev/null || true
    LEGACY_FAILURES=0
    LEGACY_REASON=
    LEGACY_SETUP_OK=0
    LEGACY_SETUP_REASON=
    LEGACY_ALPHA_ID=
    LEGACY_BETA_ID=
    WATCH_PID=
    ONCE_PID=
    FROM_NOW_PID=
    RUN_BOUNDED_PID=

    stop_watch() {
        if [ -n "$WATCH_PID" ]; then
            stop_pid "$WATCH_PID"
            WATCH_PID=
        fi
    }
    cleanup_legacy() {
        stop_pid "$ONCE_PID"
        stop_pid "$FROM_NOW_PID"
        stop_pid "$RUN_BOUNDED_PID"
        stop_watch
        if remove_temp_tree "$BASE" "$LEGACY_CREATED_ROOT"; then
            printf 'legacy smoke root removed: %s\n' "$BASE"
        else
            printf 'legacy smoke root retained after cleanup failure: %s\n' "$BASE" >&2
        fi
    }
    stop_legacy_on_signal() {
        signal_rc=$1
        trap - EXIT INT TERM
        cleanup_legacy
        exit "$signal_rc"
    }
    trap cleanup_legacy EXIT
    trap 'stop_legacy_on_signal 130' INT
    trap 'stop_legacy_on_signal 143' TERM

    legacy_record() {
        row_name=$1
        row_description=$2
        row_function=$3
        LEGACY_REASON=
        if "$row_function"; then
            printf 'ok %s: %s\n' "$row_name" "$row_description"
        else
            [ -n "$LEGACY_REASON" ] || LEGACY_REASON="row returned failure without a diagnostic (smoke bug)"
            printf 'FAIL %s: %s\n' "$row_name" "$LEGACY_REASON"
            LEGACY_FAILURES=$((LEGACY_FAILURES + 1))
        fi
    }
    require_legacy_setup() {
        if [ "$LEGACY_SETUP_OK" -ne 1 ]; then
            LEGACY_REASON="legacy scenario setup failed: $LEGACY_SETUP_REASON"
            return 1
        fi
    }
    json_field() {
        jq -er "$2 // empty" "$1" 2>/dev/null
    }
    legacy_json_actual() {
        jq -c . "$1" 2>/dev/null || printf '<invalid-json>'
    }

    legacy_doctor() {
        "$BIN" doctor --fix >"$BASE/doctor-fix.out" 2>"$BASE/doctor-fix.err" || {
            LEGACY_REASON="doctor --fix errored on fresh root: $(tr '\n' ' ' <"$BASE/doctor-fix.err")"
            return 1
        }
        "$BIN" doctor >"$BASE/doctor.out" 2>"$BASE/doctor.err" || {
            LEGACY_REASON="doctor unhealthy after --fix: $(tr '\n' ' ' <"$BASE/doctor.err")"
            return 1
        }
    }

    legacy_setup() {
        mkdir -p "$BASE/alpha" "$BASE/beta" || {
            LEGACY_SETUP_REASON="could not create legacy workspaces"
            return 1
        }
        "$BIN" rooms add alpha "$BASE/alpha" >/dev/null 2>"$BASE/rooms-alpha.err" || {
            LEGACY_SETUP_REASON="rooms add alpha failed: $(tr '\n' ' ' <"$BASE/rooms-alpha.err")"
            return 1
        }
        "$BIN" rooms add beta "$BASE/beta" >/dev/null 2>"$BASE/rooms-beta.err" || {
            LEGACY_SETUP_REASON="rooms add beta failed: $(tr '\n' ' ' <"$BASE/rooms-beta.err")"
            return 1
        }
        (
            unset POST_PARTICIPANT POST_HARNESS POST_ARX_GENERATION \
                POST_PARTICIPANT_LEASE_HOURS CLAUDE_CODE_SESSION_ID \
                CLAUDE_PID CODEX_THREAD_ID CODEX_SESSION_ID
            "$BIN" participant bind --harness smoke --key legacy-alpha \
                --workspace alpha --json
        ) >"$BASE/legacy-alpha-bind.json" 2>"$BASE/legacy-alpha-bind.err" || {
            LEGACY_SETUP_REASON="alpha participant bind failed: $(tr '\n' ' ' <"$BASE/legacy-alpha-bind.err")"
            return 1
        }
        (
            unset POST_PARTICIPANT POST_HARNESS POST_ARX_GENERATION \
                POST_PARTICIPANT_LEASE_HOURS CLAUDE_CODE_SESSION_ID \
                CLAUDE_PID CODEX_THREAD_ID CODEX_SESSION_ID
            "$BIN" participant bind --harness smoke --key legacy-beta \
                --workspace beta --json
        ) >"$BASE/legacy-beta-bind.json" 2>"$BASE/legacy-beta-bind.err" || {
            LEGACY_SETUP_REASON="beta participant bind failed: $(tr '\n' ' ' <"$BASE/legacy-beta-bind.err")"
            return 1
        }
        LEGACY_ALPHA_ID=$(json_field "$BASE/legacy-alpha-bind.json" '.participant.id') || {
            LEGACY_SETUP_REASON="alpha bind returned no participant id: $(legacy_json_actual "$BASE/legacy-alpha-bind.json")"
            return 1
        }
        LEGACY_BETA_ID=$(json_field "$BASE/legacy-beta-bind.json" '.participant.id') || {
            LEGACY_SETUP_REASON="beta bind returned no participant id: $(legacy_json_actual "$BASE/legacy-beta-bind.json")"
            return 1
        }
        export POST_PARTICIPANT="$LEGACY_ALPHA_ID"
        LEGACY_SETUP_OK=1
    }

    legacy_backlog_watch() {
        require_legacy_setup || return 1
        ( cd "$BASE/alpha" && "$BIN" send --to beta --from alpha \
            --subject backlog --body "backlog message" --json \
            >"$BASE/backlog-send.json" 2>"$BASE/backlog-send.err" ) || {
            LEGACY_REASON="backlog send failed: $(tr '\n' ' ' <"$BASE/backlog-send.err")"
            return 1
        }
        backlog_id=$(json_field "$BASE/backlog-send.json" '.envelope.id') || {
            LEGACY_REASON="backlog send returned no id: $(legacy_json_actual "$BASE/backlog-send.json")"
            return 1
        }
        ( cd "$BASE/beta" && exec env POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" watch --room beta --once --text \
            >"$BASE/backlog-watch.out" 2>"$BASE/backlog-watch.err" ) &
        ONCE_PID=$!
        if ! wait_for_pid "$ONCE_PID" 100; then
            LEGACY_REASON="default watch --once failed or timed out: $(tr '\n' ' ' <"$BASE/backlog-watch.err")"
            ONCE_PID=
            return 1
        fi
        ONCE_PID=
        /usr/bin/grep -Fq -- "$backlog_id" "$BASE/backlog-watch.out" || {
            LEGACY_REASON="default watch did not ring backlog id $backlog_id"
            return 1
        }
    }

    legacy_from_now() {
        require_legacy_setup || return 1
        ( cd "$BASE/alpha" && "$BIN" send --to beta --from alpha \
            --subject from-now-backlog --body "from now backlog" --json \
            >"$BASE/fromnow-backlog-send.json" 2>"$BASE/fromnow-backlog-send.err" ) || {
            LEGACY_REASON="from-now backlog send failed"
            return 1
        }
        backlog_id=$(json_field "$BASE/fromnow-backlog-send.json" '.envelope.id') || {
            LEGACY_REASON="from-now backlog send returned no id: $(legacy_json_actual "$BASE/fromnow-backlog-send.json")"
            return 1
        }
        ( cd "$BASE/beta" && exec env POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" watch --room beta --from now --once --text \
            >"$BASE/fromnow.out" 2>"$BASE/fromnow.err" ) &
        FROM_NOW_PID=$!
        sleep 1
        ( cd "$BASE/alpha" && "$BIN" send --to beta --from alpha \
            --subject fresh --body "fresh message" --json \
            >"$BASE/fromnow-fresh-send.json" 2>"$BASE/fromnow-fresh-send.err" ) || {
            LEGACY_REASON="post-start send failed"
            stop_pid "$FROM_NOW_PID"
            FROM_NOW_PID=
            return 1
        }
        fresh_id=$(json_field "$BASE/fromnow-fresh-send.json" '.envelope.id') || {
            LEGACY_REASON="post-start send returned no id: $(legacy_json_actual "$BASE/fromnow-fresh-send.json")"
            stop_pid "$FROM_NOW_PID"
            FROM_NOW_PID=
            return 1
        }
        if ! wait_for_pid "$FROM_NOW_PID" 100; then
            LEGACY_REASON="watch --from now --once failed or timed out: $(tr '\n' ' ' <"$BASE/fromnow.err")"
            FROM_NOW_PID=
            return 1
        fi
        FROM_NOW_PID=
        /usr/bin/grep -Fq -- "$fresh_id" "$BASE/fromnow.out" || {
            LEGACY_REASON="--from now missed the post-start arrival"
            return 1
        }
        if /usr/bin/grep -Fq -- "$backlog_id" "$BASE/fromnow.out"; then
            LEGACY_REASON="--from now leaked the backlog"
            return 1
        fi
    }

    legacy_parse_conflict() {
        require_legacy_setup || return 1
        parse_rc=0
        POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" watch --room beta \
            --from now --snapshot >/dev/null 2>&1 || parse_rc=$?
        [ "$parse_rc" -eq 2 ] || {
            LEGACY_REASON="--from now --snapshot expected exit 2, got $parse_rc"
            return 1
        }
    }

    legacy_digest_since() {
        require_legacy_setup || return 1
        channel=legacy-digest
        ( cd "$BASE/alpha" && "$BIN" chat "$channel" --join >/dev/null ) || {
            LEGACY_REASON="alpha digest join failed"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" chat "$channel" --join >/dev/null ) || {
            LEGACY_REASON="beta digest join failed"
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel" --send --body \
            "first channel msg" --anyway >/dev/null ) || {
            LEGACY_REASON="first digest message send failed"
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel" --send --body \
            "second channel msg" --anyway >/dev/null ) || {
            LEGACY_REASON="second digest message send failed"
            return 1
        }
        if ! run_bounded_exec "$BASE/beta" "$BASE/digest-snapshot.out" \
            "$BASE/digest-snapshot.err" 100 env POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" watch --room beta --snapshot --digest --text; then
            LEGACY_REASON="digest snapshot failed or timed out: $(tr '\n' ' ' <"$BASE/digest-snapshot.err")"
            return 1
        fi
        digest=$(cat "$BASE/digest-snapshot.out")
        printf '%s' "$digest" | grep -Eq "#$channel: [0-9]+ new" || {
            LEGACY_REASON="digest line missing count: $digest"
            return 1
        }
        since=$(printf '%s\n' "$digest" | sed -n "s/.*--since '\([^']*\)'.*/\1/p" | head -1)
        [ -n "$since" ] || {
            LEGACY_REASON="digest line missing --since fencepost: $digest"
            return 1
        }
        followup=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" chat "$channel" --since "$since") || {
            LEGACY_REASON="digest follow-up failed"
            return 1
        }
        printf '%s' "$followup" | grep -q "first channel msg" || {
            LEGACY_REASON="--since follow-up missed first message"
            return 1
        }
        printf '%s' "$followup" | grep -q "second channel msg" || {
            LEGACY_REASON="--since follow-up missed second message"
            return 1
        }
    }

    prepare_catchup_channel() {
        catchup_channel=$1
        ( cd "$BASE/alpha" && "$BIN" chat "$catchup_channel" --join --json >/dev/null ) || {
            LEGACY_REASON="alpha could not join catchup channel $catchup_channel"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" chat "$catchup_channel" --join --json \
            >"$BASE/$catchup_channel-beta-join.json" ) || {
            LEGACY_REASON="beta could not join catchup channel $catchup_channel"
            return 1
        }
        join_id=$(json_field "$BASE/$catchup_channel-beta-join.json" '.event_id') || {
            LEGACY_REASON="beta join returned no event id: $(legacy_json_actual "$BASE/$catchup_channel-beta-join.json")"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" chat "$catchup_channel" --discard-through "$join_id" --json >/dev/null ) || {
            LEGACY_REASON="beta could not discard catchup setup event $join_id"
            return 1
        }
        catchup_index=1
        while [ "$catchup_index" -le 3 ]; do
            ( cd "$BASE/alpha" && "$BIN" chat "$catchup_channel" --send \
                --anyway --body "catchup $catchup_index" --json \
                >"$BASE/$catchup_channel-send-$catchup_index.json" ) || {
                LEGACY_REASON="catchup setup send $catchup_index failed for $catchup_channel"
                return 1
            }
            catchup_index=$((catchup_index + 1))
        done
    }

    legacy_channels_count() {
        require_legacy_setup || return 1
        channel=legacy-count
        prepare_catchup_channel "$channel" || {
            LEGACY_REASON="could not prepare count channel"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" channels >"$BASE/channels-before-catchup.json" ) || {
            LEGACY_REASON="channels listing failed"
            return 1
        }
        if ! jq -e --arg channel "$channel" \
            '([.channels[] | select(.name==$channel and .messages >= 3 and .unread == 3)] | length) == 1' \
            "$BASE/channels-before-catchup.json" >/dev/null 2>&1; then
            LEGACY_REASON="channels did not report exactly three unread messages; actual=$(legacy_json_actual "$BASE/channels-before-catchup.json")"
            return 1
        fi
    }

    legacy_catchup_persistence() {
        require_legacy_setup || return 1
        channel=legacy-catchup
        prepare_catchup_channel "$channel" || {
            LEGACY_REASON="could not prepare catchup channel"
            return 1
        }
        first_id=$(json_field "$BASE/$channel-send-1.json" '.message.id') || {
            LEGACY_REASON="first catchup setup send returned no id: $(legacy_json_actual "$BASE/$channel-send-1.json")"
            return 1
        }
        second_id=$(json_field "$BASE/$channel-send-2.json" '.message.id') || {
            LEGACY_REASON="second catchup setup send returned no id: $(legacy_json_actual "$BASE/$channel-send-2.json")"
            return 1
        }
        third_id=$(json_field "$BASE/$channel-send-3.json" '.message.id') || {
            LEGACY_REASON="third catchup setup send returned no id: $(legacy_json_actual "$BASE/$channel-send-3.json")"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" catchup "$channel" --json >"$BASE/catchup-first.json" ) || {
            LEGACY_REASON="first catchup failed"
            return 1
        }
        if ! jq -e --arg channel "$channel" --arg one "$first_id" \
            --arg two "$second_id" --arg three "$third_id" \
            '.count == 3 and ([.targets[] | select(.source=="channel" and .channel==$channel and .count==3 and ([.messages[].id] == [$one,$two,$three]))] | length) == 1' \
            "$BASE/catchup-first.json" >/dev/null 2>&1; then
            LEGACY_REASON="first catchup did not return the exact three messages; actual=$(legacy_json_actual "$BASE/catchup-first.json")"
            return 1
        fi
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" catchup "$channel" --json >"$BASE/catchup-second.json" ) || {
            LEGACY_REASON="second catchup failed"
            return 1
        }
        if ! jq -e --arg channel "$channel" \
            '.count == 0 and ([.targets[] | select(.source=="channel" and .channel==$channel and .count==0 and (.messages|length)==0)] | length) == 1' \
            "$BASE/catchup-second.json" >/dev/null 2>&1; then
            LEGACY_REASON="second catchup was not empty; actual=$(legacy_json_actual "$BASE/catchup-second.json")"
            return 1
        fi
        cursor="$BASE/mail/participants/$LEGACY_BETA_ID/cursors.json"
        [ -f "$cursor" ] || {
            LEGACY_REASON="catchup did not persist beta participant cursor state"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" channels >"$BASE/channels-after-catchup.json" ) || {
            LEGACY_REASON="channels listing after catchup failed"
            return 1
        }
        if ! jq -e --arg channel "$channel" \
            '([.channels[] | select(.name==$channel and .unread==0)] | length) == 1' \
            "$BASE/channels-after-catchup.json" >/dev/null 2>&1; then
            LEGACY_REASON="channel did not report zero unread after catchup; actual=$(legacy_json_actual "$BASE/channels-after-catchup.json")"
            return 1
        fi
    }

    legacy_mail_consumption() {
        require_legacy_setup || return 1
        ( cd "$BASE/alpha" && "$BIN" send --to beta --from alpha --subject \
            legacy-mail --body "mail cursor observation" --json \
            >"$BASE/legacy-mail-send.json" ) || {
            LEGACY_REASON="legacy mail send failed"
            return 1
        }
        mail_id=$(json_field "$BASE/legacy-mail-send.json" '.envelope.id') || {
            LEGACY_REASON="legacy mail send returned no id: $(legacy_json_actual "$BASE/legacy-mail-send.json")"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" inbox --room beta >"$BASE/inbox-before-read.json" ) || {
            LEGACY_REASON="inbox listing before legacy mail read failed"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" read "$mail_id" --room beta --json >/dev/null ) || {
            LEGACY_REASON="mail read failed"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" inbox --room beta >"$BASE/inbox-after-read.json" ) || {
            LEGACY_REASON="inbox listing after legacy mail read failed"
            return 1
        }
        if ! jq -e --arg id "$mail_id" --slurpfile after "$BASE/inbox-after-read.json" \
            '([.unread[].id] | index($id)) != null and ([$after[0].unread[].id] | index($id)) == null and $after[0].unread_count < .unread_count' \
            "$BASE/inbox-before-read.json" >/dev/null 2>&1; then
            LEGACY_REASON="mail read did not lower unread count for the exact id; before=$(legacy_json_actual "$BASE/inbox-before-read.json"); after=$(legacy_json_actual "$BASE/inbox-after-read.json")"
            return 1
        fi
    }

    wait_for_legacy_watch_id() {
        expected=$1
        watch_attempt=0
        while [ "$watch_attempt" -lt 50 ]; do
            if /usr/bin/grep -Fq -- "$expected" "$WATCH_OUT"; then
                return 0
            fi
            if ! kill -0 "$WATCH_PID" 2>/dev/null; then
                wait_for_pid "$WATCH_PID" 1 >/dev/null 2>&1 || true
                WATCH_PID=
                LEGACY_REASON="watch exited before ringing $expected: $(tr '\n' ' ' <"$WATCH_ERR")"
                return 1
            fi
            sleep 0.1
            watch_attempt=$((watch_attempt + 1))
        done
        LEGACY_REASON="watch timed out before ringing $expected"
        return 1
    }

    legacy_long_watch() {
        require_legacy_setup || return 1
        channel=legacy-bell
        mkdir -p "$BASE/gamma" || {
            LEGACY_REASON="could not create gamma workspace"
            return 1
        }
        "$BIN" rooms add gamma "$BASE/gamma" >/dev/null || {
            LEGACY_REASON="rooms add gamma failed"
            return 1
        }
        (
            unset POST_PARTICIPANT POST_HARNESS POST_ARX_GENERATION \
                POST_PARTICIPANT_LEASE_HOURS CLAUDE_CODE_SESSION_ID \
                CLAUDE_PID CODEX_THREAD_ID CODEX_SESSION_ID
            "$BIN" participant bind --harness smoke --key legacy-gamma \
                --workspace gamma --json
        ) >"$BASE/legacy-gamma-bind.json" || {
            LEGACY_REASON="gamma participant bind failed"
            return 1
        }
        gamma_id=$(json_field "$BASE/legacy-gamma-bind.json" '.participant.id') || {
            LEGACY_REASON="gamma bind returned no participant id: $(legacy_json_actual "$BASE/legacy-gamma-bind.json")"
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel" --join --json >/dev/null ) || {
            LEGACY_REASON="alpha could not join bell channel"
            return 1
        }
        ( cd "$BASE/gamma" && POST_PARTICIPANT="$gamma_id" \
            "$BIN" chat "$channel" --join --json >"$BASE/bell-gamma-join.json" ) || {
            LEGACY_REASON="gamma could not join bell channel"
            return 1
        }
        join_id=$(json_field "$BASE/bell-gamma-join.json" '.event_id') || {
            LEGACY_REASON="gamma bell join returned no event id: $(legacy_json_actual "$BASE/bell-gamma-join.json")"
            return 1
        }
        ( cd "$BASE/gamma" && POST_PARTICIPANT="$gamma_id" \
            "$BIN" chat "$channel" --discard-through "$join_id" --json >/dev/null ) || {
            LEGACY_REASON="gamma could not discard bell join event $join_id"
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel" --send --anyway \
            --body "bell backlog" --json >"$BASE/bell-first.json" ) || {
            LEGACY_REASON="bell backlog send failed"
            return 1
        }
        first_id=$(json_field "$BASE/bell-first.json" '.message.id') || {
            LEGACY_REASON="bell backlog send returned no id: $(legacy_json_actual "$BASE/bell-first.json")"
            return 1
        }
        WATCH_OUT="$BASE/bell-watch.out"
        WATCH_ERR="$BASE/bell-watch.err"
        : >"$WATCH_OUT"
        ( cd "$BASE/gamma" && exec env POST_PARTICIPANT="$gamma_id" \
            "$BIN" watch --room gamma --interval-ms 100 \
            >"$WATCH_OUT" 2>"$WATCH_ERR" ) &
        WATCH_PID=$!
        wait_for_legacy_watch_id "$first_id" || {
            stop_watch
            return 1
        }
        ( cd "$BASE/gamma" && POST_PARTICIPANT="$gamma_id" \
            "$BIN" catchup "$channel" --json >"$BASE/bell-first-catchup.json" ) || {
            LEGACY_REASON="first bell catchup command failed"
            stop_watch
            return 1
        }
        if ! jq -e --arg channel "$channel" --arg id "$first_id" \
            '.count == 1 and ([.targets[] | select(.source=="channel" and .channel==$channel) | .messages[].id] == [$id])' \
            "$BASE/bell-first-catchup.json" >/dev/null 2>&1; then
            LEGACY_REASON="backlog catchup mismatch; actual=$(legacy_json_actual "$BASE/bell-first-catchup.json")"
            stop_watch
            return 1
        fi
        cursor="$BASE/mail/participants/$gamma_id/cursors.json"
        cp "$cursor" "$BASE/bell-cursor-before-ring" || {
            LEGACY_REASON="participant cursor missing before watch byte comparison"
            stop_watch
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel" --send --anyway \
            --body "bell after catchup" --json >"$BASE/bell-second.json" ) || {
            LEGACY_REASON="second bell message send failed"
            stop_watch
            return 1
        }
        second_id=$(json_field "$BASE/bell-second.json" '.message.id') || {
            LEGACY_REASON="second bell message returned no id: $(legacy_json_actual "$BASE/bell-second.json")"
            stop_watch
            return 1
        }
        wait_for_legacy_watch_id "$second_id" || {
            stop_watch
            return 1
        }
        /usr/bin/cmp -s "$cursor" "$BASE/bell-cursor-before-ring" || {
            LEGACY_REASON="watch ring advanced gamma cursor"
            stop_watch
            return 1
        }
        ( cd "$BASE/gamma" && POST_PARTICIPANT="$gamma_id" \
            "$BIN" catchup "$channel" --json >"$BASE/bell-second-catchup.json" ) || {
            LEGACY_REASON="second bell catchup command failed"
            stop_watch
            return 1
        }
        stop_watch
        if ! jq -e --arg channel "$channel" --arg id "$second_id" \
            '.count == 1 and ([.targets[] | select(.source=="channel" and .channel==$channel) | .messages[].id] == [$id])' \
            "$BASE/bell-second-catchup.json" >/dev/null 2>&1; then
            LEGACY_REASON="watch-ring message was not left unread; actual=$(legacy_json_actual "$BASE/bell-second-catchup.json")"
            return 1
        fi
    }

    legacy_fenced_reads() {
        require_legacy_setup || return 1
        fence_root="$BASE/fenced-mail"
        channel=fenced
        message_id=20260901-010101-000001-aaaaaa
        mkdir -p "$fence_root/fence-room" "$fence_root/channels/$channel/messages" || {
            LEGACY_REASON="could not create fenced store fixture"
            return 1
        }
        printf '{"fence-room":"%s/fence-room"}\n' "$fence_root" >"$fence_root/rooms.json"
        printf '%s\n' '{"blocked":[]}' >"$fence_root/rules.json"
        (
            unset POST_PARTICIPANT POST_HARNESS POST_ARX_GENERATION \
                POST_PARTICIPANT_LEASE_HOURS CLAUDE_CODE_SESSION_ID \
                CLAUDE_PID CODEX_THREAD_ID CODEX_SESSION_ID
            POST_MAIL_ROOT="$fence_root" "$BIN" participant bind --harness smoke \
                --key fenced-participant --workspace fence-room --json
        ) >"$BASE/fence-bind.json" || {
            LEGACY_REASON="fenced participant bind failed"
            return 1
        }
        participant=$(json_field "$BASE/fence-bind.json" '.participant.id') || {
            LEGACY_REASON="fenced bind returned no participant id: $(legacy_json_actual "$BASE/fence-bind.json")"
            return 1
        }
        printf '%s\n' '{"state":"fenced","generation":7}' >"$fence_root/.post-arx.json"
        : >"$fence_root/.post-arx.lock"
        chmod 600 "$fence_root/rooms.json" "$fence_root/rules.json" \
            "$fence_root/.post-arx.json" "$fence_root/.post-arx.lock"
        printf '%s\n' '{"name":"fenced","created":"2026-09-01 01:01:01 +0000","created_by":"fence-source"}' >"$fence_root/channels/$channel/channel.json"
        printf '%s\n' '{"fence-room":"2026-09-01 01:01:01 +0000"}' >"$fence_root/channels/$channel/members.json"
        printf '%s\n---\n%s\n' '{"id":"20260901-010101-000001-aaaaaa","from":"fence-source","channel":"fenced","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'fenced unread body' >"$fence_root/channels/$channel/messages/$message_id.msg"
        before="$BASE/fence-before.manifest"
        after="$BASE/fence-after.manifest"
        store_manifest "$fence_root" >"$before" || {
            LEGACY_REASON="could not capture fenced manifest before reads"
            return 1
        }
        fence_rc=0
        ( cd "$fence_root/fence-room" && POST_MAIL_ROOT="$fence_root" \
            POST_PARTICIPANT="$participant" "$BIN" catchup "$channel" --json \
            >"$BASE/fence-catchup.out" 2>"$BASE/fence-catchup.err" ) || fence_rc=$?
        [ "$fence_rc" -eq 78 ] || {
            LEGACY_REASON="fenced catchup expected exit 78, got $fence_rc"
            return 1
        }
        [ ! -s "$BASE/fence-catchup.out" ] || {
            LEGACY_REASON="fenced catchup emitted stdout before refusing"
            return 1
        }
        [ ! -e "$fence_root/participants/$participant/cursors.json" ] || {
            LEGACY_REASON="fenced catchup created participant cursor state"
            return 1
        }
        ( cd "$fence_root/fence-room" && POST_MAIL_ROOT="$fence_root" \
            POST_PARTICIPANT="$participant" "$BIN" chat "$channel" --peek --json \
            >"$BASE/fence-chat.json" ) || {
            LEGACY_REASON="fenced chat peek failed"
            return 1
        }
        if ! run_bounded_exec "$fence_root/fence-room" "$BASE/fence-watch.out" \
            "$BASE/fence-watch.err" 100 env POST_MAIL_ROOT="$fence_root" \
            POST_PARTICIPANT="$participant" "$BIN" watch --room fence-room --snapshot; then
            LEGACY_REASON="fenced watch snapshot failed or timed out: $(tr '\n' ' ' <"$BASE/fence-watch.err")"
            return 1
        fi
        ( cd "$fence_root/fence-room" && POST_MAIL_ROOT="$fence_root" \
            POST_PARTICIPANT="$participant" "$BIN" channels >"$BASE/fence-channels.json" ) || {
            LEGACY_REASON="fenced channels listing failed"
            return 1
        }
        ( cd "$fence_root/fence-room" && POST_MAIL_ROOT="$fence_root" \
            POST_PARTICIPANT="$participant" "$BIN" inbox --room fence-room >"$BASE/fence-inbox.json" ) || {
            LEGACY_REASON="fenced inbox listing failed"
            return 1
        }
        ( cd "$fence_root/fence-room" && POST_MAIL_ROOT="$fence_root" \
            POST_PARTICIPANT="$participant" "$BIN" search fenced --channel "$channel" --json \
            >"$BASE/fence-search.json" ) || {
            LEGACY_REASON="fenced search failed"
            return 1
        }
        /usr/bin/grep -Fq -- "$message_id" "$BASE/fence-chat.json" || {
            LEGACY_REASON="fenced chat peek omitted $message_id"
            return 1
        }
        /usr/bin/grep -Fq -- "$message_id" "$BASE/fence-watch.out" || {
            LEGACY_REASON="fenced watch snapshot omitted $message_id"
            return 1
        }
        store_manifest "$fence_root" >"$after" || {
            LEGACY_REASON="could not capture fenced manifest after reads"
            return 1
        }
        /usr/bin/cmp -s "$before" "$after" || {
            LEGACY_REASON="fenced read-only surfaces changed store metadata or bytes"
            return 1
        }
        [ ! -e "$fence_root/participants/$participant/cursors.json" ] || {
            LEGACY_REASON="fenced read-only surfaces created participant cursor state"
            return 1
        }
        [ ! -e "$fence_root/participants/$participant/.cursors.lock" ] || {
            LEGACY_REASON="fenced read-only surfaces created a participant cursor lock"
            return 1
        }
    }

    legacy_search_visibility() {
        require_legacy_setup || return 1
        channel_visible=legacy-search-visible
        channel_private=legacy-search-private
        marker=legacy-search-marker
        ( cd "$BASE/alpha" && "$BIN" chat "$channel_visible" --join --json >/dev/null ) || {
            LEGACY_REASON="alpha could not join visible search channel"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" chat "$channel_visible" --join --json >/dev/null ) || {
            LEGACY_REASON="beta could not join visible search channel"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" chat "$channel_visible" --discard --json >/dev/null ) || {
            LEGACY_REASON="beta could not discard visible search backlog"
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel_private" --join --json >/dev/null ) || {
            LEGACY_REASON="alpha could not join private search channel"
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel_visible" --send --anyway \
            --body "$marker member-visible" --json >"$BASE/search-member.json" ) || {
            LEGACY_REASON="visible search fixture send failed"
            return 1
        }
        visible_id=$(json_field "$BASE/search-member.json" '.message.id') || {
            LEGACY_REASON="visible search fixture returned no id: $(legacy_json_actual "$BASE/search-member.json")"
            return 1
        }
        ( cd "$BASE/alpha" && "$BIN" chat "$channel_private" --send --anyway \
            --body "$marker non-member" --json >"$BASE/search-private.json" ) || {
            LEGACY_REASON="private search fixture send failed"
            return 1
        }
        private_id=$(json_field "$BASE/search-private.json" '.message.id') || {
            LEGACY_REASON="private search fixture returned no id: $(legacy_json_actual "$BASE/search-private.json")"
            return 1
        }
        ( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" \
            "$BIN" search "$marker" --json >"$BASE/search-result.json" ) || {
            LEGACY_REASON="beta search command failed"
            return 1
        }
        if ! jq -e --arg visible "$channel_visible" --arg visible_id "$visible_id" \
            --arg private_id "$private_id" \
            '.count == 1 and (.results|length)==1 and .results[0].id==$visible_id and .results[0].source=="channel" and .results[0].channel==$visible and (.results[0].matched|index("body")) != null and ([.results[].id]|index($private_id)) == null' \
            "$BASE/search-result.json" >/dev/null 2>&1; then
            LEGACY_REASON="search visibility/count mismatch; actual=$(legacy_json_actual "$BASE/search-result.json")"
            return 1
        fi
    }

    legacy_cursorless_store() {
        require_legacy_setup || return 1
        root="$BASE/legacy-mail"
        channel=legacy-channel
        one=20260901-010101-000001-aaaaaa
        two=20260901-010101-000002-bbbbbb
        mail=20260901-010101-cccccc
        mkdir -p "$root/legacy-beta/inbox" "$root/channels/$channel/messages" || {
            LEGACY_REASON="could not create cursorless legacy store fixture"
            return 1
        }
        printf '{"legacy-beta":"%s/legacy-beta"}\n' "$root" >"$root/rooms.json"
        printf '%s\n' '{"blocked":[]}' >"$root/rules.json"
        (
            unset POST_PARTICIPANT POST_HARNESS POST_ARX_GENERATION \
                POST_PARTICIPANT_LEASE_HOURS CLAUDE_CODE_SESSION_ID \
                CLAUDE_PID CODEX_THREAD_ID CODEX_SESSION_ID
            POST_MAIL_ROOT="$root" "$BIN" participant bind --harness smoke \
                --key legacy-store-beta --workspace legacy-beta --json
        ) >"$BASE/legacy-store-bind.json" || {
            LEGACY_REASON="cursorless legacy participant bind failed"
            return 1
        }
        participant=$(json_field "$BASE/legacy-store-bind.json" '.participant.id') || {
            LEGACY_REASON="cursorless legacy bind returned no participant id: $(legacy_json_actual "$BASE/legacy-store-bind.json")"
            return 1
        }
        printf '%s\n' '{"name":"legacy-channel","created":"2026-09-01 01:01:01 +0000","created_by":"legacy-alpha"}' >"$root/channels/$channel/channel.json"
        printf '%s\n' '{"legacy-alpha":"2026-09-01 01:01:01 +0000","legacy-beta":"2026-09-01 01:01:01 +0000"}' >"$root/channels/$channel/members.json"
        printf '%s\n---\n%s\n' '{"id":"20260901-010101-000001-aaaaaa","from":"legacy-alpha","channel":"legacy-channel","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'legacy channel one' >"$root/channels/$channel/messages/$one.msg"
        printf '%s\n---\n%s\n' '{"id":"20260901-010101-000002-bbbbbb","from":"legacy-alpha","channel":"legacy-channel","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'legacy channel two' >"$root/channels/$channel/messages/$two.msg"
        printf '%s\n---\n%s\n' '{"id":"20260901-010101-cccccc","from":"legacy-alpha","to":"legacy-beta","kind":"note","subject":"legacy mail","sent":"2026-09-01 01:01:01 +0000"}' 'legacy mail' >"$root/legacy-beta/inbox/$mail.mail"
        before="$BASE/legacy-before.manifest"
        after="$BASE/legacy-after.manifest"
        store_manifest "$root" >"$before" || {
            LEGACY_REASON="could not capture cursorless manifest before reads"
            return 1
        }
        ( cd "$root/legacy-beta" && POST_MAIL_ROOT="$root" POST_PARTICIPANT="$participant" \
            "$BIN" channels >"$BASE/legacy-channels.json" ) || {
            LEGACY_REASON="cursorless channels listing failed"
            return 1
        }
        ( cd "$root/legacy-beta" && POST_MAIL_ROOT="$root" POST_PARTICIPANT="$participant" \
            "$BIN" inbox --room legacy-beta >"$BASE/legacy-inbox.json" ) || {
            LEGACY_REASON="cursorless inbox listing failed"
            return 1
        }
        ( cd "$root/legacy-beta" && POST_MAIL_ROOT="$root" POST_PARTICIPANT="$participant" \
            "$BIN" chat "$channel" --peek --json >"$BASE/legacy-chat.json" ) || {
            LEGACY_REASON="cursorless channel peek failed"
            return 1
        }
        if ! run_bounded_exec "$root/legacy-beta" "$BASE/legacy-watch.out" \
            "$BASE/legacy-watch.err" 100 env POST_MAIL_ROOT="$root" \
            POST_PARTICIPANT="$participant" "$BIN" watch --room legacy-beta --snapshot; then
            LEGACY_REASON="cursorless watch snapshot failed or timed out: $(tr '\n' ' ' <"$BASE/legacy-watch.err")"
            return 1
        fi
        if ! jq -e --arg channel "$channel" \
            '([.channels[] | select(.name==$channel and .messages==2 and .unread==2)] | length)==1' \
            "$BASE/legacy-channels.json" >/dev/null 2>&1; then
            LEGACY_REASON="cursorless channel was not all-unread; actual=$(legacy_json_actual "$BASE/legacy-channels.json")"
            return 1
        fi
        if ! jq -e '.count==0 and .unread_count==0 and .pending==1 and .pending_by_address["workspace:legacy-beta"]==1' \
            "$BASE/legacy-inbox.json" >/dev/null 2>&1; then
            LEGACY_REASON="cursorless inbox did not report one pending mail; actual=$(legacy_json_actual "$BASE/legacy-inbox.json")"
            return 1
        fi
        if ! jq -e --arg one "$one" --arg two "$two" \
            '.count==2 and [.messages[].id]==[$one,$two]' "$BASE/legacy-chat.json" >/dev/null 2>&1; then
            LEGACY_REASON="cursorless channel peek mismatch; actual=$(legacy_json_actual "$BASE/legacy-chat.json")"
            return 1
        fi
        /usr/bin/grep -Fq -- "$one" "$BASE/legacy-watch.out" || {
            LEGACY_REASON="cursorless watch omitted first channel id $one"
            return 1
        }
        /usr/bin/grep -Fq -- "$two" "$BASE/legacy-watch.out" || {
            LEGACY_REASON="cursorless watch omitted second channel id $two"
            return 1
        }
        [ ! -e "$root/participants/$participant/cursors.json" ] || {
            LEGACY_REASON="cursorless read-only surfaces created participant cursor state"
            return 1
        }
        [ ! -e "$root/participants/$participant/.cursors.lock" ] || {
            LEGACY_REASON="cursorless read-only surfaces created a participant cursor lock"
            return 1
        }
        store_manifest "$root" >"$after" || {
            LEGACY_REASON="could not capture cursorless manifest after reads"
            return 1
        }
        /usr/bin/cmp -s "$before" "$after" || {
            LEGACY_REASON="cursorless read-only surfaces changed store metadata or bytes"
            return 1
        }
        ( cd "$root/legacy-beta" && POST_MAIL_ROOT="$root" POST_PARTICIPANT="$participant" \
            "$BIN" catchup "$channel" --json >"$BASE/legacy-catchup.json" ) || {
            LEGACY_REASON="cursorless catchup command failed"
            return 1
        }
        if ! jq -e --arg channel "$channel" --arg one "$one" --arg two "$two" \
            '.count==2 and ([.targets[] | select(.source=="channel" and .channel==$channel) | .messages[].id] == [$one,$two])' \
            "$BASE/legacy-catchup.json" >/dev/null 2>&1; then
            LEGACY_REASON="legacy catchup did not consume both messages; actual=$(legacy_json_actual "$BASE/legacy-catchup.json")"
            return 1
        fi
        [ -f "$root/participants/$participant/cursors.json" ] || {
            LEGACY_REASON="legacy catchup did not materialize participant cursor state"
            return 1
        }
    }

    legacy_record LEGACY-01 "doctor bootstrap succeeds on a fresh root" legacy_doctor
    if legacy_setup; then :; else LEGACY_SETUP_OK=0; fi
    legacy_record LEGACY-02 "default watch rings backlog with the full id" legacy_backlog_watch
    legacy_record LEGACY-03 "--from now suppresses backlog and rings a post-start arrival" legacy_from_now
    legacy_record LEGACY-04 "--from now conflicts with --snapshot at parse" legacy_parse_conflict
    legacy_record LEGACY-05 "digest --since fencepost round-trips through chat" legacy_digest_since
    legacy_record LEGACY-06 "channels reports exactly three unread messages" legacy_channels_count
    legacy_record LEGACY-07 "catchup returns all three once and persists its cursor" legacy_catchup_persistence
    legacy_record LEGACY-08 "consuming direct mail lowers the exact unread count" legacy_mail_consumption
    legacy_record LEGACY-09 "long watch rings after catchup without advancing the cursor" legacy_long_watch
    legacy_record LEGACY-10 "fenced consuming writes refuse while read-only surfaces stay unchanged" legacy_fenced_reads
    legacy_record LEGACY-11 "search includes a member marker and excludes a non-member marker" legacy_search_visibility
    legacy_record LEGACY-12 "cursorless legacy reads stay read-only until first catchup" legacy_cursorless_store

    [ "$LEGACY_FAILURES" -eq 0 ]
)

SMOKE_FAILURES=0
participants_smoke "$BIN" || SMOKE_FAILURES=$((SMOKE_FAILURES + 1))
legacy_smoke "$BIN" || SMOKE_FAILURES=$((SMOKE_FAILURES + 1))
if [ "$SMOKE_FAILURES" -eq 0 ]; then
    printf 'SMOKE PASS\n'
    exit 0
fi
printf 'SMOKE FAIL (%s scenario section(s) failed)\n' "$SMOKE_FAILURES"
exit 1
