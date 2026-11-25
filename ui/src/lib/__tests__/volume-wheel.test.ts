import { describe, expect, it } from "vitest";
import { VolumeWheel, wheelNotches, type WheelSample } from "../volume-wheel.js";

let clock = 0;
/** A wheel event 16 ms after the previous one unless `timeStamp` says otherwise. */
const ev = (fields: Partial<WheelSample>): WheelSample => ({
  deltaX: 0,
  deltaY: 0,
  deltaMode: 0,
  ctrlKey: false,
  timeStamp: (clock += 16),
  ...fields,
});

describe("wheelNotches", () => {
  it("reads one notch from wheelDeltaY whatever pixel distance the platform gives it", () => {
    // X11-style 53 px, Windows-style 100 px and Wayland-style 120 px notches at 100% zoom; the
    // display scale (2x on the target machine) cancels out before the page sees the event.
    for (const px of [53, 100, 120]) {
      expect(wheelNotches(ev({ deltaY: px, wheelDeltaY: -120 }), 1)).toBe(1);
      expect(wheelNotches(ev({ deltaY: -px, wheelDeltaY: 120 }), 1)).toBe(-1);
    }
    // Blink divides by the page zoom: at a 150% interface scale a notch is 80 and 80.
    expect(wheelNotches(ev({ deltaY: 80, wheelDeltaY: -80 }), 1.5)).toBe(1);
    // And truncates wheelDeltaY: 120 / 1.1 = 109.09 reads as 109.
    expect(wheelNotches(ev({ deltaY: 109.09, wheelDeltaY: -109 }), 1.1)).toBe(1);
    // A fast turn that Chromium coalesced into one event (or a compositor scroll factor of 2,
    // which the event cannot tell apart) counts as two notches.
    expect(wheelNotches(ev({ deltaY: 240, wheelDeltaY: -240 }), 1)).toBe(2);
  });

  it("falls back to deltaY by deltaMode when wheelDeltaY is missing or 0", () => {
    expect(wheelNotches(ev({ deltaY: 120 }), 1)).toBe(1);
    expect(wheelNotches(ev({ deltaY: 80 }), 1.5)).toBe(1);
    expect(wheelNotches(ev({ deltaY: 3, deltaMode: 1 }), 1)).toBe(1);
    expect(wheelNotches(ev({ deltaY: -1, deltaMode: 2 }), 1)).toBe(-1);
    expect(wheelNotches(ev({ deltaY: 0.4, wheelDeltaY: 0 }), 1)).toBeCloseTo(0.4 / 120);
  });
});

describe("VolumeWheel", () => {
  it("moves five points per notch for every notch size", () => {
    const wheel = new VolumeWheel();
    expect(wheel.next(ev({ deltaY: -53, wheelDeltaY: 120 }), 50, 1)).toBe(55);
    expect(wheel.next(ev({ deltaY: -100, wheelDeltaY: 120 }), 55, 1)).toBe(60);
    expect(wheel.next(ev({ deltaY: -120, wheelDeltaY: 120 }), 60, 1)).toBe(65);
    expect(wheel.next(ev({ deltaY: 3, deltaMode: 1 }), 65, 1)).toBe(60);
    expect(wheel.next(ev({ deltaY: 1, deltaMode: 2 }), 60, 1)).toBe(55);
    expect(wheel.next(ev({ deltaY: 109.09, wheelDeltaY: -109 }), 55, 1.1)).toBe(50);
    expect(wheel.next(ev({ deltaY: 80, wheelDeltaY: -80 }), 50, 1.5)).toBe(45);
  });

  it("adds up a touchpad's small deltas and never takes one of them for a notch", () => {
    const wheel = new VolumeWheel();
    // 4 units is a thirtieth of a notch, a sixth of a point.
    const tiny = () => ev({ deltaY: -4, wheelDeltaY: 4 });
    expect(wheel.next(tiny(), 50, 1)).toBe(50);
    let volume = 50;
    for (let i = 1; i < 32; i++) volume = wheel.next(tiny(), volume, 1)!;
    expect(volume).toBe(55);
  });

  it("acts on a reversal at once, with nothing carried from the old direction", () => {
    const wheel = new VolumeWheel();
    // Half a notch up: two points shown, half a point carried upwards.
    expect(wheel.next(ev({ deltaY: -60, wheelDeltaY: 60 }), 50, 1)).toBe(52);
    expect(wheel.next(ev({ deltaY: 120, wheelDeltaY: -120 }), 52, 1)).toBe(47);
  });

  it("stops at 0 and 100 and stores nothing past them", () => {
    const wheel = new VolumeWheel();
    expect(wheel.next(ev({ deltaY: -120, wheelDeltaY: 120 }), 98, 1)).toBe(100);
    expect(wheel.next(ev({ deltaY: -1200, wheelDeltaY: 1200 }), 100, 1)).toBe(100);
    expect(wheel.next(ev({ deltaY: 120, wheelDeltaY: -120 }), 100, 1)).toBe(95);
    expect(wheel.next(ev({ deltaY: 12000, wheelDeltaY: -12000 }), 3, 1)).toBe(0);
    // Up from 0, small movement still adds up instead of being dropped at the limit.
    expect(wheel.next(ev({ deltaY: -12, wheelDeltaY: 12 }), 0, 1)).toBe(0);
    expect(wheel.next(ev({ deltaY: -12, wheelDeltaY: 12 }), 0, 1)).toBe(1);
  });

  it("forgets leftover movement after a pause", () => {
    const wheel = new VolumeWheel();
    expect(wheel.next(ev({ deltaY: -36, wheelDeltaY: 36 }), 50, 1)).toBe(51);
    // 0.5 point carried; after a pause the same movement gives one point, not two.
    expect(wheel.next(ev({ deltaY: -36, wheelDeltaY: 36, timeStamp: (clock += 1000) }), 51, 1)).toBe(52);
  });

  it("leaves Ctrl+wheel, horizontal and empty gestures to the page", () => {
    const wheel = new VolumeWheel();
    expect(wheel.next(ev({ deltaY: -120, wheelDeltaY: 120, ctrlKey: true }), 50, 1)).toBeNull();
    expect(wheel.next(ev({ deltaX: 80, deltaY: 10 }), 50, 1)).toBeNull();
    expect(wheel.next(ev({ deltaX: 40, deltaY: 0 }), 50, 1)).toBeNull();
  });
});
