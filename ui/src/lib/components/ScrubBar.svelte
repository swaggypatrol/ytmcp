<script lang="ts">
  /**
   * Seek and volume: a flat rounded bar with no thumb. White (the surface's text colour) up to
   * the value, a lighter grey up to `buffered` when given, a dark grey track for the rest. It gets
   * a little thicker while pressed.
   *
   * Interaction, keyboard and screen readers go through a transparent native range input laid
   * over the bar, so it behaves exactly like one; under forced colours that input is shown as it
   * is. Usage: `<ScrubBar value={42} max={100} label="Volume" oninput={…} />`.
   */
  interface Props {
    value: number;
    max: number;
    min?: number;
    /** When set, the arrow keys move the value by this much instead of by 1. */
    keyStep?: number;
    disabled?: boolean;
    label: string;
    valueText?: string;
    title?: string;
    /** How far the media has loaded, in the same units as `value`. */
    buffered?: number | null;
    class?: string;
    testid?: string;
    oninput?: (value: number) => void;
    onchange?: (value: number) => void;
    onpress?: () => void;
    onrelease?: () => void;
  }

  let {
    value,
    max,
    min = 0,
    keyStep,
    disabled = false,
    label,
    valueText,
    title,
    buffered = null,
    class: className = "",
    testid,
    oninput,
    onchange,
    onpress,
    onrelease,
  }: Props = $props();

  let pressed = $state(false);

  const span = $derived(Math.max(max - min, Number.EPSILON));
  const fraction = (v: number) => Math.min(1, Math.max(0, (v - min) / span));
  const fill = $derived(fraction(value));
  const loaded = $derived(buffered == null ? 0 : fraction(buffered));

  function press() {
    if (disabled) return;
    pressed = true;
    onpress?.();
  }

  /** Arrow keys move by `keyStep`; dragging keeps the input's fine resolution. */
  function keydown(event: KeyboardEvent & { currentTarget: HTMLInputElement }) {
    if (!keyStep || disabled) return;
    const direction = { ArrowRight: 1, ArrowUp: 1, ArrowLeft: -1, ArrowDown: -1 }[event.key];
    if (!direction) return;
    event.preventDefault();
    const next = Math.min(max, Math.max(min, Number(event.currentTarget.value) + direction * keyStep));
    oninput?.(next);
    onchange?.(next);
  }

  function release() {
    if (!pressed) return;
    pressed = false;
    onrelease?.();
  }
</script>

<div
  class="scrub {className}"
  class:is-pressed={pressed}
  class:is-disabled={disabled}
  style="--fill: {(fill * 100).toFixed(3)}%; --buffered: {(loaded * 100).toFixed(3)}%;"
>
  <div class="scrub-track" aria-hidden="true">
    <div class="scrub-buffered"></div>
    <div class="scrub-fill"></div>
  </div>
  <input
    type="range"
    class="scrub-input"
    {min}
    {max}
    {value}
    {disabled}
    {title}
    aria-label={label}
    aria-valuetext={valueText}
    onpointerdown={press}
    onpointerup={release}
    onpointercancel={release}
    onlostpointercapture={release}
    onblur={release}
    onkeydown={keydown}
    oninput={(e) => oninput?.(Number(e.currentTarget.value))}
    onchange={(e) => onchange?.(Number(e.currentTarget.value))}
    data-testid={testid}
  />
</div>
