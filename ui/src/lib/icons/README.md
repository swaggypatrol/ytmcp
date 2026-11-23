# Icons

The UI draws its icons from Glass Music Icons 1.0.0, a stroke icon set for liquid glass interfaces
made for this project and licensed with it under the MIT license.
`glass-icons.generated.ts` holds the glyphs the app uses, converted from the set's SVG sources by
`ui/scripts/generate-glass-icons.mjs`. Do not edit the generated file; add a name to the script's
list and run it against an extracted copy of the set:

```sh
node ui/scripts/generate-glass-icons.mjs /path/to/glass-music-icons
```

The script accepts only `path`, `circle`, `rect`, `line`, `polyline`, `polygon` and `g` elements, a
fixed set of geometry attributes, and `fill`/`stroke` values of `currentColor` or `none`. Anything
else stops the run. `__tests__/icons.test.ts` checks the same rules on what is committed.

## Specs

| Item | Value |
|---|---|
| Grid | 24 × 24, live area 2–22 |
| Stroke | 1.75, round caps and joins, set on the root `<svg>` |
| Colour | `currentColor` only |
| States | Outline when inactive; filled variant plus accent when active |

Glyphs with no filled variant (shuffle, repeat, chevrons and others) stay outlined when active and
show the state with the accent colour and a soft accent fill.

## Use in the app

`Icon.svelte` maps the app's icon names to glyphs and takes `filled` for the active state. The
styles that apply the set's rules live in the existing tokens, not in a separate stylesheet:

- Icon buttons draw the glyph at half the button height (`.btn-icon` 32 → 16 px, `.btn-icon-lg`
  40 → 20 px).
- On glass, `--lg-icon` colours inactive glyphs: about 92% white on dark glass and about 84% black
  on light glass, never pure. The tone audit checks it at 3:1 like other marks.
- On glass, `--lg-icon-lift` adds a 1 px soft shade (dark) or a 0.5 px light edge (light) under each
  glyph. Forced-colours mode removes it.
- The play/pause button is the only control on the player bar that is not glass: a solid
  `--lg-accent-fill` disc with a filled glyph in `--lg-on-accent-fill`. Previous and next use
  their filled glyphs; the other player-bar controls are outlined.

## Glyphs drawn for this app

The set has no glyph for these; they are drawn locally in `Icon.svelte` to the same specs:

| Name | Use |
|---|---|
| `arrow-left` | Back buttons |
| `external-link` | Open the official player page |
| `now-playing` | Three bars marking the current track |
