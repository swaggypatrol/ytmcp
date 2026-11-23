import { render } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";
import Icon, { ICON_NAMES, glyphFor } from "../../components/Icon.svelte";

const ELEMENTS = new Set(["path", "circle", "rect", "line", "polyline", "polygon", "g"]);
const ATTRIBUTES = new Set(["d", "cx", "cy", "r", "x", "y", "width", "height", "rx", "ry", "x1", "y1", "x2", "y2", "points", "fill", "stroke", "stroke-width"]);
const SVG_NS = "http://www.w3.org/2000/svg";

describe("glass icons", () => {
  it("draws every icon name from allowlisted elements and attributes in currentColor only", () => {
    for (const name of ICON_NAMES) {
      for (const filled of [false, true]) {
        const glyph = glyphFor(name, filled);
        expect(glyph.length, name).toBeGreaterThan(0);
        for (const [tag, attrs] of glyph) {
          expect(ELEMENTS.has(tag), `${name}: <${tag}>`).toBe(true);
          for (const [key, value] of Object.entries(attrs)) {
            expect(ATTRIBUTES.has(key), `${name}: ${key}`).toBe(true);
            if (key === "fill" || key === "stroke") expect(["currentColor", "none"], `${name}: ${key}`).toContain(value);
          }
        }
      }
    }
  });

  it("renders SVG-namespaced elements and switches to the filled variant for active states", () => {
    const { container, rerender } = render(Icon, { name: "heart" });
    const svg = container.querySelector("svg")!;
    expect(svg.getAttribute("fill")).toBe("none");
    const outline = svg.querySelector("path")!;
    expect(outline.namespaceURI).toBe(SVG_NS);
    expect(outline.hasAttribute("fill")).toBe(false);

    rerender({ name: "heart", filled: true });
    expect(container.querySelector("path")!.getAttribute("fill")).toBe("currentColor");

    // Shuffle has no filled glyph in the pack; the active state keeps the outline.
    expect(glyphFor("shuffle", true)).toBe(glyphFor("shuffle"));
  });
});
