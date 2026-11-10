// What the three creatures share: their props, how open the eyes are in a pose, and the small
// marks a pose adds (a sleeper's "z"s, the "!" of a waiting pet).

import type { Pose } from '../model.ts';

export interface CreatureProps {
  pose: Pose;
  /** The eyes are shut for an instant (the page times the blinks). */
  blink: boolean;
}

/** `shut`, `half` or nothing: closed for sleep and blinks, half for a creature that is down. */
export function eyeState(pose: Pose, blink: boolean): '' | 'shut' | 'half' {
  if (pose === 'sleeping' || blink) return 'shut';
  return pose === 'failed' ? 'half' : '';
}

/** Rising "z"s, shown while asleep. `x` and `y` are where the first starts. */
export function Zs({ x, y }: { x: number; y: number }) {
  return (
    <g className="pet-zs" fill="var(--pet-ink)" fontFamily="sans-serif" fontWeight="700">
      <text className="pet-z pet-z1" x={x} y={y} fontSize="8">
        z
      </text>
      <text className="pet-z pet-z2" x={x + 6} y={y - 6} fontSize="6">
        z
      </text>
    </g>
  );
}

/** A round "!" in the accent colour, shown while the pet waits for the user. */
export function Bang({ x, y }: { x: number; y: number }) {
  return (
    <g className="pet-bang">
      <circle cx={x} cy={y} r="5.4" fill="var(--accent)" />
      <rect x={x - 0.8} y={y - 3} width="1.6" height="3.7" rx="0.8" fill="#fff" />
      <circle cx={x} cy={y + 2.6} r="1" fill="#fff" />
    </g>
  );
}

/** Small round sparks that fade in turn, shown while celebrating. */
export function Sparks({ spots }: { spots: [number, number][] }) {
  return (
    <g className="pet-sparks" fill="var(--accent)">
      {spots.map(([x, y], i) => (
        <circle key={i} className="pet-spark" cx={x} cy={y} r={i % 2 ? 1.2 : 1.7} style={{ animationDelay: `${i * 0.18}s` }} />
      ))}
    </g>
  );
}
