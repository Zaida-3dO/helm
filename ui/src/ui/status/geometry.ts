// Shape maths for the still status marks: the completion star. Pure functions returning SVG
// path data, so they can be tested under node.

export interface Pt {
  x: number;
  y: number;
}

const r2 = (n: number) => Math.round(n * 100) / 100;
const xy = (p: Pt) => `${r2(p.x)} ${r2(p.y)}`;

/** A closed polygon path. */
export function polygonPath(pts: Pt[]): string {
  return pts.length ? `M${pts.map(xy).join('L')}Z` : '';
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
