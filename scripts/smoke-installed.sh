#!/bin/sh
# Live smoke for an installed post binary against a throwaway mail root.
# Usage: scripts/smoke-installed.sh /path/to/post
# Red proof: /tmp/post-before-participants.64fZu4/post (verified Post 0.9.0).
# Covers: fresh-root doctor bootstrap, watch backlog ring, --from now,
# --from/--snapshot conflict, digest --since fencepost round-trip, and the
# installed participant/lineage acceptance protocol from docs/PARTICIPANTS.md.
# shellcheck disable=SC2016,SC2030,SC2031,SC2329
set -eu
BIN="$1"
case "$BIN" in
    /*) ;;
    *) BIN="$(pwd)/$BIN" ;;
esac

# This scenario deliberately accumulates every named result. A pre-participant
# binary should therefore leave a useful red-proof transcript instead of dying
# at the first unknown subcommand.
participants_smoke() (
    set +e
    PS_BIN=$1
    PS_BASE=$(mktemp -d)
    PS_ROOT="$PS_BASE/mail"
    PS_WORK="$PS_BASE/workspace"
    PS_A_KEY=aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa
    PS_B_KEY=bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb
    PS_C_KEY=cccccccc-cccc-4ccc-8ccc-cccccccccccc
    PS_FAILURES=0
    ROW_REASON=
    A_ID=claude-missing0000
    B_ID=codex-missing0000
    C_ID=smoke-missing0000
    export POST_MAIL_ROOT="$PS_ROOT"
    unset POST_FROM POST_SENDER_ADDRESS POST_FRAMING POST_ARX_GENERATION \
        POST_PARTICIPANT POST_HARNESS CLAUDE_CODE_SESSION_ID CLAUDE_PID \
        CODEX_THREAD_ID CODEX_SESSION_ID 2>/dev/null || true

    mkdir -p "$PS_WORK"
    "$PS_BIN" doctor --fix >/dev/null 2>"$PS_BASE/doctor.err"
    "$PS_BIN" rooms add smoke "$PS_WORK" >/dev/null 2>"$PS_BASE/rooms.err"

    clear_identity() {
        unset POST_PARTICIPANT POST_HARNESS POST_SENDER_ADDRESS \
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
            ROW_REASON="command failed ($capture_rc): $(tr '\n' ' ' <"$output.err")"
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
            ROW_REASON="JSON assertion failed: $assertion_filter"
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
            [ -n "$ROW_REASON" ] || ROW_REASON="assertion failed"
            printf 'FAIL %s: %s\n' "$row_name" "$ROW_REASON"
            PS_FAILURES=$((PS_FAILURES + 1))
        fi
    }
    mail_id_from() {
        jq -r '.envelope.id // empty' "$1"
    }
    manifest() {
        /usr/bin/find "$1" -type f -exec sha256sum {} + | /usr/bin/sort
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
        if [ "$BIND_A_RC" -ne 0 ] || [ "$BIND_B_RC" -ne 0 ] || [ "$BIND_C_RC" -ne 0 ]; then
            ROW_REASON="participant bind unavailable: $(tr '\n' ' ' <"$PS_BASE/bind-a.err")"
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
            --subject row02 --body "A to the shared workspace" --allow-self --json || return 1
        id=$(mail_id_from "$PS_BASE/row02-send.json")
        [ -n "$id" ] || { ROW_REASON="send returned no envelope id"; return 1; }
        capture_json "$PS_BASE/row02-b-before.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row02-b-before.json" '([.unread[]?.id] | index($id)) != null' --arg id "$id" || return 1
        capture_json "$PS_BASE/row02-a-before.json" as_a inbox --json || return 1
        assert_jq "$PS_BASE/row02-a-before.json" '([.unread[]?.id] | index($id)) == null' --arg id "$id" || return 1
        capture_json "$PS_BASE/row02-b-read.json" as_b read "$id" --json || return 1
        capture_json "$PS_BASE/row02-a-read.json" as_a read "$id" --json || return 1
        [ -f "$PS_ROOT/smoke/inbox/$id.mail" ] || { ROW_REASON="canonical inbox file moved"; return 1; }
        capture_json "$PS_BASE/row02-b-after.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row02-b-after.json" '([.unread[]?.id] | index($id)) == null' --arg id "$id"
    }

    row_03() {
        capture_json "$PS_BASE/row03-send.json" as_c send --to workspace:smoke \
            --subject row03 --body "third-party fan-out" --allow-self --json || return 1
        id=$(mail_id_from "$PS_BASE/row03-send.json")
        capture_json "$PS_BASE/row03-a-before.json" as_a inbox --json || return 1
        capture_json "$PS_BASE/row03-b-before.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row03-a-before.json" '([.unread[]?.id] | index($id)) != null' --arg id "$id" || return 1
        assert_jq "$PS_BASE/row03-b-before.json" '([.unread[]?.id] | index($id)) != null' --arg id "$id" || return 1
        capture_json "$PS_BASE/row03-a-read.json" as_a read "$id" --json || return 1
        capture_json "$PS_BASE/row03-b-still.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row03-b-still.json" '([.unread[]?.id] | index($id)) != null' --arg id "$id" || return 1
        capture_json "$PS_BASE/row03-b-read.json" as_b read "$id" --json || return 1
        receipt="$PS_ROOT/smoke/routing/$id.json"
        [ -f "$receipt" ] || { ROW_REASON="routing receipt missing"; return 1; }
        assert_jq "$receipt" \
            '([.recipients[]] | index($a)) != null and ([.recipients[]] | index($b)) != null and ([.recipients[]] | index($c)) == null and .address == {kind:"workspace",name:"smoke"}' \
            --arg a "$A_ID" --arg b "$B_ID" --arg c "$C_ID"
    }

    row_04() {
        capture_json "$PS_BASE/row04-new.json" as_a identity new ember --json || return 1
        capture_json "$PS_BASE/row04-continue.json" as_b identity continue ember --json || return 1
        capture_json "$PS_BASE/row04-c-send.json" as_c send --to lineage:ember \
            --subject row04-c --body "lineage third-party" --json || return 1
        cid=$(mail_id_from "$PS_BASE/row04-c-send.json")
        capture_json "$PS_BASE/row04-a-inbox.json" as_a inbox --json || return 1
        capture_json "$PS_BASE/row04-b-inbox.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row04-a-inbox.json" '([.unread[]?.id] | index($id)) != null' --arg id "$cid" || return 1
        assert_jq "$PS_BASE/row04-b-inbox.json" '([.unread[]?.id] | index($id)) != null' --arg id "$cid" || return 1
        capture_json "$PS_BASE/row04-a-read.json" as_a read "$cid" --json || return 1
        capture_json "$PS_BASE/row04-b-read.json" as_b read "$cid" --json || return 1
        capture_json "$PS_BASE/row04-a-send.json" as_a send --to lineage:ember \
            --subject row04-a --body "sibling lineage mail" --json || return 1
        aid=$(mail_id_from "$PS_BASE/row04-a-send.json")
        capture_json "$PS_BASE/row04-a-after.json" as_a inbox --json || return 1
        capture_json "$PS_BASE/row04-b-after.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row04-a-after.json" '([.unread[]?.id] | index($id)) == null' --arg id "$aid" || return 1
        assert_jq "$PS_BASE/row04-b-after.json" '([.unread[]?.id] | index($id)) != null' --arg id "$aid"
    }

    row_05() {
        capture_json "$PS_BASE/row05-new.json" as_c identity new dormant --json || return 1
        capture_json "$PS_BASE/row05-c-leave.json" as_c identity leave --json || return 1
        capture_json "$PS_BASE/row05-send.json" as_c send --to lineage:dormant \
            --subject row05 --body "held lineage mail" --json || return 1
        id=$(mail_id_from "$PS_BASE/row05-send.json")
        receipt="$PS_ROOT/lineages/dormant/routing/$id.json"
        [ ! -e "$receipt" ] || { ROW_REASON="receipt existed before adopt"; return 1; }
        capture_json "$PS_BASE/row05-a-leave.json" as_a identity leave --json || return 1
        capture_json "$PS_BASE/row05-a-continue.json" as_a identity continue dormant --json || return 1
        capture_json "$PS_BASE/row05-adopt.json" as_a inbox --adopt --json || return 1
        [ -f "$receipt" ] || { ROW_REASON="adopt did not publish a receipt"; return 1; }
        assert_jq "$receipt" '.recipients == [$a]' --arg a "$A_ID" || return 1
        capture_json "$PS_BASE/row05-b-leave.json" as_b identity leave --json || return 1
        capture_json "$PS_BASE/row05-b-continue.json" as_b identity continue dormant --json || return 1
        capture_json "$PS_BASE/row05-b-inbox.json" as_b inbox --json || return 1
        assert_jq "$PS_BASE/row05-b-inbox.json" '([.unread[]?.id] | index($id)) == null' --arg id "$id"
    }

    row_06() {
        voice="$PS_BASE/orchard-voice.md"
        terms="$PS_BASE/orchard-terms.md"
        printf '%s\n' 'ORCHARD-VOICE-OPT-IN' >"$voice"
        printf '%s\n' 'Review these continuation terms.' >"$terms"
        capture_json "$PS_BASE/row06-new.json" as_c identity new orchard --json || return 1
        capture_json "$PS_BASE/row06-voice.json" as_c identity voice add --body-file "$voice" --json || return 1
        capture_json "$PS_BASE/row06-terms.json" as_c identity terms set --body-file "$terms" --json || return 1
        capture_json "$PS_BASE/row06-list.json" as_a identity list --json || return 1
        ! grep -q 'ORCHARD-VOICE-OPT-IN' "$PS_BASE/row06-list.json" || { ROW_REASON="identity list loaded voice text"; return 1; }
        capture_json "$PS_BASE/row06-show.json" as_a identity show orchard --json || return 1
        ! grep -q 'ORCHARD-VOICE-OPT-IN' "$PS_BASE/row06-show.json" || { ROW_REASON="identity show loaded voice text"; return 1; }
        capture_json "$PS_BASE/row06-show-voices.json" as_a identity show orchard --voices --json || return 1
        assert_jq "$PS_BASE/row06-show-voices.json" \
            '([.rendered_voices[]?] | map(contains("ORCHARD-VOICE-OPT-IN")) | any) and ([.rendered_voices[]?] | map(contains("carries no authority")) | any)' || return 1
        capture_json "$PS_BASE/row06-a-leave.json" as_a identity leave --json || return 1
        as_a identity continue orchard --json >"$PS_BASE/row06-refused.json" 2>"$PS_BASE/row06-refused.err"
        refused_rc=$?
        [ "$refused_rc" -eq 2 ] || { ROW_REASON="terms continuation was not refused with exit 2"; return 1; }
        assert_jq "$PS_BASE/row06-refused.json" '.ok == false and .code == "terms_acknowledgement_required"' || return 1
        capture_json "$PS_BASE/row06-ack.json" as_a identity continue orchard --acknowledge --json || return 1
        capture_json "$PS_BASE/row06-leave.json" as_a identity leave --json || return 1
        assert_jq "$PS_ROOT/participants/$A_ID/participant.json" '.lineage == null' || return 1
        assert_jq "$PS_ROOT/participants/$C_ID/participant.json" '.lineage == "orchard"'
    }

    row_07() {
        channel=workspace-default
        capture_json "$PS_BASE/row07-create.json" as_a chat "$channel" --join --json || return 1
        printf '{"smoke":"2026-09-16 00:00:00 +0000"}\n' >"$PS_ROOT/channels/$channel/members.json"
        capture_json "$PS_BASE/row07-seed.json" as_c chat "$channel" --send --anyway --body "default member seed" --json || return 1
        capture_json "$PS_BASE/row07-b-read.json" as_b chat "$channel" --limit 0 --json || return 1
        cursor="$PS_ROOT/participants/$B_ID/cursors.json"
        [ -f "$cursor" ] || { ROW_REASON="B participant cursor missing before leave"; return 1; }
        cp "$cursor" "$PS_BASE/row07-cursor-before.json" 2>/dev/null
        capture_json "$PS_BASE/row07-leave.json" as_b chat "$channel" --leave --json || return 1
        cmp -s "$cursor" "$PS_BASE/row07-cursor-before.json" || { ROW_REASON="leave changed B seen state"; return 1; }
        capture_json "$PS_BASE/row07-a-send.json" as_a chat "$channel" --send --anyway --body "A remains joined" --json || return 1
        capture_json "$PS_BASE/row07-rebind.json" as_b participant bind --workspace smoke --json || return 1
        if as_b chat "$channel" --send --body "B must remain left" --json >"$PS_BASE/row07-b-send.json" 2>"$PS_BASE/row07-b-send.err"; then
            ROW_REASON="SessionStart-style rebind rejoined B"
            return 1
        fi
        assert_jq "$PS_ROOT/participants/$B_ID/channels.json" '([.left[]] | index($channel)) != null' --arg channel "$channel"
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
            --subject "$suffix-prime" --body prime --allow-self --json || return 1
        prime_id=$(mail_id_from "$PS_BASE/$suffix-prime-send.json")
        capture_json "$PS_BASE/$suffix-prime-read.json" as_participant "$pipe_id" read "$prime_id" --json || return 1
        cursor="$PS_ROOT/participants/$pipe_id/cursors.json"
        [ -f "$cursor" ] || { ROW_REASON="prime read created no participant cursor"; return 1; }
        big="$PS_BASE/$suffix-big.txt"
        dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\000' x >"$big"
        capture_json "$PS_BASE/$suffix-big-send.json" as_c send --to workspace:smoke \
            --subject "$suffix-big" --body-file "$big" --oversize --allow-self --json || return 1
        big_id=$(mail_id_from "$PS_BASE/$suffix-big-send.json")
        cp "$cursor" "$PS_BASE/$suffix-cursor-before.json"
        fifo="$PS_BASE/$suffix.fifo"
        mkfifo "$fifo" || { ROW_REASON="could not create closed-pipe fixture"; return 1; }
        dd if="$fifo" of=/dev/null bs=1 count=1 2>/dev/null &
        reader_pid=$!
        (
            as_participant "$pipe_id" read "$big_id" --json >"$fifo" 2>"$PS_BASE/$suffix-read.err"
            printf '%s\n' "$?" >"$PS_BASE/$suffix-read.status"
        ) &
        writer_pid=$!
        wait "$reader_pid"
        wait "$writer_pid"
        read_rc=$(cat "$PS_BASE/$suffix-read.status" 2>/dev/null)
        [ -n "$read_rc" ] && [ "$read_rc" -ne 0 ] || { ROW_REASON="closed pipe read reported success"; return 1; }
        cmp -s "$cursor" "$PS_BASE/$suffix-cursor-before.json" || { ROW_REASON="failed stdout advanced cursor bytes"; return 1; }
        ! jq -e --arg id "$big_id" '[.. | strings] | index($id) != null' "$cursor" >/dev/null || {
            ROW_REASON="failed stdout recorded the message id"
            return 1
        }
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
        capture_json "$PS_BASE/row08-newer-read.json" as_a read "$newer" --json || return 1
        printf '%s\n---\n%s\n' \
            "{\"id\":\"$older\",\"from\":\"smoke\",\"to\":\"smoke\",\"kind\":\"note\",\"subject\":\"older\",\"sent\":\"2026-09-16 03:02:00 -0500\",\"from_participant\":\"$C_ID\",\"address_kind\":\"workspace\"}" \
            older >"$inbox/$older.mail"
        capture_json "$PS_BASE/row08-before.json" as_a inbox --json || return 1
        assert_jq "$PS_BASE/row08-before.json" '.pending_by_address["workspace:smoke"] >= 1' || return 1
        capture_json "$PS_BASE/row08-older-read.json" as_a read "$older" --json || return 1
        assert_jq "$PS_BASE/row08-older-read.json" 'has("already_read") | not' || return 1
        closed_pipe_case row08-pipe
    }

    row_09() {
        capture_json "$PS_BASE/row09-send.json" as_c send --to workspace:smoke \
            --subject row09 --body "watch restart" --allow-self --json || return 1
        id=$(mail_id_from "$PS_BASE/row09-send.json")
        as_a watch --room smoke --once --json >"$PS_BASE/row09-a-first.ndjson" 2>"$PS_BASE/row09-a-first.err" || {
            ROW_REASON="A watch failed"
            return 1
        }
        grep -q "$id" "$PS_BASE/row09-a-first.ndjson" || { ROW_REASON="A was not notified"; return 1; }
        capture_json "$PS_BASE/row09-a-read.json" as_a read "$id" --json || return 1
        as_a watch --room smoke --snapshot --json >"$PS_BASE/row09-a-restart.ndjson" 2>"$PS_BASE/row09-a-restart.err" || {
            ROW_REASON="A restarted watch failed"
            return 1
        }
        ! grep -q "$id" "$PS_BASE/row09-a-restart.ndjson" || { ROW_REASON="restart repeated A's consumed id"; return 1; }
        as_b watch --room smoke --snapshot --json >"$PS_BASE/row09-b.ndjson" 2>"$PS_BASE/row09-b.err" || {
            ROW_REASON="B watch failed"
            return 1
        }
        grep -q "$id" "$PS_BASE/row09-b.ndjson" || { ROW_REASON="B was not independently notified"; return 1; }
    }

    row_10() {
        expected=${POST_SMOKE_EXPECT_CAPABILITIES:-participants,lineages,routing-receipts,cursors-v2}
        capture_json "$PS_BASE/row10-version.json" unbound version --json || return 1
        assert_jq "$PS_BASE/row10-version.json" \
            '($expected | split(",") | sort) as $want | (.capabilities | sort) == $want and (.build_sha | type == "string" and length > 0)' \
            --arg expected "$expected"
    }

    lifecycle_touch() {
        bind_explicit lifecycle-touch >"$PS_BASE/lifecycle-touch-bind.json" 2>"$PS_BASE/lifecycle-touch-bind.err" || {
            ROW_REASON="touch participant bind failed"
            return 1
        }
        id=$(jq -r '.participant.id // empty' "$PS_BASE/lifecycle-touch-bind.json")
        capture_json "$PS_BASE/lifecycle-touch.json" as_participant "$id" participant touch --json || return 1
        assert_jq "$PS_BASE/lifecycle-touch.json" '.participant.id == $id and (.participant.last_seen | type == "string")' --arg id "$id"
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

    lifecycle_frozen() {
        bind_explicit lifecycle-frozen >"$PS_BASE/lifecycle-frozen-bind.json" 2>/dev/null || { ROW_REASON="frozen recipient bind failed"; return 1; }
        frozen=$(jq -r '.participant.id' "$PS_BASE/lifecycle-frozen-bind.json")
        capture_json "$PS_BASE/lifecycle-frozen-send.json" as_c send --to workspace:smoke \
            --subject lifecycle-frozen --body "frozen delivery" --allow-self --json || return 1
        id=$(mail_id_from "$PS_BASE/lifecycle-frozen-send.json")
        receipt="$PS_ROOT/smoke/routing/$id.json"
        [ -f "$receipt" ] || { ROW_REASON="frozen-delivery receipt missing"; return 1; }
        assert_jq "$receipt" '([.recipients[]] | index($id)) != null' --arg id "$frozen" || return 1
        capture_json "$PS_BASE/lifecycle-frozen-end.json" as_participant "$frozen" participant end --json || return 1
        capture_json "$PS_BASE/lifecycle-frozen-read.json" as_participant "$frozen" read "$id" --json
    }

    unbound_watch() {
        before=$(manifest "$PS_ROOT")
        unbound watch --room smoke --snapshot --json >"$PS_BASE/unbound-watch.ndjson" 2>"$PS_BASE/unbound-watch.err" || {
            ROW_REASON="unbound snapshot failed"
            return 1
        }
        if [ -s "$PS_BASE/unbound-watch.ndjson" ] && ! jq -e -s 'all(.[]; type == "object" and (.event | type == "string"))' "$PS_BASE/unbound-watch.ndjson" >/dev/null 2>&1; then
            ROW_REASON="unbound snapshot emitted a non-NDJSON line"
            return 1
        fi
        after=$(manifest "$PS_ROOT")
        [ "$before" = "$after" ] || { ROW_REASON="unbound snapshot mutated the store"; return 1; }
    }

    unbound_version() {
        before=$(manifest "$PS_ROOT")
        capture_json "$PS_BASE/unbound-version.json" unbound version --json || return 1
        assert_jq "$PS_BASE/unbound-version.json" '.version | type == "string"' || return 1
        after=$(manifest "$PS_ROOT")
        [ "$before" = "$after" ] || { ROW_REASON="unbound version mutated the store"; return 1; }
    }

    unbound_show() {
        before=$(manifest "$PS_ROOT")
        capture_json "$PS_BASE/unbound-show.json" unbound participant show --json || return 1
        assert_jq "$PS_BASE/unbound-show.json" '.ok == true and .status == "unbound" and (.fix | contains("participant bind"))' || return 1
        after=$(manifest "$PS_ROOT")
        [ "$before" = "$after" ] || { ROW_REASON="unbound participant show mutated the store"; return 1; }
    }

    own_channel_leave() {
        channel=own-channel-leave
        capture_json "$PS_BASE/own-a-join.json" as_a chat "$channel" --join --json || return 1
        capture_json "$PS_BASE/own-b-join.json" as_b chat "$channel" --join --json || return 1
        capture_json "$PS_BASE/own-a-seed.json" as_a chat "$channel" --send --anyway --body seed --json || return 1
        capture_json "$PS_BASE/own-b-read.json" as_b chat "$channel" --limit 0 --json || return 1
        cursor="$PS_ROOT/participants/$B_ID/cursors.json"
        cp "$cursor" "$PS_BASE/own-cursor-before.json" 2>/dev/null || { ROW_REASON="own-channel cursor missing"; return 1; }
        capture_json "$PS_BASE/own-b-leave.json" as_b chat "$channel" --leave --json || return 1
        cmp -s "$cursor" "$PS_BASE/own-cursor-before.json" || { ROW_REASON="own-channel leave changed seen state"; return 1; }
        capture_json "$PS_BASE/own-a-after.json" as_a chat "$channel" --send --anyway --body "A still joined" --json || return 1
        if as_b chat "$channel" --send --body "B is left" --json >"$PS_BASE/own-b-after.json" 2>"$PS_BASE/own-b-after.err"; then
            ROW_REASON="leaver could still send"
            return 1
        fi
        assert_jq "$PS_ROOT/participants/$B_ID/channels.json" '([.left[]] | index($channel)) != null' --arg channel "$channel"
    }

    identity_withdraw() {
        capture_json "$PS_BASE/withdraw-leave.json" as_c identity leave --json || return 1
        capture_json "$PS_BASE/withdraw-new.json" as_c identity new withdrawal --json || return 1
        printf '%s\n' 'WITHDRAW-ME' >"$PS_BASE/withdraw-voice.md"
        capture_json "$PS_BASE/withdraw-add.json" as_c identity voice add --body-file "$PS_BASE/withdraw-voice.md" --json || return 1
        capture_json "$PS_BASE/withdraw.json" as_c identity voice withdraw --json || return 1
        [ ! -e "$PS_ROOT/lineages/withdrawal/voices/$C_ID.md" ] || { ROW_REASON="withdraw left current voice"; return 1; }
        [ -f "$PS_ROOT/lineages/withdrawal/voices/$C_ID.gap" ] || { ROW_REASON="withdrawal gap missing"; return 1; }
        capture_json "$PS_BASE/withdraw-show.json" as_a identity show withdrawal --voices --json || return 1
        assert_jq "$PS_BASE/withdraw-show.json" '.withdrawn_voices == 1 and ([.rendered_voices[]] | index("[post] one voice withdrawn")) != null'
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
        assert_jq "$PS_BASE/terms-refused.json" '.code == "terms_acknowledgement_required" and (.exact_fix | contains("--acknowledge"))' || return 1
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
    record_row P13-08 "late older ids stay unread and failed output records no seen id" row_08
    record_row P13-09 "watch restart dedupes consumed mail while another participant still rings" row_09
    record_row P13-10 "version advertises the expected installed capabilities" row_10
    record_row LIFECYCLE-touch "participant touch refreshes the participant lease" lifecycle_touch
    record_row LIFECYCLE-who "who labels active, stale, ended, and no lease record" lifecycle_who
    record_row LIFECYCLE-frozen "a frozen receipt remains readable after participant end" lifecycle_frozen
    record_row UNBOUND-watch "fully unbound snapshot output is NDJSON or empty and read-only" unbound_watch
    record_row UNBOUND-version "version --json works fully unbound and read-only" unbound_version
    record_row UNBOUND-show "participant show reports the unbound payload without mutation" unbound_show
    record_row CHANNEL-own-leave "an explicit leave preserves the leaver cursor and the peer membership" own_channel_leave
    record_row IDENTITY-withdraw "voice withdraw removes content and leaves only the anonymous gap" identity_withdraw
    record_row IDENTITY-terms "continue refuses terms until the explicit acknowledge path" identity_terms
    record_row OUTPUT-closed-pipe "closed-pipe read failure leaves the cursor byte-identical" output_failure

    printf 'participants smoke root: %s\n' "$PS_BASE"
    [ "$PS_FAILURES" -eq 0 ]
)

if ! participants_smoke "$BIN"; then
    exit 1
fi

BASE=$(mktemp -d)
export POST_MAIL_ROOT="$BASE/mail"
unset POST_FROM POST_SENDER_ADDRESS POST_FRAMING 2>/dev/null || true
fail() { printf 'SMOKE FAIL: %s\n' "$1" >&2; exit 1; }
ok() { printf 'ok: %s\n' "$1"; }
WATCH_PID=
stop_watch() {
    if [ -n "$WATCH_PID" ]; then
        kill "$WATCH_PID" 2>/dev/null || true
        wait "$WATCH_PID" 2>/dev/null || true
        WATCH_PID=
    fi
}
trap stop_watch EXIT

# 1. The papercut sequence, exactly as a set -e bootstrap runs it.
"$BIN" doctor --fix >/dev/null || fail "doctor --fix errored on fresh root"
"$BIN" doctor >/dev/null || fail "doctor unhealthy after --fix on fresh root"
ok "doctor --fix && doctor exits 0 on fresh root (papercut pc2_97e06ad)"

mkdir -p "$BASE/alpha" "$BASE/beta"
"$BIN" rooms add alpha "$BASE/alpha" >/dev/null
"$BIN" rooms add beta "$BASE/beta" >/dev/null
LEGACY_ALPHA_BIND=$(
    unset POST_PARTICIPANT CLAUDE_CODE_SESSION_ID CODEX_THREAD_ID CODEX_SESSION_ID
    "$BIN" participant bind --harness smoke --key legacy-alpha --workspace alpha --json
)
LEGACY_ALPHA_ID=$(printf '%s' "$LEGACY_ALPHA_BIND" | jq -r '.participant.id')
LEGACY_BETA_BIND=$(
    unset POST_PARTICIPANT CLAUDE_CODE_SESSION_ID CODEX_THREAD_ID CODEX_SESSION_ID
    "$BIN" participant bind --harness smoke --key legacy-beta --workspace beta --json
)
LEGACY_BETA_ID=$(printf '%s' "$LEGACY_BETA_BIND" | jq -r '.participant.id')
export POST_PARTICIPANT="$LEGACY_ALPHA_ID"

# 2. Default watch still rings the backlog (the invariant we must not break).
BACKLOG_ID=$("$BIN" send --to beta --from alpha --subject backlog --body "backlog message" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["envelope"]["id"])')
BACKLOG_OUT=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" watch --room beta --once --text)
printf '%s' "$BACKLOG_OUT" | grep -q "$BACKLOG_ID" || fail "default watch did not ring backlog id $BACKLOG_ID"
ok "default watch rings backlog, per-event line carries full id"

# 3. --from now: backlog (still unread) stays silent; a post-start arrival rings.
( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" watch --room beta --from now --once --text > "$BASE/fromnow.out" 2> "$BASE/fromnow.err" ) &
WPID=$!
sleep 2
FRESH_ID=$("$BIN" send --to beta --from alpha --subject fresh --body "fresh message" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["envelope"]["id"])')
wait "$WPID" || fail "watch --from now --once exited nonzero: $(cat "$BASE/fromnow.err")"
grep -q "$FRESH_ID" "$BASE/fromnow.out" || fail "--from now missed the post-start arrival"
if grep -q "$BACKLOG_ID" "$BASE/fromnow.out"; then fail "--from now leaked the backlog"; fi
ok "--from now suppresses backlog, rings post-start arrival"

# 4. Parse-boundary conflict.
rc=0
POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" watch --room beta --from now --snapshot >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || fail "--from now --snapshot expected exit 2, got $rc"
ok "--from now conflicts with --snapshot at parse (exit 2)"

# 5. Digest line's --since fencepost round-trips through post chat.
( cd "$BASE/alpha" && "$BIN" chat ops --join >/dev/null )
( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" chat ops --join >/dev/null )
( cd "$BASE/alpha" && "$BIN" chat ops --send --body "first channel msg" --anyway >/dev/null )
( cd "$BASE/alpha" && "$BIN" chat ops --send --body "second channel msg" --anyway >/dev/null )
DIGEST=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" watch --room beta --snapshot --digest --text)
printf '%s' "$DIGEST" | grep -Eq '#ops: [0-9]+ new' || fail "digest line missing count: $DIGEST"
SINCE=$(printf '%s\n' "$DIGEST" | sed -n "s/.*--since '\([^']*\)'.*/\1/p" | head -1)
[ -n "$SINCE" ] || fail "digest line missing --since fencepost: $DIGEST"
FOLLOWUP=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" chat ops --since "$SINCE")
printf '%s' "$FOLLOWUP" | grep -q "first channel msg" || fail "--since follow-up missed first message"
printf '%s' "$FOLLOWUP" | grep -q "second channel msg" || fail "--since follow-up missed second message"
ok "digest --since fencepost round-trips: follow-up returns the whole digest"

# Goal-lock rows 1-2: a fresh channel starts with exactly three unread
# messages, catchup consumes the complete slice once, and the cursor survives
# the second (fresh-process) invocation.
CATCHUP_CHANNEL=b8-catchup
( cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --join --json >/dev/null )
CATCHUP_BETA_JOIN=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" chat "$CATCHUP_CHANNEL" --join --json)
CATCHUP_BETA_EVENT_ID=$(printf '%s' "$CATCHUP_BETA_JOIN" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')
( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" chat "$CATCHUP_CHANNEL" --discard-through "$CATCHUP_BETA_EVENT_ID" --json >/dev/null )
CATCHUP_ONE=$(cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --send --anyway --body "catchup one" --json)
CATCHUP_ID_ONE=$(printf '%s' "$CATCHUP_ONE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
CATCHUP_TWO=$(cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --send --anyway --body "catchup two" --json)
CATCHUP_ID_TWO=$(printf '%s' "$CATCHUP_TWO" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
CATCHUP_THREE=$(cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --send --anyway --body "catchup three" --json)
CATCHUP_ID_THREE=$(printf '%s' "$CATCHUP_THREE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')

CHANNELS_BEFORE=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" channels)
printf '%s' "$CHANNELS_BEFORE" > "$BASE/channels-before-catchup.json"
python3 - "$BASE/channels-before-catchup.json" "$CATCHUP_CHANNEL" <<'PY'
import json
import sys

path, channel_name = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
channel = next((item for item in data["channels"] if item["name"] == channel_name), None)
if channel is None or channel["messages"] < 3 or channel["unread"] != 3:
    raise SystemExit(f"expected three unread messages in {channel_name}: {data}")
PY
ok "row 1: channels reports three unread messages before catchup"

CATCHUP_FIRST=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" catchup "$CATCHUP_CHANNEL" --json)
printf '%s' "$CATCHUP_FIRST" > "$BASE/catchup-first.json"
python3 - "$BASE/catchup-first.json" "$CATCHUP_CHANNEL" "$CATCHUP_ID_ONE" "$CATCHUP_ID_TWO" "$CATCHUP_ID_THREE" <<'PY'
import json
import sys

path, channel_name, *expected = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
if data["count"] != 3 or len(targets) != 1 or targets[0]["count"] != 3:
    raise SystemExit(f"catchup did not return exactly three messages: {data}")
ids = [message["id"] for message in targets[0]["messages"]]
if ids != expected:
    raise SystemExit(f"catchup ids differ: expected {expected}, got {ids}")
PY
CATCHUP_SECOND=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" catchup "$CATCHUP_CHANNEL" --json)
printf '%s' "$CATCHUP_SECOND" > "$BASE/catchup-second.json"
python3 - "$BASE/catchup-second.json" "$CATCHUP_CHANNEL" <<'PY'
import json
import sys

path, channel_name = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
if data["count"] != 0 or len(targets) != 1 or targets[0]["count"] != 0 or targets[0]["messages"]:
    raise SystemExit(f"second catchup was not empty: {data}")
PY
[ -f "$BASE/mail/participants/$LEGACY_BETA_ID/cursors.json" ] || fail "catchup did not persist beta participant cursor state"
CHANNELS_AFTER=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" channels)
printf '%s' "$CHANNELS_AFTER" > "$BASE/channels-after-catchup.json"
python3 - "$BASE/channels-after-catchup.json" "$CATCHUP_CHANNEL" <<'PY'
import json
import sys

path, channel_name = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
channel = next((item for item in data["channels"] if item["name"] == channel_name), None)
if channel is None or channel["unread"] != 0:
    raise SystemExit(f"channel did not report zero unread after catchup: {data}")
PY
ok "row 2: catchup returns all three once, then empty, with a persistent cursor"

# Goal-lock row 3: consuming one newly delivered mail lowers the per-room
# unread count and removes that id from the physical inbox listing.
MAIL_SENT=$("$BIN" send --to beta --from alpha --subject b8-mail --body "mail cursor observation" --json)
MAIL_ID=$(printf '%s' "$MAIL_SENT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["envelope"]["id"])')
MAIL_BEFORE=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" inbox --room beta)
printf '%s' "$MAIL_BEFORE" > "$BASE/inbox-before-read.json"
( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" read "$MAIL_ID" --room beta --json >/dev/null )
MAIL_AFTER=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" inbox --room beta)
printf '%s' "$MAIL_AFTER" > "$BASE/inbox-after-read.json"
python3 - "$BASE/inbox-before-read.json" "$BASE/inbox-after-read.json" "$MAIL_ID" <<'PY'
import json
import sys

before_path, after_path, mail_id = sys.argv[1:]
before = json.load(open(before_path, encoding="utf-8"))
after = json.load(open(after_path, encoding="utf-8"))
before_ids = {item["id"] for item in before["unread"]}
after_ids = {item["id"] for item in after["unread"]}
if mail_id not in before_ids or mail_id in after_ids:
    raise SystemExit(f"read did not consume the expected mail id: before={before}, after={after}")
if after["unread_count"] >= before["unread_count"]:
    raise SystemExit(f"inbox unread count did not drop: before={before}, after={after}")
PY
ok "row 3: consuming a mail read drops the room inbox unread count"

# Goal-lock row 4: arm a real long-running watch, consume its backlog, and
# prove the next send still rings. The byte comparison makes the converse
# (watch rings do not advance the cursor) non-vacuous.
WATCH_CHANNEL=b8-bell
mkdir -p "$BASE/gamma"
"$BIN" rooms add gamma "$BASE/gamma" >/dev/null
LEGACY_GAMMA_BIND=$(
    unset POST_PARTICIPANT CLAUDE_CODE_SESSION_ID CODEX_THREAD_ID CODEX_SESSION_ID
    "$BIN" participant bind --harness smoke --key legacy-gamma --workspace gamma --json
)
LEGACY_GAMMA_ID=$(printf '%s' "$LEGACY_GAMMA_BIND" | jq -r '.participant.id')
( cd "$BASE/alpha" && "$BIN" chat "$WATCH_CHANNEL" --join --json >/dev/null )
BELL_GAMMA_JOIN=$(cd "$BASE/gamma" && POST_PARTICIPANT="$LEGACY_GAMMA_ID" "$BIN" chat "$WATCH_CHANNEL" --join --json)
BELL_GAMMA_EVENT_ID=$(printf '%s' "$BELL_GAMMA_JOIN" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')
( cd "$BASE/gamma" && POST_PARTICIPANT="$LEGACY_GAMMA_ID" "$BIN" chat "$WATCH_CHANNEL" --discard-through "$BELL_GAMMA_EVENT_ID" --json >/dev/null )
BELL_FIRST=$(cd "$BASE/alpha" && "$BIN" chat "$WATCH_CHANNEL" --send --anyway --body "bell backlog" --json)
BELL_FIRST_ID=$(printf '%s' "$BELL_FIRST" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
WATCH_OUT="$BASE/bell-watch.out"
WATCH_ERR="$BASE/bell-watch.err"
: > "$WATCH_OUT"
(
    cd "$BASE/gamma"
    exec env POST_PARTICIPANT="$LEGACY_GAMMA_ID" "$BIN" watch --room gamma --interval-ms 100 >"$WATCH_OUT" 2>"$WATCH_ERR"
) &
WATCH_PID=$!
wait_for_watch_id() {
    expected=$1
    attempt=0
    while [ "$attempt" -lt 50 ]; do
        if /usr/bin/grep -Fq -- "$expected" "$WATCH_OUT"; then
            return 0
        fi
        if ! kill -0 "$WATCH_PID" 2>/dev/null; then
            wait "$WATCH_PID" 2>/dev/null || true
            fail "watch exited before ringing $expected: $(cat "$WATCH_ERR")"
        fi
        sleep 0.1
        attempt=$((attempt + 1))
    done
    fail "watch did not ring $expected"
}
wait_for_watch_id "$BELL_FIRST_ID"
BELL_CAUGHT=$(cd "$BASE/gamma" && POST_PARTICIPANT="$LEGACY_GAMMA_ID" "$BIN" catchup "$WATCH_CHANNEL" --json)
printf '%s' "$BELL_CAUGHT" > "$BASE/bell-first-catchup.json"
python3 - "$BASE/bell-first-catchup.json" "$WATCH_CHANNEL" "$BELL_FIRST_ID" <<'PY'
import json
import sys

path, channel_name, expected_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
ids = [message["id"] for message in targets[0]["messages"]] if targets else []
if data["count"] != 1 or ids != [expected_id]:
    raise SystemExit(f"backlog catchup mismatch: {data}")
PY
cp "$BASE/mail/participants/$LEGACY_GAMMA_ID/cursors.json" "$BASE/bell-cursor-before-ring"
BELL_SECOND=$(cd "$BASE/alpha" && "$BIN" chat "$WATCH_CHANNEL" --send --anyway --body "bell after catchup" --json)
BELL_SECOND_ID=$(printf '%s' "$BELL_SECOND" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
wait_for_watch_id "$BELL_SECOND_ID"
/usr/bin/cmp -s "$BASE/mail/participants/$LEGACY_GAMMA_ID/cursors.json" "$BASE/bell-cursor-before-ring" || fail "watch ring advanced gamma cursor"
BELL_SECOND_CAUGHT=$(cd "$BASE/gamma" && POST_PARTICIPANT="$LEGACY_GAMMA_ID" "$BIN" catchup "$WATCH_CHANNEL" --json)
printf '%s' "$BELL_SECOND_CAUGHT" > "$BASE/bell-second-catchup.json"
python3 - "$BASE/bell-second-catchup.json" "$WATCH_CHANNEL" "$BELL_SECOND_ID" <<'PY'
import json
import sys

path, channel_name, expected_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
ids = [message["id"] for message in targets[0]["messages"]] if targets else []
if data["count"] != 1 or ids != [expected_id]:
    raise SystemExit(f"watch-ring message was not left unread: {data}")
PY
stop_watch
ok "row 4: watch rings after catchup and leaves ring-only messages unread (byte-compare)"

# Goal-lock row 5: a fenced store refuses the consuming writer before any
# cursor mutation, while peek/snapshot/listing/search surfaces remain usable
# and byte-for-byte read-only.
FENCE_ROOT="$BASE/fenced-mail"
FENCE_CHANNEL=fenced
FENCE_ID=20260901-010101-000001-aaaaaa
mkdir -p "$FENCE_ROOT/fence-room" "$FENCE_ROOT/channels/$FENCE_CHANNEL/messages"
printf '{"fence-room":"%s/fence-room"}\n' "$FENCE_ROOT" > "$FENCE_ROOT/rooms.json"
printf '%s\n' '{"blocked":[]}' > "$FENCE_ROOT/rules.json"
FENCE_BIND=$(
    POST_MAIL_ROOT="$FENCE_ROOT" "$BIN" participant bind --harness smoke \
        --key fenced-participant --workspace fence-room --json
)
FENCE_PARTICIPANT_ID=$(printf '%s' "$FENCE_BIND" | jq -r '.participant.id')
printf '%s\n' '{"state":"fenced","generation":7}' > "$FENCE_ROOT/.post-arx.json"
: > "$FENCE_ROOT/.post-arx.lock"
chmod 600 "$FENCE_ROOT/rooms.json" "$FENCE_ROOT/rules.json" "$FENCE_ROOT/.post-arx.json" "$FENCE_ROOT/.post-arx.lock"
printf '%s\n' '{"name":"fenced","created":"2026-09-01 01:01:01 +0000","created_by":"fence-source"}' > "$FENCE_ROOT/channels/$FENCE_CHANNEL/channel.json"
printf '%s\n' '{"fence-room":"2026-09-01 01:01:01 +0000"}' > "$FENCE_ROOT/channels/$FENCE_CHANNEL/members.json"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-000001-aaaaaa","from":"fence-source","channel":"fenced","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'fenced unread body' > "$FENCE_ROOT/channels/$FENCE_CHANNEL/messages/$FENCE_ID.msg"
export POST_MAIL_ROOT="$FENCE_ROOT"
unset POST_ARX_GENERATION
fence_manifest() { /usr/bin/find "$1" -type f -exec sha256sum {} + | /usr/bin/sort; }
FENCE_BEFORE=$(fence_manifest "$FENCE_ROOT")
FENCE_CATCHUP_OUT="$BASE/fence-catchup.out"
FENCE_CATCHUP_ERR="$BASE/fence-catchup.err"
rc=0
( cd "$FENCE_ROOT/fence-room" && POST_PARTICIPANT="$FENCE_PARTICIPANT_ID" "$BIN" catchup "$FENCE_CHANNEL" --json >"$FENCE_CATCHUP_OUT" 2>"$FENCE_CATCHUP_ERR" ) || rc=$?
[ "$rc" -eq 78 ] || fail "fenced catchup expected exit 78, got $rc"
[ ! -s "$FENCE_CATCHUP_OUT" ] || fail "fenced catchup emitted stdout before refusing"
[ ! -e "$FENCE_ROOT/participants/$FENCE_PARTICIPANT_ID/cursors.json" ] || fail "fenced catchup created participant cursor state"
( cd "$FENCE_ROOT/fence-room" && POST_PARTICIPANT="$FENCE_PARTICIPANT_ID" "$BIN" chat "$FENCE_CHANNEL" --peek --json >"$BASE/fence-chat.json" )
( cd "$FENCE_ROOT/fence-room" && POST_PARTICIPANT="$FENCE_PARTICIPANT_ID" "$BIN" watch --room fence-room --snapshot >"$BASE/fence-watch.out" )
( cd "$FENCE_ROOT/fence-room" && POST_PARTICIPANT="$FENCE_PARTICIPANT_ID" "$BIN" channels >"$BASE/fence-channels.json" )
( cd "$FENCE_ROOT/fence-room" && POST_PARTICIPANT="$FENCE_PARTICIPANT_ID" "$BIN" inbox --room fence-room >"$BASE/fence-inbox.json" )
( cd "$FENCE_ROOT/fence-room" && POST_PARTICIPANT="$FENCE_PARTICIPANT_ID" "$BIN" search fenced --channel "$FENCE_CHANNEL" --json >"$BASE/fence-search.json" )
/usr/bin/grep -Fq -- "$FENCE_ID" "$BASE/fence-chat.json" || fail "fenced peek missed channel message"
/usr/bin/grep -Fq -- "$FENCE_ID" "$BASE/fence-watch.out" || fail "fenced snapshot missed channel message"
FENCE_AFTER=$(fence_manifest "$FENCE_ROOT")
[ "$FENCE_BEFORE" = "$FENCE_AFTER" ] || fail "fenced read-only surfaces changed store bytes"
[ ! -e "$FENCE_ROOT/participants/$FENCE_PARTICIPANT_ID/cursors.json" ] || fail "fenced read-only surfaces created participant cursor state"
[ ! -e "$FENCE_ROOT/participants/$FENCE_PARTICIPANT_ID/.cursors.lock" ] || fail "fenced read-only surfaces created participant cursor lock"
ok "row 5: fenced catchup refuses, while peek/snapshot/listings/search stay read-only"

# Goal-lock row 6: search returns a member-channel marker but excludes a
# planted marker in a channel beta is not a member of.
export POST_MAIL_ROOT="$BASE/mail"
SEARCH_VISIBLE=b8-search-visible
SEARCH_PRIVATE=b8-search-private
SEARCH_MARKER=b8-search-marker
( cd "$BASE/alpha" && "$BIN" chat "$SEARCH_VISIBLE" --join --json >/dev/null )
( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" chat "$SEARCH_VISIBLE" --join --json >/dev/null )
( cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" chat "$SEARCH_VISIBLE" --discard --json >/dev/null )
( cd "$BASE/alpha" && "$BIN" chat "$SEARCH_PRIVATE" --join --json >/dev/null )
SEARCH_MEMBER=$(cd "$BASE/alpha" && "$BIN" chat "$SEARCH_VISIBLE" --send --anyway --body "$SEARCH_MARKER member-visible" --json)
SEARCH_MEMBER_ID=$(printf '%s' "$SEARCH_MEMBER" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
SEARCH_NON_MEMBER=$(cd "$BASE/alpha" && "$BIN" chat "$SEARCH_PRIVATE" --send --anyway --body "$SEARCH_MARKER non-member" --json)
SEARCH_NON_MEMBER_ID=$(printf '%s' "$SEARCH_NON_MEMBER" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
SEARCH_RESULT=$(cd "$BASE/beta" && POST_PARTICIPANT="$LEGACY_BETA_ID" "$BIN" search "$SEARCH_MARKER" --json)
printf '%s' "$SEARCH_RESULT" > "$BASE/search-result.json"
python3 - "$BASE/search-result.json" "$SEARCH_VISIBLE" "$SEARCH_MEMBER_ID" "$SEARCH_NON_MEMBER_ID" <<'PY'
import json
import sys

path, visible_channel, visible_id, private_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
ids = [result["id"] for result in data["results"]]
if data["count"] != 1 or len(ids) != 1 or ids[0] != visible_id:
    raise SystemExit(f"search visibility/count mismatch: {data}")
result = data["results"][0]
if result["source"] != "channel" or result["channel"] != visible_channel or "body" not in result["matched"]:
    raise SystemExit(f"member-channel result has wrong shape: {result}")
if private_id in ids:
    raise SystemExit(f"non-member marker leaked into search: {data}")
PY
ok "row 6: search returns the member marker and excludes the planted non-member marker"

# Goal-lock row 7: a cursorless old store is readable as all-unread and stays
# untouched until the first consuming catchup materializes cursors.json.
LEGACY_ROOT="$BASE/legacy-mail"
LEGACY_CHANNEL=legacy-channel
LEGACY_ONE=20260901-010101-000001-aaaaaa
LEGACY_TWO=20260901-010101-000002-bbbbbb
LEGACY_MAIL=20260901-010101-cccccc
mkdir -p "$LEGACY_ROOT/legacy-beta/inbox" "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/messages"
printf '{"legacy-beta":"%s/legacy-beta"}\n' "$LEGACY_ROOT" > "$LEGACY_ROOT/rooms.json"
printf '%s\n' '{"blocked":[]}' > "$LEGACY_ROOT/rules.json"
export POST_MAIL_ROOT="$LEGACY_ROOT"
unset POST_ARX_GENERATION
LEGACY_STORE_BIND=$(
    "$BIN" participant bind --harness smoke --key legacy-store-beta \
        --workspace legacy-beta --json
)
LEGACY_STORE_PARTICIPANT=$(printf '%s' "$LEGACY_STORE_BIND" | jq -r '.participant.id')
printf '%s\n' '{"name":"legacy-channel","created":"2026-09-01 01:01:01 +0000","created_by":"legacy-alpha"}' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/channel.json"
printf '%s\n' '{"legacy-alpha":"2026-09-01 01:01:01 +0000","legacy-beta":"2026-09-01 01:01:01 +0000"}' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/members.json"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-000001-aaaaaa","from":"legacy-alpha","channel":"legacy-channel","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'legacy channel one' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/messages/$LEGACY_ONE.msg"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-000002-bbbbbb","from":"legacy-alpha","channel":"legacy-channel","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'legacy channel two' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/messages/$LEGACY_TWO.msg"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-cccccc","from":"legacy-alpha","to":"legacy-beta","kind":"note","subject":"legacy mail","sent":"2026-09-01 01:01:01 +0000"}' 'legacy mail' > "$LEGACY_ROOT/legacy-beta/inbox/$LEGACY_MAIL.mail"
LEGACY_BEFORE=$(fence_manifest "$LEGACY_ROOT")
( cd "$LEGACY_ROOT/legacy-beta" && POST_PARTICIPANT="$LEGACY_STORE_PARTICIPANT" "$BIN" channels >"$BASE/legacy-channels.json" )
( cd "$LEGACY_ROOT/legacy-beta" && POST_PARTICIPANT="$LEGACY_STORE_PARTICIPANT" "$BIN" inbox --room legacy-beta >"$BASE/legacy-inbox.json" )
( cd "$LEGACY_ROOT/legacy-beta" && POST_PARTICIPANT="$LEGACY_STORE_PARTICIPANT" "$BIN" chat "$LEGACY_CHANNEL" --peek --json >"$BASE/legacy-chat.json" )
( cd "$LEGACY_ROOT/legacy-beta" && POST_PARTICIPANT="$LEGACY_STORE_PARTICIPANT" "$BIN" watch --room legacy-beta --snapshot >"$BASE/legacy-watch.out" )
python3 - "$BASE/legacy-channels.json" "$BASE/legacy-inbox.json" "$BASE/legacy-chat.json" "$LEGACY_CHANNEL" "$LEGACY_ONE" "$LEGACY_TWO" "$LEGACY_MAIL" <<'PY'
import json
import sys

channels_path, inbox_path, chat_path, channel_name, first_id, second_id, mail_id = sys.argv[1:]
channels = json.load(open(channels_path, encoding="utf-8"))
channel = next((item for item in channels["channels"] if item["name"] == channel_name), None)
if channel is None or channel["messages"] != 2 or channel["unread"] != 2:
    raise SystemExit(f"cursorless channel was not all-unread: {channels}")
inbox = json.load(open(inbox_path, encoding="utf-8"))
if inbox["count"] != 0 or inbox["unread_count"] != 0 or inbox["pending"] != 1 or inbox["pending_by_address"].get("workspace:legacy-beta") != 1:
    raise SystemExit(f"cursorless inbox mismatch: {inbox}")
chat = json.load(open(chat_path, encoding="utf-8"))
ids = [message["id"] for message in chat["messages"]]
if chat["count"] != 2 or ids != [first_id, second_id]:
    raise SystemExit(f"cursorless chat mismatch: {chat}")
PY
/usr/bin/grep -Fq -- "$LEGACY_ONE" "$BASE/legacy-watch.out" || fail "legacy snapshot missed first channel message"
/usr/bin/grep -Fq -- "$LEGACY_TWO" "$BASE/legacy-watch.out" || fail "legacy snapshot missed second channel message"
[ ! -e "$LEGACY_ROOT/participants/$LEGACY_STORE_PARTICIPANT/cursors.json" ] || fail "legacy read-only surfaces migrated participant cursor state"
[ ! -e "$LEGACY_ROOT/participants/$LEGACY_STORE_PARTICIPANT/.cursors.lock" ] || fail "legacy read-only surfaces created participant cursor lock"
LEGACY_AFTER=$(fence_manifest "$LEGACY_ROOT")
[ "$LEGACY_BEFORE" = "$LEGACY_AFTER" ] || fail "legacy read-only surfaces changed store bytes"
LEGACY_CAUGHT=$(cd "$LEGACY_ROOT/legacy-beta" && POST_PARTICIPANT="$LEGACY_STORE_PARTICIPANT" "$BIN" catchup "$LEGACY_CHANNEL" --json)
printf '%s' "$LEGACY_CAUGHT" > "$BASE/legacy-catchup.json"
python3 - "$BASE/legacy-catchup.json" "$LEGACY_CHANNEL" "$LEGACY_ONE" "$LEGACY_TWO" <<'PY'
import json
import sys

path, channel_name, first_id, second_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
ids = [message["id"] for message in targets[0]["messages"]] if targets else []
if data["count"] != 2 or ids != [first_id, second_id]:
    raise SystemExit(f"legacy catchup did not consume both messages: {data}")
PY
[ -f "$LEGACY_ROOT/participants/$LEGACY_STORE_PARTICIPANT/cursors.json" ] || fail "legacy catchup did not materialize participant cursor state"
export POST_MAIL_ROOT="$BASE/mail"
ok "row 7: cursorless legacy reads are all-unread and first catchup materializes state"

echo "SMOKE PASS (root: $BASE)"
