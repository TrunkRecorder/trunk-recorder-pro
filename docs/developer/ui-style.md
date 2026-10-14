# Interface style

How the browser interface (`web/`) looks and is organized, so new screens fit
in. It covers the design tokens, type, layout patterns, components and the
way the interface talks. The styles are in `web/src/styles.css`; the setup
page's building blocks are in `web/src/SetupLayout.tsx`; the dashboard's are
in `web/src/charts.tsx`.

The interface has three kinds of screen, each with its own density:

| Kind | Where | What it's for | Density |
| --- | --- | --- | --- |
| **Dashboard** | Overview, Listen, RF, Decode, Radio system, Live, Plugins, Platform | Watching the recorder: figures, charts, status | Dense; figures are the largest type |
| **Setup** | Setup (its tabs) | Changing settings | Roomy; headings and labels lead |
| **Guide** | The setup guide (`Onboarding.tsx`, `.ob-*`) | One question at a time, for someone new | Large type, line art, one column |

Pick the one a new screen is closest to and follow its rules; don't mix them
on one page.

## Principles

1. **Group by what someone is trying to do, not by how the config is
   stored.** A system's "On the air" group holds its control channels and
   its band plan because both answer "where is it?", though they are
   different keys.
2. **Every level of the hierarchy looks different.** A reader should be able
   to tell a section from a card from a group from a field by size and
   weight alone, without reading.
3. **Say what a thing is for, once, where it is.** A section and a group
   each get one line on their purpose. A field's hint says what to enter or
   what changes; it doesn't repeat the label.
4. **Status is shown, not hidden.** A folded card still shows whether it's
   recording and whether it's covered. A problem says why, next to where to
   fix it.
5. **Nothing moves under the pointer.** Folding, adding and removing keep
   the other cards as they were.

## Tokens

All colours are CSS custom properties on `:root`, with a light set under
`@media (prefers-color-scheme: light)`. Use the tokens, never literal
colours, so both themes keep working.

| Token | Use |
| --- | --- |
| `--bg` | The page; inputs inside cards (a field is a well, darker than its card) |
| `--panel` | Cards and panels |
| `--panel-2` | Raised things inside a panel: a hovered row, a dashboard tile's well |
| `--line` | Every border and rule |
| `--text` | Text |
| `--muted` | Hints, secondary text, inactive tabs, icons at rest |
| `--accent` | The one action colour: the selected tab, the primary button, focus rings, links, a switch that's on |
| `--ok` / `--warn` / `--bad` | Status only (lights, chips, banners, `.why`), never decoration |
| `--rec` | Recording, live audio |
| `--sys-0` … `--sys-5` | A system's own colour (`systemColor()`), used the same everywhere it appears |
| `--series-*`, `--seq-*`, `--hot-*` | Chart marks only; see the comment above them in `styles.css` |

Tints are made with `color-mix(in srgb, <token> N%, transparent)`, typically
13–16 % for a chip or pill background, so they follow the theme.

Radii: 6–8 px for inputs and small buttons, 10 px for wells and details
boxes, 12–14 px for cards, 999 px for pills and chips. Fonts: the system UI
font; `--mono` for frequencies, IDs, paths, codes and anything typed that
must be read exactly.

## Type

### Setup

Set on `.setup` as custom properties. Each level is a step down in **both**
size and weight, so the order holds even at a glance:

| Level | Token | Size / weight | Colour | Element |
| --- | --- | --- | --- | --- |
| Page | (topbar) | 22 px / 600 | `--text` | `h1` in the top bar |
| Section | `--fs-section` | 22 px / 650, −0.015em | `--text` | `.set-section-head h2` |
| Card | `--fs-card` | 17 px / 650 | `--text` | `.set-card-title h3`, `.panel-head h2` |
| Group | `--fs-group` | 14.5 px / 650 | `--text` | `.set-group-head h4` |
| Field label | `--fs-label` | 13 px / 550 | `--text-2` (text softened toward muted) | `.field-label` |
| Body, inputs | — | 14 px / 400 | `--text` | |
| Hint | `--fs-hint` | 12.5 px / 400 | `--muted` | `.field-hint`, group and card sub-lines |

A section's description is 14 px muted, and a card's sub-line is 13 px muted.

Rules:

- Headings are sentence case ("Talkgroups and units"), never all capitals.
- A field label is never larger or bolder than its group's heading.
- Don't add a size between these steps. If something needs to stand out,
  it's probably a level of its own (a group, a card) or a chip.

### Dashboard

Numbers lead. `--fs-hero` (2.6 rem) and `--fs-kpi` (1.65 rem) are for
figures; a card's title is 15 px / 600 (`.card-title h3`); a section's
heading is the small, capitalized, muted eyebrow (`h2.section-title`,
13 px), because there the figures, not the headings, are what's read.
`h2.big` (26 px) is for the page's one message when there's nothing to
show ("Not recording").

### Guide

17 px body, 28–38 px titles (`.ob-title`), 19 px lead, 15 px hints. It's
read once, slowly, by someone new.

## Setup's layout: section → card → group → field

```
Section   "Trunked systems"   P25, SmartNet … control channels.   [Expand all] [Add a system]
┌ Card ─────────────────────────────────────────────────────────────────────────┐
│ ›  ● wmata   SmartNet / SmartZone   [control channel on source 1, 2]   ◉ Record  Remove │
├────────────────────────────────────────────────────────────────────────────────┤
│ Basics                 │ Short name            Name                            │
│ The short name is …    │ [wmata          ]     [                ]              │
│                        │ Type                  Modulation                      │
├────────────────────────┼───────────────────────────────────────────────────────┤
│ On the air             │ Control channels, MHz                                 │
│ Where the recorder …   │ [496.43750, 490.86250 …                             ] │
└────────────────────────┴───────────────────────────────────────────────────────┘
```

### Section (`<Section>`)

A heading for one kind of thing on a tab, a one-line description, and the
actions for the whole list on the right: the primary one (Add …) last, as a
plain `.btn`; others (`Import …`, `Expand all`) as `.btn.ghost` before it.
The section's cards follow, 16 px apart. Sections are 36 px apart.

An empty list shows `.set-empty`: a dashed box saying what's missing and
what to do ("No trunked systems yet. Add one, import a Trunk Recorder
config, or find one on the air below.").

### Card (`<Card>`, or a plain `section.set-card`)

One thing: a system, a radio, the log. Its head has:

- the name (`h3`) and, under it, a one-line summary in `.set-card-sub`
  (type, frequency, rate, "3 channels, 2 on"): enough to recognize it
  folded;
- status chips: what's right or wrong with it now ("control channel on
  source 1, 2", "voice 14/14 covered", "2 to finish");
- on the right, its controls: the on/off `<Switch>` ("Record"), then
  `Remove` as a small ghost button. Remove always confirms when it loses
  data, and says what stays ("Its recordings stay on disk").

A system's card has a 4 px left border in its system colour.
`className="off"` (not recording) dims the body but not the head, so the
switch stays clear.

**Folding.** Cards that repeat in a list (systems, conventional systems,
radios) use `<Card cardKey n>`, which folds to its head:

- The whole name area is the toggle button (a chevron that turns), with
  `aria-expanded`. The switch and buttons sit outside it.
- Open state is remembered per card in `localStorage` (`trp.setupOpen`,
  keys like `sys-0`).
- With one card in a list it starts open; with several, folded. Adding one
  calls `addCard()` so the new card opens and the others keep how they
  look; removing calls `forgetCard()` so the ones after it keep their
  state.
- A list of two or more gets `<ExpandAll>` in its section's actions.
- A folded card keeps its body rendered (`hidden`), so `showTodo()` can
  find a field inside, open the card (`data-card`, `data-collapsed`) and
  scroll to it. Anything that links to a setup field should go through
  `showTodo()`.

A card that's alone on its tab and short (Log, M4A, Interfaces) doesn't
fold: use a plain `section.set-card` with a `.set-card-head`.

### Group (`<Group>`)

Settings that answer one question, under a short noun-phrase title
("Storage", "Gain", "Where it goes") and an optional one-sentence hint. On a
wide card the title and hint take a 200 px column on the left and the
fields fill the right; below about 760 px of card width (a container
query) they stack. Groups in a card are separated by a 1 px rule.

- `cols={2}` (the default) or `cols={3}` lays fields in columns; `cols={1}`
  when each field needs the width (a list of plugin settings, an override
  panel).
- Inside a group, checkboxes (`<Toggle>`), `.wide` fields, `details.help`,
  banners and paragraphs always take a whole row. So: short inputs side by
  side, and toggles as a list beneath them.
- `full` puts the title above and lets the body use the card's whole width,
  for a table (a conventional system's channels). Its `actions` (Import,
  Export, Add a channel) go on the right of the title.
- 2–6 fields make a good group. More than that is usually two groups; one
  field on its own usually belongs in a neighbour (unless it's the only
  thing of its kind, like "Units").
- Order groups from what everyone sets to what few touch: Basics first,
  Overrides or Advanced last.

Group names used so far, to reuse where they fit: **Basics** (names, type),
**On the air** (where to find it), **Talkgroups and units**, **Sites**,
**Overrides** (this one's own versions of defaults set elsewhere),
**Device**, **Tuning**, **Gain**, **Frequency correction**, **Storage**,
**Recording**, **Startup and diagnostics**, **Where it goes**, **What it
says**.

### Field (`<Field>`)

A label, the input, and an optional hint below.

- **Label:** the thing, in sentence case, with its unit after a comma:
  "Gain, dB", "Center frequency, MHz", "Pre-roll, s". No colon.
- **Hint:** short, no closing full stop, starts with a capital. Says the
  range, the default, the consequence or an example ("Most dongles:
  0–49.6", "Later ones are fallbacks", "e.g. RX2, TX/RX"). A hint that
  isn't needed is left out. When a field is overridden, the hint says the
  default ("Default: 3").
- **Placeholder:** what happens when empty ("any", "auto", "from the
  air", "Default (keep all)"), or an example ("154.430, 155.100").
- Frequencies, IDs, paths and codes use `.mono`.
- `needs` / `anchor` mark a field an import left to fill in (`.needs`, a
  warning ring); the to-do list's Show scrolls to it.

### Toggle (`<Toggle>`)

A checkbox with the label, and the hint on its own line beneath (first
letter capitalized by CSS). Use it for settings. Use the `<Switch>` (a
sliding switch) only for the one on/off that decides whether a whole card
does anything (Record, a plugin's On).

When a toggle can't do anything with the current settings, disable it and
say why in its hint ("Both are kept anyway").

### Disclosure (`details.help`)

For what most people never open: help text ("CSV format", "Dongle not
showing up?"), and advanced or override panels. The summary says what's
inside and its state: "Site lock — off", "Recording override — 4
changed".

## Dashboard patterns

- **Page:** `.page`, a column 20 px apart; sections headed by
  `h2.section-title` over a `.card-grid` (cards at least 320 px wide).
- **Card** (`Card` in `charts.tsx`): a title (a link to its page when there
  is one), an optional status light (`level`: ok / warn / bad / idle) and,
  when not ok, a `.why` line saying why, in words. `detail` adds "More ▾".
- **Figures** (`Stat`): an uppercase label, the value large, the unit
  small and muted beside it, then a sub-line or sparkline. A warning shows
  as a coloured inset line on top of the tile, not a coloured value.
- **Status lights** (`Light`) are the same in the side rail, on cards and
  in rows: green ok, amber warn, red bad (pulsing), grey idle. A light
  always has a title saying why.
- Chart rules (colour order, ramps, empty states) are in the comment above
  the `--series-*` tokens in `styles.css`.

## Shared components

| Component | Class | Notes |
| --- | --- | --- |
| Button | `.btn` | `.primary` for the one main action on the screen (Start); `.ghost` for secondary; `.small` in rows and card heads; `.danger` text for destructive |
| Chip | `.chip` | A short status or fact; `.ok` / `.warn` / `.bad` tint it. Lowercase text ("control channel on source 3") |
| Pill | `.pill` | The recorder's state in the top bar |
| Banner | `.banner` | A message across a page or card; `.bad` for errors; `.subtle` for a reminder. Has its action on the right ("Open setup", "Dismiss") |
| Segmented choice | `.seg` | A choice among 2–5 that changes what else is shown (a radio's kind); the chosen one filled with `--accent` |
| Tabs | `.setup-tabs` | Text and icon on a rule, the current one underlined in `--accent`; a count and a to-do badge after the name |
| Empty state | `.set-empty`, `.empty`, `.chart-empty` | Says what's missing and what to do |

## Writing

- Plain words, sentence case, short. Say what something does for the
  person, not how it's implemented ("Keep calls waiting to upload in
  memory", not "Enable tmpfs spool").
- Trunk Recorder's own names for settings are kept where people will look
  for them, or given in a hint, so a config can be brought over.
- Name buttons with a verb and object: "Add a system", "Import CSV…",
  "Use this file". An ellipsis means a dialog or picker follows.
- Numbers keep their unit; ranges use an en dash ("0–49.6").
- Errors and warnings say what's wrong and what to do, where it can be
  done.

## Responsiveness and accessibility

- Every screen works down to phone width: the rail turns into a top bar
  under 900 px; groups stack when their card is narrow (container queries
  on `.set-card`); grids drop to one column under 540 px.
- Every interactive element is a real `button`, `input`, `select` or
  `summary` with a visible focus ring (`--accent`). A toggle that shows or
  hides something has `aria-expanded`; tabs use `role="tab"` and
  `aria-selected`.
- Motion is short (≤ 0.15 s) and off under `prefers-reduced-motion`.
- Colour is never the only signal: lights have titles, chips have words,
  invalid inputs have a message.
- Anything kept in `localStorage` (tabs, folded cards) is a convenience,
  wrapped in `try`/`catch`; the page works without it.
