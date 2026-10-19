import { test } from 'node:test';
import assert from 'node:assert/strict';
import { FACE, gumPath, lipPath, pieSlice, starPoints, superellipse, teethPath } from './geometry.ts';

const near = (a: number, b: number, eps = 0.05) => assert.ok(Math.abs(a - b) <= eps, `${a} is not within ${eps} of ${b}`);

test('the head superellipse spans x 21-79, y 30-84 and its 45° point sits at (50 ± 24.6, 57 ± 22.9)', () => {
  const h = FACE.head;
  const pts = superellipse(h.cx, h.cy, h.a, h.b, h.n, 8);
  near(pts[0]!.x, 79);
  near(pts[0]!.y, 57);
  near(pts[2]!.y, 84);
  near(pts[4]!.x, 21);
  near(pts[6]!.y, 30);
  // The parameter at 45° (index 1 of 8) lands on the documented corner point.
  near(pts[1]!.x, 50 + 24.6);
  near(pts[1]!.y, 57 + 22.9);
  for (const p of superellipse(h.cx, h.cy, h.a, h.b, h.n)) {
    near(Math.abs((p.x - h.cx) / h.a) ** h.n + Math.abs((p.y - h.cy) / h.b) ** h.n, 1, 1e-9);
  }
});

test('seven teeth gaps at a 50/7 pitch, from 28.57 to 71.43', () => {
  assert.equal(FACE.teethX.length, 7);
  near(FACE.teethX[0]!, 28.57, 0.01);
  near(FACE.teethX[3]!, 50, 0.01);
  near(FACE.teethX[6]!, 71.43, 0.01);
  assert.equal((teethPath().match(/M/g) ?? []).length, 7);
});

test('the lip is closed and passes through its corners; the gum line runs from x 21 to 78.6', () => {
  const lip = lipPath();
  assert.ok(lip.startsWith('M25 60'));
  assert.ok(lip.includes(' 75 58C'), 'the upper edge ends at the right corner and the lower edge starts there');
  assert.ok(lip.endsWith('25 60Z'));
  const gum = gumPath();
  assert.ok(gum.startsWith('M21 63.24L25.8 63.24C'));
  assert.ok(gum.endsWith('L78.6 61.54'));
});

test('an eight-point star alternates long and short tips around its centre', () => {
  const pts = starPoints(13, 13, 8, [7, 4.6], 2.1);
  assert.equal(pts.length, 16);
  const r = pts.map((p) => Math.hypot(p.x - 13, p.y - 13));
  near(r[0]!, 7, 1e-9);
  near(r[1]!, 2.1, 1e-9);
  near(r[2]!, 4.6, 1e-9);
  near(r[4]!, 7, 1e-9);
  // Tip 0 points straight up with no rotation.
  near(pts[0]!.x, 13, 1e-9);
  near(pts[0]!.y, 6, 1e-9);
});

test('a glint slice sweeps clockwise between its two rays at its radius', () => {
  const d = pieSlice({ x: 0, y: 0 }, 5, { x: 0, y: -1 }, { x: 1, y: 0 });
  assert.equal(d, 'M0 0L0 -5A5 5 0 0 1 5 0Z');
});
