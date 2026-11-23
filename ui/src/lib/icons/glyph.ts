/** Element name allowed inside an icon; matches the allowlist the generator enforces. */
export type GlyphElement = "path" | "circle" | "rect" | "line" | "polyline" | "polygon" | "g";

/** One icon on the 24×24 grid: self-closing elements with their attributes, drawn in order. */
export type GlassGlyph = readonly (readonly [GlyphElement, Readonly<Record<string, string>>])[];
