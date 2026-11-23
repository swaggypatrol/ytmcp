# Liquid glass

The interface uses a refractive glass material for three surfaces: the sidebar, the floating player bar and popovers (menus, popup panels and dialog panels). The surfaces never overlap. A dialog's full-window scrim is a plain translucent color with no blur.

Implementation: `ui/src/lib/glass/` (`glass.css`, `liquid-glass.ts`, and the tone audit in `__tests__/tone.test.ts`). Probes: `tools/probes/glass/`.

## Summary

- `backdrop-filter: url(#filter)` works in the pinned Chromium, so refraction, dispersion, frost and tone mapping run in **one SVG filter per surface**.
- The refraction field is procedural, with no bitmap: `feFlood` → `feGaussianBlur` → `feConvolveMatrix`. A bitmap displacement map through `feImage` was tried first and dropped, because its per-frame cost grows with bitmap size times the number of references (median frame time rose to 25 ms in a prototype).
- Text contrast on glass is constrained by construction: a unit test derives the worst case from the tokens in `glass.css`, and screenshot scans of the real interface found no text below 4.5:1 and no icon below 3:1 in either theme, including over adversarial backgrounds.
- In a 60-second scroll test of the real interface, the 50th, 95th and 99th percentile frame times matched the reduced-transparency (opaque) baseline, and long frames stayed far below 1%.
- Nothing animates while the glass is idle: no rAF loop, timer or CSS animation runs.

The glass does not flip between dark and light depending on what is underneath. Its palette follows the app theme; within one palette, its tone changes continuously with the brightness below.

## Material layers

From bottom to top:

| Layer | Implementation | Purpose |
| --- | --- | --- |
| Scroll edge | `.lg-scroll-edge`: a separate element with `backdrop-filter: blur()`, a gradient mask and a fade toward the canvas color | Content softens and dims before it slides under the player bar |
| Shadow | `box-shadow` on `.lg`: a 0.5 px contact line, a near shadow and an ambient shadow | Lifts the glass off the content |
| Lens | `backdrop-filter: url(#lg-lens-N)` on `.lg` | Refraction, dispersion, frost and tone mapping in one pass |
| Tint | `background-color: var(--lg-tint)` on `.lg[data-lg-lens]`, drawn over the filter result; the heavier `--lg-tint-plain` when no lens is active | The palette base and part of the contrast bound |
| Rim | `.lg::before` (a 1 px masked ring), `.lg::after` (a sheen) and inset shadows | Highlight edge, bevel shading, thickness |
| Content | ordinary children, colored with `--lg-text-*` | Text and controls |

Inside the filter, in order: a white rectangle the size of the element (`feFlood`); a blur that turns it into a height map rising from 0.5 at the outline to 1 on the flat; a Sobel gradient of the height map (two `feConvolveMatrix`) as the displacement field; a light frost blur of the backdrop; three `feDisplacementMap` passes with slightly different scales for R, G and B (dispersion); a saturation boost; a large blur that serves as both thick frost and local mean brightness; a mask that selects the thick frost under text; and the tone mapping. The filter must use `color-interpolation-filters="sRGB"`; the default `linearRGB` distorts the displacement map.

## Refraction field

The glass is modeled as a flat slab with rounded edges, viewed straight on. The flat part does not bend light; on the curved rim, rays bend inward, so the displacement always points into the shape and is strongest at the outline, falling to zero inward.

- Height: `h(d) = Φ(d / σ)`, where `d` is the distance to the outline, `Φ` the normal cumulative distribution and `σ = --lg-bezel / 2.2`.
- Displacement: `offset(d) = --lg-reach × exp(−d² / 2σ²)` along the inward normal.

When `reach / σ` exceeds about 1.65, a narrow mirrored band appears at the rim, as it does at the edge of thick glass. Corners need no special case: the blurred rectangle's contours round off by themselves, and the element's `border-radius` clips the result.

One filter per surface uses `userSpaceOnUse`. A `ResizeObserver` rewrites the filter region, the flood size, the text-area rectangles and the derived constants; one update takes well under a millisecond. The Sobel kernel works in device pixels, so its divisor is corrected by `devicePixelRatio`, and a resolution media query rewrites it when the DPR changes. `feDisplacementMap`'s `scale` is in CSS pixels.

## Readability

- **Scroll edge.** Below the content and under the player bar, a strip blurs and dims content before it reaches the glass. It covers only the main content area.
- **Plates.** The filter keeps two versions of the backdrop: a lightly frosted, refracted one and a heavily blurred mean (`--lg-frost-plate`). The rim stays clear so the refraction shows; the body uses `--lg-body-frost` (1 on the sidebar and popovers, 0.45 on the player bar); and every child marked `data-lg-plate` gets a feathered rectangle of full plate. A 13 px blur removes the strokes of body-size text beneath, leaving only slowly changing color behind text. Nested `backdrop-filter` does not work in Chromium, so plates must live in the same filter.
- **Tone mapping.** The mean also measures local brightness. Dark palette: `out = body × (a − b · Y(mean))`; light palette: the mirrored form `1 − (1 − body) × (a − b · (1 − Y(mean)))`, with `a = --lg-tone-gain`, `b = --lg-tone-adapt` and `Y` the Rec. 709 luma of the sRGB values. The brighter the content below, the more a dark glass darkens it.
- **Tone audit.** Inside a plate the output depends only on the backdrop color, so `tone.test.ts` searches 25³ backdrop colors × sheen × three fills (none, hover, selected) using the same `toneMatrix()` as the runtime and the token values parsed from `glass.css`. It fails if any text color drops below 4.5:1 or any icon color below 3:1, with a 1.5% margin, because a screenshot once measured 1.2% below the model. The worst backdrop for the dark palette is a saturated color rather than white.
- **Text colors.** Glass has its own tokens, and `.lg` remaps the general content tokens (`--text-*`, `--accent`, `--status-*`, `--focus-ring`, `--bg-*`) to them, so a component on glass cannot pick an unaudited color. Accent color is used for icons and small marks only; the current item is shown with a selected fill, primary text and an accent icon. Primary buttons have their own fill and text pair.

The guarantee holds only for text and icons inside a plate: a surface with body frost 1, or an element marked `data-lg-plate`.

## Highlights, dispersion and shadow

- The rim is a 1 px ring with a 135° gradient and a radial highlight at `--lg-light-x/y`, following the shape through `border-radius: inherit`.
- Three inset shadows shade the bevel: a light top line, a dark bottom line and a faint glow along the rim.
- Dispersion scales the three displacements by `reach·(1+δ)`, `reach` and `reach·(1−δ)` with `δ = --lg-dispersion`, so only the rim shows a pixel of red and blue fringe.
- `--lg-shadow` (sidebar, player bar) and `--lg-shadow-raised` (popovers) lift the glass; the contact line keeps an edge on light content.

## Motion

| Action | Implementation |
| --- | --- |
| Hover | `.lg-press:hover` fills `--lg-fill-hover` over 160 ms |
| Press and release | `.lg-press:active` scales to `--lg-press-scale` in 90 ms and springs back with `--lg-ease-spring` |
| Popover entry | `.lg-morph` uses `@starting-style` to grow from `--lg-morph-from` with `--lg-ease-settle` and a quick fade |
| Pointer highlight | `pointermove` writes `--lg-light-x/y` at most once per frame; nothing is scheduled while still or hidden |
| Reduced motion | `prefers-reduced-motion` or the app setting: no scaling or movement, only color and opacity transitions; no pointer highlight |

The spring curves are sampled step responses of underdamped second-order systems (damping 0.42 and 0.7) written as CSS `linear()`. Collapsing the sidebar switches in one step, because animating its width resizes two lenses every frame and dropped frames noticeably.

## Icons

The interface icons (play, pause, previous, next, volume, lyrics, settings and the rest) come from a glass-styled SVG icon set that follows the same design language as the material. On glass they are held to the 3:1 icon contrast like any other icon. They are rendered through `ui/src/lib/components/Icon.svelte`.

## Tokens

Optics, read by `liquid-glass.ts`:

| Token | Default | Meaning |
| --- | --- | --- |
| `--lg-bezel` | 20px (sidebar 18, popover 16) | Width of the curved rim |
| `--lg-reach` | 17px (player bar 20, popover 13) | Peak displacement at the outline |
| `--lg-dispersion` | 0.06 | R and B displacement relative to G |
| `--lg-frost-clear` | 2.4px | Blur of the clear glass |
| `--lg-frost-plate` | 13px | Plate blur, also the radius of the local mean |
| `--lg-body-frost` | 0.3 (sidebar and popover 1, player bar 0.45) | Plate density of the body |
| `--lg-plate-feather` | 24px | Feather of text-area plates |
| `--lg-saturation` | 1.45 | Saturation |
| `--lg-tone-gain`, `--lg-tone-adapt` | 0.8, 0.6 | Tone mapping `a` and `b` |
| `--lg-scheme` | dark or light | Selects the tone formula; follows the theme |

Surface colors exist in a dark and a light set: `--lg-tint`, `--lg-tint-plain`, `--lg-solid`, `--lg-text-primary`, `--lg-text-secondary`, `--lg-accent`, `--lg-accent-fill`, `--lg-on-accent-fill`, `--lg-status-success/warning/danger/info`, `--lg-focus-ring`, `--lg-hairline`, `--lg-fill-hover`, `--lg-fill-active`, the rim, sheen, bevel and shadow tokens, `--lg-rim-width` and `--lg-fallback-blur`. The scroll edge uses `--lg-edge-blur` (5px), `--lg-edge-fade` (64px) and `--lg-edge-dim` (0.22). Motion uses `--lg-duration-*`, `--lg-press-scale`, `--lg-morph-from`, `--lg-ease-spring` and `--lg-ease-settle`.

After changing a tint, tone, sheen, fill or text token, run `pnpm --filter @ytm-glass/ui test`: the tone audit fails rather than letting contrast degrade silently.

## Fallbacks

| Condition | Behavior |
| --- | --- |
| The runtime has not mounted, or `CSS.supports('backdrop-filter', 'url(#x)')` is false | CSS defaults apply: `blur(--lg-fallback-blur) saturate()` with the much heavier `--lg-tint-plain`; rim and shadow stay. `liquid-glass.ts` adds `data-lg-lens` once the lens is active, which switches to the light tint. |
| `prefers-reduced-transparency: reduce` or the app setting | `backdrop-filter: none` on a solid `--lg-solid` base; the scroll edge becomes a plain fade; no filters are created. |
| `forced-colors: active` | `Canvas` background with a 1 px `CanvasText` border, no shadow or highlight, `Highlight` outlines for the selection; sliders are drawn with system colors because forced colors remove their gradient tracks. |
| `prefers-reduced-motion: reduce` or the app setting | See Motion. |

## Measurements

The contrast scans and frame timing ran the real interface in mock mode (`YTM_GLASS_MODE=mock`, temporary XDG directories, synthetic covers and a 10,000-row playlist) at 2x scaling on a 120 Hz display. Each scene was scanned per device pixel with glyphs hidden, then again over 12 adversarial backgrounds (white, black, six saturated colors, stripes of 2, 13 and 48 px, half white and half magenta). Every text measurement met 4.5:1 and every icon 3:1, in dark, light and reduced-transparency modes. The player bar keeps all of its controls, including the volume slider, at the minimum window size of 900×600; only the diagnostics button moves to Settings.

Frame timing scrolled the virtual list under the player bar for 60 seconds per scene. Dark, light, dialog-open and opaque baseline runs all had the same 50th, 95th and 99th percentile frame times, one refresh interval or just above it, with long frames below 0.06%. The glass costs at most a few dozen single missed refreshes per minute; it cannot be shown to be zero.

## Chromium behavior relied on

- `backdrop-filter: url()` is Chromium-only. Sobel kernels working in device pixels and mirrored sampling beyond the element are not guaranteed by any specification, which is one reason Electron is pinned; rerun the `behavior` probe after upgrading it.
- `feDisplacementMap` and the default `feGaussianBlur` sample a mirror of the element's own backdrop beyond its edge rather than the real content outside; output is always clipped to the border box.
- Changing `scale`, `stdDeviation` and similar attributes repaints every element using the filter at once.
- During a `scale()` transition the backdrop filter stays active; with `opacity < 1`, unfiltered content shows through, so popover fades are short.

## Open risks

- The material has been measured on one high-end GPU only. Each surface's filter has about 30 primitives; the cost on weaker GPUs is unknown. Reduced transparency is the escape hatch.
- Appearance on an HDR display has not been judged by eye; all color results come from captured frames.
- Frame timing used rAF intervals with main-thread scrolling; compositor-thread wheel scrolling and presented frames (tracing) were not measured, nor real cover decoding with the hidden player page running.
- Dark glass over very bright content looks rather dark; that is the direct cost of keeping secondary text at 4.5:1.
- Hover contrast and the plain-blur fallback are covered by the audit's calculation only, not by screenshot scans.

## Reproducing

```sh
pnpm --filter @ytm-glass/ui run build
pnpm --filter @ytm-glass/desktop run build
node tools/probes/glass/run-probe.mjs shots     # screenshots, contrast scan, interaction and fallbacks
node tools/probes/glass/run-probe.mjs perf 60   # frame intervals
node tools/probes/glass/run-probe.mjs look dark # page screenshots for visual review
node tools/probes/glass/run-probe.mjs behavior  # Chromium behavior matrix on a synthetic page
```

Output goes to `tools/probes/glass/out/`. The probe refuses to run outside mock mode, points every XDG directory at a temporary directory and never contacts `music.youtube.com`. Keep the window visible and the machine otherwise idle while timing frames.
