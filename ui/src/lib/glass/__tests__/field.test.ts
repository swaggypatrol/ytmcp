// @vitest-environment node
import { describe, expect, it } from "vitest";
import { cornerExponent, cornerTexel, refractionProfile } from "../liquid-glass.js";

describe("liquid glass refraction field", () => {
  it("bends most at the outline, not at all where the glass turns flat", () => {
    expect(refractionProfile(0)).toBeCloseTo(1, 6);
    expect(refractionProfile(1)).toBe(0);
    expect(refractionProfile(1.5)).toBe(0);
    for (let t = 0.01; t < 1; t += 0.01) expect(refractionProfile(t + 0.01)).toBeLessThanOrEqual(refractionProfile(t));
  });

  it("makes a thick-lens rim: inverted and compressed at the outline, upright inside, one turn", () => {
    // Depth x inside the outline shows what lies at x + reach × profile(x / bezel). The surfaces
    // set reach to about 1.4 × bezel.
    const bezel = 24;
    const reach = 33;
    const sample = (x: number) => x + reach * refractionProfile(x / bezel);
    const slope = (x: number) => (sample(x + 0.01) - sample(x)) / 0.01;
    // The image is upside down at the outline, compressed but still readable (not a smear line).
    expect(slope(0)).toBeLessThan(-0.8);
    expect(slope(0)).toBeGreaterThan(-2.5);
    // It turns exactly once and is upright from there inward.
    let turns = 0;
    for (let x = 0.25; x < bezel; x += 0.25) if (Math.sign(slope(x)) !== Math.sign(slope(x - 0.25))) turns += 1;
    expect(turns).toBe(1);
    expect(slope(bezel - 0.5)).toBeGreaterThan(0.5);
    // Nothing is sampled from further inside than the reach.
    for (let x = 0; x <= bezel; x += 0.5) expect(sample(x)).toBeLessThanOrEqual(reach + 1e-9);
  });

  it("points inward along the outline's normal, radially around a rounded corner", () => {
    const left = cornerTexel(3, 60, 22, 28, 2);
    expect(left.dx).toBeGreaterThan(0);
    expect(left.dy).toBe(0);
    const top = cornerTexel(60, 3, 22, 28, 2);
    expect(top.dy).toBeGreaterThan(0);
    expect(top.dx).toBe(0);
    // On the corner's diagonal the offset points at the corner centre: equal parts x and y.
    const diagonal = cornerTexel(22 - 18 / Math.SQRT2, 22 - 18 / Math.SQRT2, 22, 28, 2);
    expect(diagonal.dx).toBeGreaterThan(0);
    expect(diagonal.dx).toBeCloseTo(diagonal.dy, 6);
    // Outside the rounded outline nothing moves; deeper than the rim nothing moves either.
    expect(cornerTexel(1, 1, 22, 28, 2)).toEqual({ dx: 0, dy: 0 });
    expect(cornerTexel(40, 40, 22, 28, 2)).toEqual({ dx: 0, dy: 0 });
  });

  it("reads the corner curve from corner-shape as Chromium reports it", () => {
    // Pinned Chromium computes `corner-shape: squircle` as superellipse(2).
    expect(cornerExponent("superellipse(2)")).toBe(4);
    expect(cornerExponent("squircle")).toBe(4);
    expect(cornerExponent("superellipse(1)")).toBe(2);
    expect(cornerExponent("round")).toBe(2);
    expect(cornerExponent("")).toBe(2);
    expect(cornerExponent("scoop")).toBe(2);
  });
});
