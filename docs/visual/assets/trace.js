/* ===========================================================================
 * Trace — the channel field.
 *
 * Everything here changes this page's own explanatory state. Nothing touches a
 * real mailbox. The field is a model of the two read-state designs, built so
 * that no control can produce a state the real system cannot produce.
 * ======================================================================== */
(function () {
  "use strict";

  var SLOTS = 4;

  var PEOPLE = [
    { id: "astra", name: "Astra", ch: "a", glyph: "g-square", pid: "codex-7f3a91c4" },
    { id: "fable", name: "Fable", ch: "b", glyph: "g-circle", pid: "claude-2c68ade0" }
  ];

  var SEED = [{ mid: "m-4f21", from: "trey", to: "tower" }];

  /* ---------------------------------------------------------------- state */

  var state;

  function reset() {
    state = {
      mode: "participants",
      messages: SEED.map(function (m) { return { mid: m.mid, from: m.from, to: m.to }; }),
      seen: { astra: {}, fable: {} },   // participant id -> { messageId: true }
      roomSeen: {},                     // the pre-participant cursors.json seen set
      counter: 0,
      readout: null,
      lastEvent: null
    };
  }

  var IDS = ["m-8b07", "m-c193", "m-2e5a", "m-a740", "m-61df", "m-d3c8"];
  function nextId() { return IDS[state.counter++ % IDS.length]; }

  /* --------------------------------------------------------- the model ---
   * cellFor(person, message) returns what that participant's rail shows.
   *   "unread"     the receipt names this participant and the id is unseen
   *   "read"       the receipt names this participant and the id is in its seen set
   *   "suppressed" this participant is the sender, so the receipt excludes it
   *   "consumed"   legacy only: the shared read moved the file out from under it
   *   null         nothing routed here
   */
  function cellFor(person, msg) {
    if (msg.to === "ember") return null;          // neither is affiliated to it

    if (state.mode === "participants") {
      if (msg.from === person.id) return "suppressed";
      return state.seen[person.id][msg.mid] ? "read" : "unread";
    }

    // Before participants: the sender is the room, so either agent looks
    // like a message from the room and is suppressed for both of them.
    if (msg.from === "astra" || msg.from === "fable") return "suppressed";
    if (!state.roomSeen[msg.mid]) return "unread";
    return state.roomSeen[msg.mid] === person.id ? "read" : "consumed";
  }

  function heldFor(msg) {
    if (msg.to !== "ember") return false;
    return true;   // no participant is affiliated to ember in this model
  }

  function unreadFor(person) {
    return state.messages.filter(function (m) { return cellFor(person, m) === "unread"; });
  }

  /* ------------------------------------------------------------- drawing */

  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  function use(symbolId) {
    var svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("class", "rail__glyph");
    svg.setAttribute("viewBox", "0 0 10 10");
    svg.setAttribute("aria-hidden", "true");
    var u = document.createElementNS("http://www.w3.org/2000/svg", "use");
    u.setAttribute("href", "#" + symbolId);
    svg.appendChild(u);
    return svg;
  }

  var STATE_WORD = {
    unread: "Unread",
    read: "Read",
    suppressed: "Self",
    consumed: "Gone"
  };
  var STATE_LONG = {
    unread: "unread",
    read: "read",
    suppressed: "suppressed, this participant is the sender",
    consumed: "gone: the shared read moved the file away"
  };

  function eventBox(msg, cellState, extraClass) {
    var b = el("div", "ev" + (extraClass ? " " + extraClass : ""));
    b.setAttribute("data-state", cellState);
    b.appendChild(el("span", "ev__tick"));
    b.appendChild(el("span", "ev__id", msg.mid.replace("m-", "")));
    b.appendChild(el("span", "ev__state", STATE_WORD[cellState] || "Held"));
    if (msg.mid === state.lastEvent) b.classList.add("is-arriving");
    return b;
  }

  function slotRow(container, render) {
    container.textContent = "";
    for (var i = 0; i < SLOTS; i++) {
      var slot = el("div", "slot");
      var msg = state.messages[i];
      if (msg) {
        var content = render(msg);
        if (content) { slot.setAttribute("data-has", "1"); slot.appendChild(content); }
      }
      container.appendChild(slot);
    }
  }

  function drawOrigin() {
    var track = document.getElementById("origin-track");
    slotRow(track, function (msg) {
      var b = el("div", "ev ev--origin");
      b.setAttribute("data-state", "origin");
      b.appendChild(el("span", "ev__id", msg.mid.replace("m-", "")));
      b.appendChild(el("span", "ev__state", "→" + msg.to));
      b.title = msg.mid + " from " + msg.from + " to " + msg.to;
      return b;
    });
    var note = document.getElementById("origin-note");
    note.textContent = state.messages.length + " of " + SLOTS + " slots used";
  }

  function drawRails() {
    var wrap = document.getElementById("rails");
    wrap.textContent = "";

    PEOPLE.forEach(function (p) {
      var rail = el("div", "rail" + (state.mode === "legacy" ? " rail--shared" : ""));
      rail.style.setProperty("--ch", "var(--ch-" + p.ch + ")");

      var idCol = el("div", "rail__id");
      var name = el("div", "rail__name");
      name.appendChild(use(p.glyph));
      name.appendChild(document.createTextNode(p.name));
      idCol.appendChild(name);

      // The label column carries the file that owns this rail's read state.
      // Before participants both rails name the same file. That is the bug.
      var path = el("span", "rail__pid",
        state.mode === "legacy" ? "tower/cursors.json" : "participants/" + p.pid + "/cursors.json");
      idCol.appendChild(path);

      var u = unreadFor(p).length;
      var r = state.messages.filter(function (m) { return cellFor(p, m) === "read"; }).length;
      var g = state.messages.filter(function (m) { return cellFor(p, m) === "consumed"; }).length;
      var counts = u + " unread · " + r + " read" + (g ? " · " + g + " gone" : "");
      idCol.appendChild(el("span", "rail__count", counts));

      var track = el("div", "rail__track");
      slotRow(track, function (msg) {
        var cs = cellFor(p, msg);
        if (!cs) return null;
        var box = eventBox(msg, cs);
        box.title = p.name + " — " + msg.mid + ": " + STATE_LONG[cs];
        return box;
      });

      rail.appendChild(idCol);
      rail.appendChild(track);
      wrap.appendChild(rail);
    });
  }

  function drawHeld() {
    var track = document.getElementById("held-track");
    var any = state.messages.some(heldFor);
    track.textContent = "";
    if (!any) {
      track.appendChild(el("span", "held__empty", "Empty. Every dispatched message found at least one eligible participant."));
      return;
    }
    slotRow(track, function (msg) {
      if (!heldFor(msg)) return null;
      var b = eventBox(msg, "held", "ev--held");
      b.querySelector(".ev__state").textContent = "Held";
      b.title = msg.mid + " is held: no participant is affiliated to " + msg.to;
      return b;
    });
  }

  /* ---------------------------------------------------------- the words --
   * Every state change is a sentence on the page, not only in a live region.
   * The reader should never have to infer the rule from an animation.        */

  function describe() {
    var r = state.readout;
    if (r) return r;
    if (state.mode === "legacy") {
      return "Before participants, Post kept one seen set for the whole room, in tower/cursors.json. " +
        "Both rails above name that same file. One message is dispatched and unread.";
    }
    return "Two participants are bound to the tower workspace address. One message " +
      "has been dispatched to it and neither has read it yet.";
  }

  function draw() {
    drawOrigin();
    drawRails();
    drawHeld();
    document.getElementById("readout").textContent = describe();
    syncButtons();
    state.lastEvent = null;
  }

  function announce(text) {
    state.readout = text;
    var live = document.getElementById("live");
    live.textContent = "";
    window.setTimeout(function () { live.textContent = text; }, 60);
  }

  /* ------------------------------------------------------------ controls */

  function syncButtons() {
    var full = state.messages.length >= SLOTS;

    document.querySelectorAll('[data-act="send"]').forEach(function (btn) {
      var to = btn.getAttribute("data-to");
      var why = null;
      if (full) why = "The field holds " + SLOTS + " messages. Reset it to send another.";
      else if (to === "ember" && state.mode === "legacy") why = "Lineage addresses did not exist before participants. Switch to the participant model to send one.";
      btn.disabled = !!why;
      btn.title = why || "";
    });

    PEOPLE.forEach(function (p) {
      var btn = document.querySelector('[data-act="read"][data-who="' + p.id + '"]');
      if (!btn) return;
      var n = unreadFor(p).length;
      btn.disabled = n === 0;
      btn.title = n === 0 ? "Nothing on " + p.name + "'s rail is unread." : "";
    });
  }

  function personById(id) {
    return PEOPLE.filter(function (p) { return p.id === id; })[0];
  }

  function send(from, to) {
    if (state.messages.length >= SLOTS) return;
    var mid = nextId();
    state.messages.push({ mid: mid, from: from, to: to });
    state.lastEvent = mid;

    var who = from === "trey" ? "Trey" : personById(from).name;
    var text;

    if (to === "ember") {
      text = who + " sent " + mid + " to the ember lineage address. No participant " +
        "is affiliated to ember, so nothing routed and " + mid + " is held. That is " +
        "pending delivery, not unread mail, and nobody's inbox grew.";
    } else if (from === "fable") {
      text = state.mode === "legacy"
        ? "Fable sent " + mid + " to tower. Before participants the sender was the room, and " +
          "Astra is also the room, so " + mid + " is suppressed as self on both rails. " +
          "Astra never learns it exists."
        : "Fable sent " + mid + " to tower. Fable's own notification is suppressed, " +
          "because the comparison is on the sending participant id. Astra is named " +
          "in the routing receipt, and the message is unread for Astra.";
    } else {
      text = state.mode === "legacy"
        ? who + " sent " + mid + " to tower. One copy arrived for the room, and both " +
          "rails are reading the same seen set."
        : who + " sent " + mid + " to tower. Its routing receipt names both bound " +
          "participants, and each reads against its own seen set.";
    }
    announce(text);
    draw();
  }

  function read(personId) {
    var p = personById(personId);
    var pending = unreadFor(p);
    if (!pending.length) return;
    var msg = pending[0];
    var other = PEOPLE.filter(function (q) { return q.id !== personId; })[0];

    var text;
    if (state.mode === "participants") {
      state.seen[personId][msg.mid] = true;
      var otherState = cellFor(other, msg);
      var clause =
        otherState === "unread"     ? msg.mid + " is still unread for " + other.name
      : otherState === "read"       ? other.name + " had already read " + msg.mid + " out of its own seen set"
      : otherState === "suppressed" ? other.name + " has no copy of " + msg.mid + ", because it sent it"
      :                               other.name + " has no copy of " + msg.mid;
      var tail = otherState === "read"
        ? ", and this read did not touch it."
        : ", and nothing opened " + other.name + "'s seen set.";
      text = p.name + " read " + msg.mid + ". One id went into participants/" + p.pid +
        "/cursors.json. " + clause + tail;
    } else {
      state.roomSeen[msg.mid] = personId;
      text = p.name + " read " + msg.mid + ". The room's shared cursor advanced and " +
        "exclusive_move took the file to tower/read/. " + other.name + "'s copy is not " +
        "unread. It is gone, because there was only ever one file.";
    }
    announce(text);
    draw();
  }

  function setMode(mode) {
    state.mode = mode;
    // Reading history is not portable between the two designs, so switching
    // re-settles the field rather than pretending one model's seen set means
    // something in the other.
    state.seen = { astra: {}, fable: {} };
    state.roomSeen = {};
    state.messages = state.messages.filter(function (m) {
      return !(mode === "legacy" && m.to === "ember");
    });
    state.readout = null;
    announce(mode === "legacy"
      ? "Switched to the pre-participant model. Read state is now one seen set for the whole " +
        "room, and both rails name tower/cursors.json. Any reads were cleared, because " +
        "one model's seen set does not mean anything in the other."
      : "Switched to the participant model. Each rail now names its own cursor file. " +
        "Any reads were cleared, because one model's seen set does not mean anything in the other.");
    draw();
  }

  /* ------------------------------------------------- the sibling diagram */

  function drawSiblings() {
    var wrap = document.getElementById("sib-rails");
    if (!wrap) return;
    var sibs = [
      { name: "Ember", ch: "c", glyph: "g-tri", pid: "demo-9d41c07b", cell: "unread" },
      { name: "Ember", ch: "c", glyph: "g-diamond", pid: "demo-31ba78e2", cell: "read" }
    ];
    sibs.forEach(function (s) {
      var rail = el("div", "rail");
      rail.style.setProperty("--ch", "var(--ch-" + s.ch + ")");
      var idCol = el("div", "rail__id");
      var name = el("div", "rail__name");
      name.appendChild(use(s.glyph));
      name.appendChild(document.createTextNode(s.name));
      idCol.appendChild(name);
      idCol.appendChild(el("span", "rail__pid", "participants/" + s.pid + "/cursors.json"));
      idCol.appendChild(el("span", "rail__count", s.cell === "read" ? "0 unread · 1 read" : "1 unread · 0 read"));

      var track = el("div", "rail__track");
      var slot = el("div", "slot");
      slot.setAttribute("data-has", "1");
      var b = el("div", "ev");
      b.setAttribute("data-state", s.cell);
      b.appendChild(el("span", "ev__tick"));
      b.appendChild(el("span", "ev__id", "9c02"));
      b.appendChild(el("span", "ev__state", STATE_WORD[s.cell]));
      slot.appendChild(b);
      track.appendChild(slot);
      for (var i = 1; i < 3; i++) track.appendChild(el("div", "slot"));

      rail.appendChild(idCol);
      rail.appendChild(track);
      wrap.appendChild(rail);
    });
  }

  /* --------------------------------------------------------- section marks
   * The same rail at its smallest scale: eight ticks, the current one filled.
   * It answers "where am I", which is why it is drawn at all.               */

  function drawMarks() {
    var NS = "http://www.w3.org/2000/svg";
    document.querySelectorAll(".sec-head__mark").forEach(function (svg) {
      var current = parseInt(svg.getAttribute("data-tick"), 10);
      var line = document.createElementNS(NS, "line");
      line.setAttribute("x1", "0"); line.setAttribute("x2", "44");
      line.setAttribute("y1", "6"); line.setAttribute("y2", "6");
      line.setAttribute("stroke", "currentColor");
      line.setAttribute("stroke-width", "1");
      line.setAttribute("opacity", "0.35");
      svg.appendChild(line);
      for (var i = 0; i < 8; i++) {
        var x = 2.5 + i * 5.6;
        var on = i + 1 === current;
        var t = document.createElementNS(NS, "rect");
        t.setAttribute("x", String(x - (on ? 1.6 : 0.5)));
        t.setAttribute("y", String(on ? 1.5 : 4));
        t.setAttribute("width", String(on ? 3.2 : 1));
        t.setAttribute("height", String(on ? 9 : 4));
        t.setAttribute("fill", "currentColor");
        t.setAttribute("opacity", on ? "1" : "0.45");
        svg.appendChild(t);
      }
      svg.style.color = "var(--ink-3)";
      svg.setAttribute("role", "img");
      svg.setAttribute("aria-hidden", "true");
    });
  }

  /* ------------------------------------------------------- reading rail -- */

  function spine() {
    var ticks = document.getElementById("spine-ticks");
    var cursor = document.getElementById("spine-cursor");
    if (!ticks || !cursor) return;
    var marks = Array.prototype.slice.call(document.querySelectorAll("section[id]"));

    function place() {
      var h = document.documentElement.scrollHeight || 1;
      ticks.textContent = "";
      marks.forEach(function (s) {
        var t = document.createElement("div");
        t.className = "rail-spine__tick";
        t.style.top = (s.offsetTop / h * 100) + "%";
        ticks.appendChild(t);
      });
    }
    function move() {
      var h = document.documentElement.scrollHeight - window.innerHeight;
      var p = h > 0 ? Math.min(1, Math.max(0, window.scrollY / h)) : 0;
      cursor.style.top = (p * 100) + "%";
    }
    place(); move();
    window.addEventListener("scroll", move, { passive: true });
    window.addEventListener("resize", function () { place(); move(); }, { passive: true });
  }

  /* ------------------------------------------------------------ receipts */

  /* One model, derived from POST_RECEIPTS and nothing else. Every surface
     that states acceptance status (the masthead stamp, the status line above
     the field, the record's stamp, count and caption) reads this object, so
     no sentence on the page can disagree with the rows. */
  function receiptModel(data) {
    var checks = (data && data.checks) || [];
    var counts = { pending: 0, pass: 0, fail: 0 };
    checks.forEach(function (c) {
      var st = c.status === "pass" || c.status === "fail" ? c.status : "pending";
      counts[st] += 1;
    });
    var total = checks.length;
    var overall = total === 0 ? "pending"
      : counts.fail ? "fail" : counts.pending ? "pending" : "pass";
    var run = (data && data.run) || {};
    var noun = function (n) { return n === 1 ? "check" : "checks"; };

    var stamp = overall === "pass" ? "Acceptance passed"
      : overall === "fail" ? "Acceptance failing" : "Acceptance pending";
    var shortStamp = overall === "pass" ? "Complete"
      : overall === "fail" ? "Failing" : "Pending";

    var tally = total === 0 ? "No checks recorded"
      : counts.pending === total ? "No run recorded · " + total + " " + noun(total) + " waiting"
      : counts.pass + " of " + total + " passing" +
        (counts.fail ? " · " + counts.fail + " failing" : "") +
        (counts.pending ? " · " + counts.pending + " pending" : "");

    var sentence;
    if (overall === "pass") {
      sentence = "All " + total + " acceptance " + noun(total) +
        " passed, with evidence recorded in each row of the record at the foot.";
    } else if (overall === "fail") {
      sentence = counts.fail + " of " + total + " acceptance " + noun(total) +
        (counts.fail === 1 ? " is" : " are") + " failing" +
        (counts.pending ? " and " + counts.pending + (counts.pending === 1 ? " is" : " are") + " still pending" : "") +
        ". The demonstration shows the agreed design, not a working install.";
    } else if (counts.pass) {
      sentence = counts.pass + " of " + total + " acceptance " + noun(total) +
        " passed; " + counts.pending + (counts.pending === 1 ? " is" : " are") +
        " still pending. Nothing here proves the rest.";
    } else {
      sentence = "This page is not evidence that it works. All " + total + " acceptance " +
        noun(total) + " for both machines are still pending.";
    }

    var caption = run.recordedAt
      ? "Recorded " + run.recordedAt + (run.headCommit ? " at " + String(run.headCommit).slice(0, 12) : "") + "."
      : null;

    return { overall: overall, counts: counts, total: total, stamp: stamp,
             shortStamp: shortStamp, tally: tally, sentence: sentence, caption: caption };
  }

  function stampEl(id, overall, text) {
    var e = document.getElementById(id);
    if (!e) return;
    e.className = "stamp stamp--" + overall;
    e.textContent = text;
  }

  function receipts() {
    var data = window.POST_RECEIPTS;
    var m = receiptModel(data);

    stampEl("masthead-stamp", m.overall, m.stamp);
    stampEl("overall-stamp", m.overall, m.shortStamp);

    var panel = document.getElementById("page-status");
    if (panel) panel.setAttribute("data-status", m.overall);
    var ptext = document.getElementById("page-status-text");
    if (ptext) ptext.textContent = m.sentence;
    var note = document.getElementById("overall-note");
    if (note) note.textContent = m.tally;
    if (m.caption) {
      var cap = document.getElementById("receipts-caption");
      if (cap) cap.textContent = m.caption;
    }

    var body = document.getElementById("receipts-body");
    if (!data || !body) return;
    body.textContent = "";

    (data.checks || []).forEach(function (c) {
      var status = c.status === "pass" || c.status === "fail" ? c.status : "pending";
      var tr = document.createElement("tr");
      tr.setAttribute("data-status", status);

      var th = document.createElement("td");
      th.appendChild(el("div", null, c.label));
      var d = el("div", "receipt__detail", c.detail);
      th.appendChild(d);

      var st = document.createElement("td");
      st.appendChild(el("span", "stamp stamp--" + status,
        status === "pass" ? "Pass" : status === "fail" ? "Fail" : "Pending"));

      var ev = document.createElement("td");
      if (c.evidence) ev.appendChild(el("code", "mono", c.evidence));
      else ev.appendChild(el("span", "receipt__none", "Nothing recorded yet."));

      tr.appendChild(th); tr.appendChild(st); tr.appendChild(ev);
      body.appendChild(tr);
    });
  }

  /* Exposed for fixture validation only; the page itself never calls it. */
  window.POST_RECEIPT_MODEL = receiptModel;

  /* ----------------------------------------------------------------- init */

  function init() {
    reset();
    drawMarks();
    drawSiblings();
    receipts();
    spine();

    document.querySelectorAll('input[name="mode"]').forEach(function (r) {
      r.addEventListener("change", function () { if (r.checked) setMode(r.value); });
    });

    document.addEventListener("click", function (e) {
      var btn = e.target.closest ? e.target.closest("[data-act]") : null;
      if (!btn || btn.disabled) return;
      var act = btn.getAttribute("data-act");
      if (act === "send") send(btn.getAttribute("data-from"), btn.getAttribute("data-to"));
      else if (act === "read") read(btn.getAttribute("data-who"));
      else if (act === "reset") {
        var mode = state.mode;
        reset();
        state.mode = mode;
        announce("The field is back to its settled state: one message dispatched to tower, unread on both rails.");
        draw();
      }
    });

    draw();
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
