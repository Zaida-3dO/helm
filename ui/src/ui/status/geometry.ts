// Shape maths for the still status marks: the working face's head and grin, the completion
// star, the wheel. Pure functions returning SVG path data, so they can be tested under node.
//
// The face shares one 100 x 100 unit grid, x right and y down. Coordinates
// below are that grid's; the components fit a window of it into their box with the viewBox.

export interface Pt {
  x: number;
  y: number;
}

const r2 = (n: number) => Math.round(n * 100) / 100;
const xy = (p: Pt) => `${r2(p.x)} ${r2(p.y)}`;

/**
 * Points on a superellipse |dx/a|^n + |dy/b|^n = 1 about (cx, cy), starting at the right and going
 * clockwise on screen. `steps` points, evenly spaced in the parameter.
 */
export function superellipse(cx: number, cy: number, a: number, b: number, n: number, steps = 96): Pt[] {
  const e = 2 / n;
  const out: Pt[] = [];
  for (let i = 0; i < steps; i++) {
    const t = (i / steps) * Math.PI * 2;
    const c = Math.cos(t);
    const s = Math.sin(t);
    out.push({ x: cx + a * Math.sign(c) * Math.abs(c) ** e, y: cy + b * Math.sign(s) * Math.abs(s) ** e });
  }
  return out;
}

/** A closed polygon path. */
export function polygonPath(pts: Pt[]): string {
  return pts.length ? `M${pts.map(xy).join('L')}Z` : '';
}

/**
 * A smooth open curve through every point (Catmull-Rom, tension 0.5, as cubic Béziers). The ends
 * are not closed; append `Z` or chain another curve for a closed shape.
 */
export function smoothThrough(pts: Pt[], move = true): string {
  if (pts.length < 2) return '';
  let d = move ? `M${xy(pts[0]!)}` : '';
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i]!;
    const p1 = pts[i]!;
    const p2 = pts[i + 1]!;
    const p3 = pts[i + 2] ?? p2;
    const c1 = { x: p1.x + (p2.x - p0.x) / 6, y: p1.y + (p2.y - p0.y) / 6 };
    const c2 = { x: p2.x - (p3.x - p1.x) / 6, y: p2.y - (p3.y - p1.y) / 6 };
    d += `C${xy(c1)} ${xy(c2)} ${xy(p2)}`;
  }
  return d;
}

/** A star with `points` tips: radii alternate outer[i % outer.length] and `inner`. Tip 0 points up. */
export function starPoints(cx: number, cy: number, points: number, outer: number[], inner: number, rotateDeg = 0): Pt[] {
  const out: Pt[] = [];
  const step = Math.PI / points;
  const rot = (rotateDeg * Math.PI) / 180 - Math.PI / 2;
  for (let i = 0; i < points * 2; i++) {
    const r = i % 2 === 0 ? (outer[(i / 2) % outer.length] ?? 1) : inner;
    const t = rot + i * step;
    out.push({ x: cx + r * Math.cos(t), y: cy + r * Math.sin(t) });
  }
  return out;
}

/** A pie slice of radius r about c, from the ray towards `from` clockwise (on screen) to the ray towards `to`. */
export function pieSlice(c: Pt, r: number, from: Pt, to: Pt): string {
  const at = (p: Pt) => {
    const len = Math.hypot(p.x - c.x, p.y - c.y) || 1;
    return { x: c.x + ((p.x - c.x) / len) * r, y: c.y + ((p.y - c.y) / len) * r };
  };
  return `M${xy(c)}L${xy(at(from))}A${r2(r)} ${r2(r)} 0 0 1 ${xy(at(to))}Z`;
}

// ---- The working face and the brand mark's shared shapes. ----

export const FACE = {
  head: { cx: 50, cy: 57, a: 29, b: 27, n: 4.2 },
  eyes: [
    { cx: 39, cy: 50 },
    { cx: 61, cy: 50 },
  ],
  eye: { rx: 5.6, ry: 7.8 },
  cheeks: [
    { cx: 39, cy: 58.6 },
    { cx: 61, cy: 58.6 },
  ],
  cheek: { rx: 8.6, ry: 6.2 },
  glints: [
    { c: { x: 39.4, y: 49.8 }, from: { x: 42.6, y: 44.26 }, to: { x: 45.33, y: 47.4 } },
    { c: { x: 61.4, y: 49.8 }, from: { x: 64.6, y: 44.26 }, to: { x: 67.33, y: 47.4 } },
  ],
  glintRadius: 6.4,
  lipUpper: [
    { x: 25, y: 60 },
    { x: 37.5, y: 63.5 },
    { x: 50, y: 64.25 },
    { x: 62.5, y: 62.8 },
    { x: 75, y: 58 },
  ],
  lipLower: [
    { x: 75, y: 58 },
    { x: 72.2, y: 68.4 },
    { x: 62.5, y: 77.8 },
    { x: 50, y: 80 },
    { x: 37.5, y: 77.9 },
    { x: 28.2, y: 69.9 },
    { x: 25, y: 60 },
  ],
  gum: [
    { x: 25.8, y: 63.24 },
    { x: 33, y: 69.2 },
    { x: 41, y: 71.5 },
    { x: 49, y: 72.1 },
    { x: 57, y: 71.6 },
    { x: 65, y: 69.4 },
    { x: 70.6, y: 66.1 },
    { x: 73.8, y: 62.3 },
  ],
  /** Seven gaps at a 50/7 pitch across the lip, the first half a pitch in from x 25. */
  teethX: [0, 1, 2, 3, 4, 5, 6].map((i) => 25 + ((i + 0.5) * 50) / 7),
  stem: { x: 48.5, y: 17.3, w: 3, h: 14, r: 1 },
  ball: { cx: 50, cy: 14.8, r: 4.4 },
  /** The viewBox windows. */
  faceWindow: '20.5 4 59 80.5',
  markWindow: '20.5 9.9 59 74.6',
} as const;

export function headPath(): string {
  const h = FACE.head;
  return polygonPath(superellipse(h.cx, h.cy, h.a, h.b, h.n));
}

export function lipPath(): string {
  return `${smoothThrough([...FACE.lipUpper])}${smoothThrough([...FACE.lipLower], false)}Z`;
}

/** Level from x 21, through the dip, then level again to x 78.6 (draw it clipped to the lip). */
export function gumPath(): string {
  return `M21 63.24L${xy(FACE.gum[0])}${smoothThrough([...FACE.gum], false)}L78.6 61.54`;
}

export function teethPath(): string {
  return FACE.teethX.map((x) => `M${r2(x)} 55V84`).join('');
}

export function glintPaths(): string[] {
  return FACE.glints.map((g) => pieSlice(g.c, FACE.glintRadius, g.from, g.to));
}
