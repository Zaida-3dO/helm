import { test } from 'node:test';
import assert from 'node:assert/strict';
import { polygonPath, starPoints } from './geometry.ts';

const near = (a: number, b: number, eps = 0.05) => assert.ok(Math.abs(a - b) <= eps, `${a} is not within ${eps} of ${b}`);

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

test('a polygon path is closed and an empty one is empty', () => {
  assert.equal(polygonPath([{ x: 0, y: 0 }, { x: 4, y: 0 }, { x: 4.123, y: 2 }]), 'M0 0L4 0L4.12 2Z');
  assert.equal(polygonPath([]), '');
});
