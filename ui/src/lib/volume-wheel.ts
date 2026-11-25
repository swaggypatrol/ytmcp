/** The fields of a wheel event the volume wheel reads; a plain object in tests. */
export interface WheelSample {
  readonly deltaX: number;
  readonly deltaY: number;
  readonly deltaMode: number;
  /** Legacy notch-based delta; absent outside Chromium, 0 for a movement under one unit. */
  readonly wheelDeltaY?: number;
  readonly ctrlKey: boolean;
  readonly timeStamp: number;
}

/** One mouse-wheel notch moves the volume by this many percentage points (a design value). */
export const WHEEL_NOTCH_PERCENT = 5;

// Chromium's unit per notch (ui::MouseWheelEvent::kWheelDelta). The browser scales the event by
// the display scale and Blink divides by devicePixelRatio (display scale times page zoom), so
// wheelDeltaY is ticks * 120 / page zoom: the display scale cancels and the zoom is put back.
const UNITS_PER_NOTCH = 120;
// Fallbacks when wheelDeltaY is missing or 0: lines per notch, and one page as one notch.
const LINES_PER_NOTCH = 3;
// Blink truncates wheelDeltaY to an integer, so a notch at a fractional ratio reads as 0.99.
const NOTCH_SNAP = 0.04;
// A pause this long ends a gesture; leftover sub-point movement is not carried into the next.
const GESTURE_IDLE_MS = 500;

/**
 * Notches in one event, positive downwards like deltaY. Taken from wheelDeltaY when present
 * (whole notches from a notched wheel, fractions from a touchpad or a high-resolution wheel),
 * otherwise from deltaY by its deltaMode. Pixel deltas use the same Chromium unit, which on
 * Linux (Ozone) is the very value wheelDeltaY is derived from. `pageZoom` is the window's zoom
 * factor (the interface scale), not the display scale.
 */
export function wheelNotches(sample: WheelSample, pageZoom: number): number {
  const zoom = pageZoom > 0 && Number.isFinite(pageZoom) ? pageZoom : 1;
  const legacy = sample.wheelDeltaY;
  const raw =
    typeof legacy === "number" && Number.isFinite(legacy) && legacy !== 0
      ? (-legacy * zoom) / UNITS_PER_NOTCH
      : sample.deltaMode === 1
        ? sample.deltaY / LINES_PER_NOTCH
        : sample.deltaMode === 2
          ? sample.deltaY
          : (sample.deltaY * zoom) / UNITS_PER_NOTCH;
  const whole = Math.round(raw);
  return whole !== 0 && Math.abs(raw - whole) < NOTCH_SNAP ? whole : raw;
}

/**
 * Turns wheel events over the volume control into volume targets in whole percentage points.
 * Fractions of a step add up (a touchpad stays fine and continuous), a reversal drops what was
 * left in the old direction, and nothing is stored past 0 or 100.
 */
export class VolumeWheel {
  private carry = 0;
  private lastAt = Number.NEGATIVE_INFINITY;

  public constructor(private readonly percentPerNotch = WHEEL_NOTCH_PERCENT) {}

  /**
   * The new volume (0..100) for this event, which may equal `current`; null when the event
   * belongs to the page (Ctrl+wheel zoom, a mostly horizontal or an empty gesture).
   */
  public next(sample: WheelSample, current: number, pageZoom: number): number | null {
    if (sample.ctrlKey || sample.deltaY === 0) return null;
    if (Math.abs(sample.deltaX) >= Math.abs(sample.deltaY)) return null;
    if (sample.timeStamp - this.lastAt > GESTURE_IDLE_MS) this.carry = 0;
    this.lastAt = sample.timeStamp;

    const change = -wheelNotches(sample, pageZoom) * this.percentPerNotch;
    if (Math.sign(change) !== Math.sign(this.carry)) this.carry = 0;
    this.carry += change;
    const steps = Math.trunc(this.carry);
    this.carry -= steps;
    const target = Math.min(100, Math.max(0, Math.round(current) + steps));
    // Pushing against a limit stores nothing, so the first step back is a full step.
    if ((target === 0 && change < 0) || (target === 100 && change > 0)) this.carry = 0;
    return target;
  }
}
