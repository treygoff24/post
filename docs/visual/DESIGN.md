---
name: Post Participants Explainer
description: A printed channel record that makes per-participant read state visible.
colors:
  chart-stock: "#f2ede3"
  inset-stock: "#e8e1d3"
  deep-stock: "#ded5c3"
  record-ink: "#181713"
  secondary-ink: "#565049"
  tertiary-ink: "#645d4f"
  hairline-rule: "#d3cabb"
  major-rule: "#b6ab96"
  astra-vermilion: "#b03a1e"
  astra-wash: "#f6e4dd"
  fable-prussian: "#1c4d6a"
  fable-wash: "#dde8ef"
  lineage-moss: "#47591f"
  lineage-wash: "#e6ead7"
  evidence-ochre: "#7a5709"
  evidence-wash: "#f4e9cf"
typography:
  display:
    fontFamily: "Newsreader, Iowan Old Style, Georgia, serif"
    fontSize: "clamp(1.625rem, 1.2rem + 1.5vw, 2.625rem)"
    fontWeight: 400
    lineHeight: 1.02
    letterSpacing: "-0.024em"
    fontVariation: "'opsz' 60, 'wght' 400"
  headline:
    fontFamily: "Newsreader, Iowan Old Style, Georgia, serif"
    fontSize: "clamp(2rem, 1.3rem + 2.8vw, 3.25rem)"
    fontWeight: 450
    lineHeight: 1.04
    letterSpacing: "-0.025em"
    fontVariation: "'opsz' 48, 'wght' 450"
  title:
    fontFamily: "Newsreader, Iowan Old Style, Georgia, serif"
    fontSize: "clamp(1.3rem, 1.1rem + 0.8vw, 1.65rem)"
    fontWeight: 560
    lineHeight: 1.18
    letterSpacing: "-0.015em"
    fontVariation: "'opsz' 28, 'wght' 560"
  body:
    fontFamily: "Newsreader, Iowan Old Style, Georgia, serif"
    fontSize: "clamp(1.0625rem, 0.95rem + 0.4vw, 1.1875rem)"
    fontWeight: 400
    lineHeight: 1.62
    fontFeature: "'kern', 'liga', 'calt'"
    fontVariation: "'opsz' 16, 'wght' 400"
  label:
    fontFamily: "Archivo, Helvetica Neue, Arial, sans-serif"
    fontSize: "0.6875rem"
    fontWeight: 620
    lineHeight: 1.2
    letterSpacing: "0.13em"
    fontVariation: "'wdth' 86, 'wght' 620"
  mono:
    fontFamily: "JetBrains Mono, ui-monospace, SF Mono, Menlo, monospace"
    fontSize: "0.86em"
    fontWeight: 450
    letterSpacing: "-0.01em"
    fontFeature: "'kern', 'calt'"
    fontVariation: "'wght' 450"
spacing:
  page-gutter: "clamp(1.25rem, 4vw, 4rem)"
  section-band: "clamp(3.5rem, 9vh, 6.5rem)"
  field-padding: "1rem"
  control-x: "0.8rem"
  control-y: "0.45rem"
components:
  button-neutral:
    backgroundColor: "{colors.chart-stock}"
    textColor: "{colors.record-ink}"
    typography: "{typography.label}"
    padding: "{spacing.control-y} {spacing.control-x}"
  button-astra:
    backgroundColor: "{colors.chart-stock}"
    textColor: "{colors.astra-vermilion}"
    typography: "{typography.label}"
    padding: "{spacing.control-y} {spacing.control-x}"
  button-fable:
    backgroundColor: "{colors.chart-stock}"
    textColor: "{colors.fable-prussian}"
    typography: "{typography.label}"
    padding: "{spacing.control-y} {spacing.control-x}"
  mode-selected:
    backgroundColor: "{colors.record-ink}"
    textColor: "{colors.chart-stock}"
    typography: "{typography.label}"
    padding: "{spacing.control-y} {spacing.control-x}"
  field:
    backgroundColor: "{colors.inset-stock}"
    textColor: "{colors.record-ink}"
  evidence-pending:
    backgroundColor: "{colors.evidence-wash}"
    textColor: "{colors.evidence-ochre}"
    typography: "{typography.label}"
---

# Design System: Post Participants Explainer

## Overview

**Creative North Star: "The Trace: a printed channel record"**

The explainer is a warm, exacting instrument record rather than a terminal costume. A ruled channel field owns the mechanism; editorial serif text owns the argument. Thin rules, event ticks, rail marks, condensed captions, and explicit state words make routing and read state inspectable without turning the page into a dashboard.

The visual density is deliberate but orderly. Chart-stock surfaces and saturated channel inks keep the mechanism legible, while long passages retain a comfortable editorial measure. Every built mark has a job: location, attribution, state, boundary, or evidence. At documentation time (2026-09-16, before acceptance), every receipt row was pending. Live acceptance status belongs solely to `assets/receipts.js`; this design record does not imply an installed or working runtime.

**Key Characteristics:**
- Warm printed stock with dark record ink and fine functional rulings.
- One channel grammar at three scales: working field, inline glyph, and section mark.
- Editorial Newsreader prose paired with condensed Archivo captions and narrowly scoped JetBrains Mono data.
- State communicated by fill, strike, border pattern, words, and hue together.
- Flat, exposed structure with no decorative shadows or ornamental cards.

## Colors

Warm paper neutrals carry the document; saturated inks identify participants and evidence without carrying state by themselves.

### Primary
- **Astra Vermilion:** Identifies Astra rails, marks, and participant actions; its pale wash supports failure-state surfaces.
- **Fable Prussian:** Identifies Fable rails, marks, and participant actions; its pale wash is reserved for related supporting surfaces.

### Secondary
- **Evidence Ochre:** Marks pending acceptance, held delivery, consumed legacy copies, and cautionary notices against its evidence wash.

### Tertiary
- **Lineage Moss:** Identifies the additional-participant and lineage voice, and supplies the passing evidence state.

### Neutral
- **Chart Stock:** The page and control ground used for sustained reading.
- **Inset Stock:** The channel field, alternating bands, and withdrawn-voice surfaces.
- **Deep Stock:** The strongest paper layer, used for simulation notes and unselected mode hover.
- **Record Ink:** Primary text, major borders, selected controls, and unread legend marks.
- **Secondary and Tertiary Ink:** Supporting prose, metadata, paths, and low-emphasis labels.
- **Hairline and Major Rules:** Section rhythm, tracks, dividers, and stronger internal boundaries.

### Named Rules
**The Redundant State Rule.** Hue identifies a participant; unread, read, self, gone, and held are also distinguished by fill, strike, border treatment, and words.

**The Evidence Color Rule.** Ochre describes pending or constrained states; it is not a success color. Moss is the built passing state, and vermilion is the built failing state.

## Typography

**Display Font:** Newsreader (with Iowan Old Style, Georgia, and serif fallbacks)  
**Body Font:** Newsreader (with Iowan Old Style, Georgia, and serif fallbacks)  
**Label/Mono Fonts:** Archivo for captions and controls; JetBrains Mono for ids, paths, commands, and machine data.

**Character:** Newsreader makes the explanation feel considered and readable at length. Archivo supplies the printed-record caption voice, while JetBrains Mono appears only where fixed-form technical data improves attribution or verification.

### Hierarchy
- **Display** (weight 400, responsive 1.625rem-2.625rem, line-height 1.02): The opening defect statement, balanced on a narrow measure; the desktop grid overrides it to 2.25rem-2.875rem.
- **Headline** (weight 450, responsive 2rem-3.25rem, line-height 1.04): Major section statements with optical size 48.
- **Title** (weight 560, responsive 1.3rem-1.65rem, line-height 1.18): Mechanism and register subsections with optical size 28.
- **Body** (weight 400, responsive 1.0625rem-1.1875rem, line-height 1.62): Sustained explanation on a 66ch maximum measure with optical size 16.
- **Label** (weight 620, 0.6875rem, 0.13em tracking, uppercase): Channel captions, metadata, registry micro-type, and control context, usually at condensed width 86.
- **Mono** (weight 450, 0.86em): Message ids, participant ids, paths, commands, and receipt evidence only.

### Named Rules
**The Three-Voice Rule.** Serif explains, condensed sans labels, and mono identifies machine data; do not let mono become the page's atmosphere.

## Layout

The page uses a centered 78rem shell with a fluid 1.25rem-4rem gutter. Section bands use 3.5rem-6.5rem vertical padding, and prose is organized by ruled heads rather than floating containers. On wide screens, the opening uses a narrow narrative column beside the dominant working field; other content becomes two- or three-column grids only when the content can hold its measure.

At 64rem and above, the opening becomes a two-column grid. Content grids resolve at 58rem, voice and limit rows at 52rem, and the five-choice strip at 72rem. The full-height reading rail appears only above 1180px. The acceptance table keeps its semantics and becomes a keyboard-focusable horizontal scroll region below its intrinsic width.

The channel field responds to its own width. Below 44rem it stacks the mode control across the field; below 34rem rail labels move above their tracks, routing fan lines disappear, and event marks retain useful size while fewer slots remain visible. At viewport widths below 40rem, control row labels take the full line and secondary masthead metadata is reduced. No layout depends on horizontal page overflow.

## Elevation & Depth

The system is flat: it uses no ambient or structural shadows. Depth comes from three paper tones, one-pixel and two-pixel rules, inset bands, and solid versus hollow state marks. The only `box-shadow` is a high-contrast keyboard focus indicator, not elevation.

### Named Rules
**The Flat Record Rule.** Separate levels with stock tone and rulings; do not lift panels with decorative shadows.

## Shapes

The form language is rectilinear and instrument-like. Fields, buttons, mode segments, notices, stamps, event marks, and register blocks use square corners and explicit borders. Hairlines organize the page; two-pixel rules declare stronger section or rail boundaries; dashed and dotted borders carry actual state. The only rounded built shape is the browser scrollbar thumb, while focus treatment uses a minimal 2px radius.

## Components

### Buttons
- **Shape:** Square, one-pixel outlined controls with compact 0.45rem by 0.8rem padding.
- **Neutral:** Chart stock with record ink; hover reverses to a record-ink background with chart-stock text.
- **Participant variants:** Transparent chart-stock controls use the participant ink for border and text, then fill with that ink on hover.
- **Quiet:** Major-rule border and secondary ink for reset or lower-emphasis actions.
- **Hover / Focus / Active:** Color transitions run for 120ms; active controls move down 1px; keyboard focus uses the two-ring focus treatment. Disabled controls are 40% opaque, dashed, and retain a textual reason in the native title.

### Mode Control
- **Style:** A two-segment outlined fieldset. The selected segment uses a record-ink background with chart-stock text; the unselected segment stays on chart stock and gains deep stock on hover.
- **Behavior:** Native radio inputs remain the source of truth beneath the visible labels, with an inset focus ring. Below a 44rem field width, both options divide the full row and wrap their labels.

### Channel Field and Event Marks
- **Container:** A square-cornered inset-stock field with a record-ink frame, chart-stock header, ruled plot, legend, reading sentence, controls, and simulation note.
- **Unread:** Solid participant ink with a raised tick and light id/state text.
- **Read:** Hollow chart stock with a participant-ink strike.
- **Self:** Dotted tertiary-ink outline on opaque inset stock.
- **Gone:** Dashed ochre outline crossed twice; this state exists only in the legacy model.
- **Held:** Solid ochre outline on evidence wash, placed below the participant rails.
- **Motion:** Newly dispatched marks settle downward over 420ms with exponential ease-out. Reduced-motion preference collapses animations and transitions to 1ms without hiding the resting state.

### Registers, Voices, and Choices
- **Registers:** Ruled definition columns, not cards; a two-pixel top rule introduces the term and hairlines separate capabilities from constraints.
- **Voices:** Attributed entries use a metadata column and an editorial text column. Dissent is italic; withdrawal remains as a dashed, inset-stock gap rather than disappearing.
- **Choices:** A one-pixel shared grid holds five equal paper cells at the widest layout, two columns at medium width, and one column on narrow screens.

### Evidence Record
- **Status:** Pending uses ochre, pass uses moss, and fail uses vermilion. The header, opening panel, table tally, caption, and rows derive from the same receipt model.
- **Authoring snapshot:** On 2026-09-16 before acceptance, all 11 checks were pending, with no run timestamp, commit, or evidence recorded. Consult `assets/receipts.js` for live status; the page explicitly says pending evidence is not proof that the system works.
- **Table:** A ruled, semantic three-column table with condensed uppercase headers and a focusable overflow wrapper on narrow screens.

## Do's and Don'ts

### Do:
- **Do** preserve redundant state encoding: words and marks must remain meaningful without hue or motion.
- **Do** keep acceptance language derived from `assets/receipts.js`, and leave a row pending until a real observed check supplies evidence.
- **Do** retain full-size event marks on narrow fields and reduce visible slots instead of shrinking the marks.
- **Do** use the participant inks for attribution and action ownership, not as the only signal of read state.
- **Do** keep every rule, tick, and gap tied to information or navigation.

### Don't:
- **Don't** claim installation, runtime acceptance, or passing evidence while the receipt rows remain pending.
- **Don't** turn the printed record into a terminal theme or spread monospace beyond ids, paths, commands, and evidence.
- **Don't** introduce decorative shadows, rounded cards, texture, or uninformative rules.
- **Don't** infer the mechanism from animation alone; every state transition also needs visible words.
- **Don't** collapse participant, lineage, and address into one visual or conceptual unit.
