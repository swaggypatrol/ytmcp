// @vitest-environment node
import { describe, expect, it } from "vitest";
import { cornerExponent, cornerTexel, lightShift, PIECE_PAINT_ORDER, pieceBoxes, refractionProfile, type PieceBox } from "../liquid-glass.js";

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

  it("lets the pointer nudge the rim light only a little either way, never across the surface", () => {
    expect(lightShift(0.5)).toBe("0.0%");
    expect(lightShift(0)).toBe("-8.0%");
    expect(lightShift(1)).toBe("8.0%");
    // Pointer events can land just outside the box while leaving it.
    expect(lightShift(-0.3)).toBe("-8.0%");
    expect(lightShift(1.7)).toBe("8.0%");
  });

  it("leaves no seam in the rim where pieces meet between device pixels: every point of the rim lies a device pixel deep inside some piece", () => {
    // A boundary inside a device pixel is only partly covered by the pieces that meet there; one
    // more piece under it, with the same values, has to cover that pixel whole. Sizes as the
    // sidebar, the player bar and a pill toast report them at 125% and 150%.
    const inside = (b: PieceBox, x: number, y: number, m: number) =>
      x - m >= b.x && x + m <= b.x + b.width && y - m >= b.y && y + m <= b.y + b.height;
    for (const [width, height, size] of [
      [224, 776, 25],
      [1009.6, 79.2, 25],
      [303.33, 44, 22],
    ] as const) {
      const boxes = pieceBoxes(width, height, size);
      const rim = size - 1;
      for (let a = 0.8; a < Math.max(width, height) - 0.8; a += 0.2) {
        for (const depth of [0.8, rim / 2, rim - 0.8]) {
          const points = [
            [a, depth],
            [a, height - depth],
            [depth, a],
            [width - depth, a],
          ].filter(([x, y]) => x! > 0.6 && x! < width - 0.6 && y! > 0.6 && y! < height - 0.6);
          for (const [x, y] of points) {
            // At 100% a device pixel is one CSS px: half of it either side of the point.
            expect(boxes.some((b) => inside(b, x!, y!, 0.5)), `${width}x${height} at (${x}, ${y})`).toBe(true);
          }
        }
      }
    }
  });

  it("paints the corner tiles over the strips, so the curve stays on top where they overlap", () => {
    expect([...PIECE_PAINT_ORDER]).toEqual([4, 5, 6, 7, 0, 1, 2, 3]);
    const [tl, , , , top] = pieceBoxes(300, 80, 25);
    expect(top!.x).toBeLessThan(tl!.x + tl!.width);
  });
});
