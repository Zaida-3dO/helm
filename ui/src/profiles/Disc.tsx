// A profile's disc: its colour with its initial.

import { initialOf } from './model.ts';

export function ProfileDisc({ name, colour, size = 18 }: { name: string; colour: string; size?: number }) {
  return (
    <span className="pf-disc" aria-hidden="true" style={{ background: colour, width: size, height: size, fontSize: Math.round(size * 0.55) }}>
      {initialOf(name)}
    </span>
  );
}
