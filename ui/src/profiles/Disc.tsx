// A profile's disc: its colour with its initial, and the stacked pair that stands for all profiles.

import { initialOf } from './model.ts';

export function ProfileDisc({ name, colour, size = 18 }: { name: string; colour: string; size?: number }) {
  return (
    <span className="pf-disc" aria-hidden="true" style={{ background: colour, width: size, height: size, fontSize: Math.round(size * 0.55) }}>
      {initialOf(name)}
    </span>
  );
}

/** Two overlapping discs: all profiles. */
export function AllDiscs({ size = 18 }: { size?: number }) {
  const d = Math.round(size * 0.78);
  return (
    <span className="pf-all" aria-hidden="true" style={{ width: size + Math.round(d * 0.4), height: size }}>
      <span className="pf-all-disc" style={{ width: d, height: d, left: 0, top: 0 }} />
      <span className="pf-all-disc" style={{ width: d, height: d, right: 0, bottom: 0 }} />
    </span>
  );
}
