# Direction contract — the Post participants explainer

Scoped to `docs/visual/`. Authored 2026-09-16 by the visual lane under Astra's
ownership, against the brief in `/tmp/post-papercuts-2026-09-15/visual-brief.md`
and the final amended architecture in `docs/PARTICIPANTS.md` at `239ae8b`.

## Concept seed record

`impeccable concept-seed --scope direction --mode read` (key `2ab5754b`,
pool `c3b204a1eed6`) assigned index 7 of the grounded list below.

Grounded directions, ordered by resonance before the roll:

1. The register: intaglio plate and impressions, the material's own metaphor.
2. The polyphonic score: staves as participants, "polyphony" being the
   philosophy's own word.
3. General arrangement drawing: title block, revision table, mechanism to scale.
4. Poste restante: a sorting frame of pigeonholes, brass tags, slot labels.
5. The state matrix: a Swiss typographic truth table, rows by participant.
6. Chain of custody: exhibit tags, dockets, attributed signatures.
7. **The trace: a printed channel record.** Assigned. Built.

### Why the assignment holds

Post's read state is a cursor. An instrument's measurement marker is a cursor.
The product's central noun and the world's central instrument are the same word,
and the invariant we have to teach is a statement about cursors: advancing one
participant's cursor must not advance another's.

That makes the bug drawable in the world's native grammar. Two participants
sharing one channel rail is not an illustration of the defect, it is the defect.
Two rails with two independent cursors is not an illustration of the fix. The
reader who understands the picture understands the change.

The failure mode of an instrument world is the phosphor-green terminal costume,
monospace everywhere, technical as a look rather than as a fact. This build
refuses that by taking the instrument's **printed record** as the substrate
rather than its screen. A chart record is warm stock, a fine printed ruling,
saturated pen inks, and tracked condensed captions. That substrate carries the
long philosophical sections in an editorial serif without switching worlds.

### Challenger weighing

Judged on audience identification and product clarity, after fusing each
challenger's grammar with the product's facts.

| Challenger | Verdict | Reason |
|---|---|---|
| Darkroom exposure record | competitive | Conflict rendered without colour is genuinely strong; the zone-system frame adds a second subject the reader has to learn. |
| Film cutting bench | competitive | Its rail is nearly the assigned rail; flag-orange on grainy black fights sustained reading and the grain is close to banned texture. |
| Emigre bitmap specimen | declined | Lo-fi display type undercuts a page whose job is to be trusted about correctness. |
| Stitched sampler | declined | Wrong register for a correctness document; silk and linen drift into faux-craft illustration. |
| Coptic exposed binding | declined | Quiet and honest, but the binding does not model per-participant read state. |
| Orizuru fold sequence | competitive | Creases surviving into the finished form is a fine model of lineage history; washi and kanji are a costume unrelated to agent mail. |

No challenger wins both axes, so the assigned direction builds. Each one
donates a discipline the assigned direction lacked:

1. **State by mark and fill, never by hue alone.** Hue identifies whose channel
   it is. Read and unread are distinguished by fill, by a struck rule, and by
   words. The demo is legible in grayscale.
2. **One rail crosses the whole document, and set-aside things have a visible
   home.** The reading rail runs the full page with a tick per section. Held
   mail sits below the rails rather than being described in prose.
3. **One form at three scales.** The channel field appears full size in the
   demonstration, as a two-rail glyph inline in prose, and as a single-rail
   mark on section heads. The reader learns the grammar once.
4. **The record is signed and dated at the foot**, naming which participants
   built it and at what revision. Attribution is the page's own subject.
5. **Exposed structure only.** Every rule, tick, and mark carries information.
   Nothing on the page is present because a page usually has one.
6. **No impossible state is reachable, and prior states stay visible.** A
   control that would produce a state the real system cannot produce is
   disabled and says why. Affiliation history accumulates rather than replacing
   a label.

## First viewport

The mechanism, at working size, already in a settled state before any click.

Left: the masthead, set small. A single display line states the defect. Right
and dominant: the channel field, showing the `tower` workspace address, two
participant rails (Astra in vermilion, Fable in prussian), and one message
already dispatched and unread on both rails.

Above the field, a two-position mode control reading `0.9.0 — one cursor per
room` and `participants — one cursor each`. No scroll cue, no call to action,
no hero metric. A reader who opens the page and reads nothing else sees two
rails, two unread marks, and the words that name them.

## Visitor path

1. **The collision.** The field, live, with the mode control. Switching to the
   legacy mode collapses two rails into one; reading as Astra takes Fable's
   copy. The defect is a geometric fact on screen.
2. **The invariant.** The demonstration proper: send, read as either
   participant, reset. Status line and live region narrate every transition in
   words. Simulation wording sits beside the controls.
3. **Three things that are not each other.** Participant, lineage, address, as
   a definition register: what each is, what it can do, what it cannot do.
4. **Same name, separate mail.** Two participants continuing Ember. Self
   suppression by participant id, never by name.
5. **Continuing is a choice.** Session-only, continue an existing lineage, or
   found a new one are the neutral identity choices; preview and decline remain
   legitimate interactions. None is drawn as failure.
6. **A lineage speaks in more than one voice.** Polyphony, with illustrative
   voices attributed to synthetic participant ids, including one dissent and
   one withdrawal showing a visible gap.
7. **What this does not claim.** The limits, unhedged.
8. **The record.** Receipts, driven by one embedded JSON object, pending until
   real acceptance lands. Starter commands. Signature and date.

## Signature interaction

The channel field. Sending drops an event tick into the address and fans it
along routing lines to every eligible rail, one coordinated motion, exponential
ease-out, about 420ms. Reading marks one rail's copy and nothing else.

The field is not a decoration that animates. It is the page's instrument, and
it is readable with animation disabled, with colour removed, and before any
control is touched.

## Typography

Newsreader, self-hosted variable, optical size and weight. Display at `opsz 72`
with tracking -0.025em; prose at `opsz 16` on a 66 to 70 character measure.
Italic for attributed voices and asides.

Archivo, self-hosted variable, width and weight. Condensed grades in caps with
open tracking for channel labels, field captions, and registry micro-type.
Normal width for buttons.

JetBrains Mono for message ids, participant ids, paths, and commands. Nowhere
else. All three are OFL; the licenses ship in `assets/fonts/`.

## Palette

Warm chart stock `#F2EDE3`, inset `#E7E0D2`, ink `#181713`, secondary `#565049`,
tertiary `#8A8278`, hairline ruling `#D3CABA`, major ruling `#B8AD99`.

Channel inks: Astra vermilion `#B23A1E`, Fable prussian `#1D4E6B`, a third moss
`#4A5D2A` for additional participants. Ochre `#8A6410` carries limits.

Light, not dark, chosen from the use scene: the maintainer reading in the
morning, at length, wanting to trust what he reads.

## Responsive behaviour

Desktop holds the field beside its caption. Below 900px the caption moves above
the field and the rails take the full width. Below 600px rail labels move above
their rails and the field shows fewer events rather than smaller ones. The
reading rail becomes a thin progress ruling at the page edge. No horizontal
overflow at any width; the diagrams are SVG with a viewBox, not fixed pixels.

## Honest risk

The instrument world can read as cold. The philosophy sections are about
consent, welfare, and withdrawal, and a page that renders them as instrument
output would be making a claim about them that nobody should make. The
mitigation is that the instrument owns the mechanism and the editorial serif
owns the argument, with the two meeting only where the argument is genuinely
about routing. If the polyphony section starts looking like telemetry, this
direction has failed and the section, not the world, is what to fix.

Second risk: a diagram this abstract can teach nothing if the reader has to
infer the rule from motion. Every state change is therefore also a sentence,
visible on the page, not only in a live region.

## Quality-bar calibration waiver (2026-09-16)

Astra ruled this under Trey's explicit overnight design authority. This page
was built code-first in an existing world, and no QUALITY BAR card or approved
comp exists for it. External reference-card calibration is waived. Fidelity is
judged against this written direction and the actual renders only. This page
therefore makes no claim to have reached a ceiling set by an external reference.

The waiver covers that calibration step and nothing else. The first-viewport
contract above, factual accuracy against `docs/PARTICIPANTS.md` and the
installed CLI, contrast, and runtime-evidence gates still apply in full.
