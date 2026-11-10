// Glim, the ship's lantern: a brass lantern with a flame behind its glass and two dot eyes on
// the glass. Drawn here, moved by pet.css.

import { Bang, Zs, eyeState, type CreatureProps } from './shared.tsx';

const BRASS = '#c99a38';
const BRASS_DARK = '#8a5f12';
const EMBERS: [number, number][] = [
  [14, 20],
  [50, 18],
  [32, 6],
  [10, 40],
  [54, 38],
];

export function Lantern({ pose, blink }: CreatureProps) {
  const eyes = eyeState(pose, blink);
  return (
    <svg viewBox="0 0 64 64" className={`pet-svg pet-lantern pet-pose-${pose}`} aria-hidden="true" focusable="false">
      <g className="pet-all">
        <g className="pet-swing">
          <path d="M23 17 C23 3 41 3 41 17" fill="none" stroke={BRASS_DARK} strokeWidth="3.2" strokeLinecap="round" />
          <path d="M23 17 C23 3 41 3 41 17" fill="none" stroke={BRASS} strokeWidth="1.8" strokeLinecap="round" />
          <path d="M21 18 H43 L40 23.5 H24 Z" fill={BRASS} stroke={BRASS_DARK} strokeWidth="1" strokeLinejoin="round" />
          <rect x="21.5" y="23" width="21" height="25.5" rx="6" fill="var(--pet-light)" fillOpacity="0.42" stroke={BRASS_DARK} strokeWidth="1.6" />
          <g className="pet-flame">
            <path className="pet-flame-a" d="M32 25 C37.5 32 39.5 37 36.4 42.4 C35 45.2 29 45.2 27.6 42.4 C24.5 37 26.5 32 32 25 Z" />
            <path className="pet-flame-b" d="M32 32.5 C34.6 36 35.2 38.8 33.8 41.2 C33 42.6 31 42.6 30.2 41.2 C28.8 38.8 29.4 36 32 32.5 Z" />
          </g>
          <g className={`pet-eyes ${eyes}`}>
            <circle cx="28.2" cy="38.2" r="1.6" fill="var(--pet-ink)" />
            <circle cx="35.8" cy="38.2" r="1.6" fill="var(--pet-ink)" />
          </g>
          <rect x="19" y="48" width="26" height="6" rx="2" fill={BRASS} stroke={BRASS_DARK} strokeWidth="1" />
          <rect x="24" y="54" width="16" height="3.4" rx="1.7" fill={BRASS_DARK} />
        </g>
        {pose === 'waiting' && <Bang x={52} y={10} />}
        {pose === 'celebrate' && (
          <g className="pet-sparks" fill="#ffb02e">
            {EMBERS.map(([x, y], i) => (
              <circle key={i} className="pet-spark" cx={x} cy={y} r={i % 2 ? 1.3 : 1.8} style={{ animationDelay: `${i * 0.2}s` }} />
            ))}
          </g>
        )}
        {pose === 'sleeping' && <Zs x={46} y={16} />}
      </g>
    </svg>
  );
}
