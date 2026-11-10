// Pip, the puffin: a round dark body, a white face disc, a triangular beak in the accent colour
// and two orange feet. Drawn here, moved by pet.css.

import { Bang, Sparks, Zs, eyeState, type CreatureProps } from './shared.tsx';

const WARM = '#f08a24';

export function Puffin({ pose, blink }: CreatureProps) {
  const eyes = eyeState(pose, blink);
  return (
    <svg viewBox="0 0 64 64" className={`pet-svg pet-puffin pet-pose-${pose}`} aria-hidden="true" focusable="false">
      <g className="pet-all">
        <g className="pet-body-group">
          <g className="pet-wing pet-wing-l">
            <path d="M17 32 Q8 44 15 55 Q20 47 20 35 Z" fill="var(--pet-ink)" />
          </g>
          <g className="pet-wing pet-wing-r">
            <path d="M47 32 Q56 44 49 55 Q44 47 44 35 Z" fill="var(--pet-ink)" />
          </g>
          <ellipse cx="32" cy="38" rx="17" ry="19" fill="var(--pet-ink)" />
          <ellipse cx="32" cy="44" rx="11" ry="12.5" fill="var(--pet-light)" />
          <g className="pet-head">
            <ellipse cx="32" cy="26" rx="12.5" ry="11" fill="var(--pet-ink)" />
            <ellipse cx="32" cy="28" rx="10" ry="8" fill="var(--pet-light)" />
            <g className={`pet-eyes ${eyes}`}>
              <circle cx="27.5" cy="26.5" r="1.9" fill="var(--pet-ink)" />
              <circle cx="36.5" cy="26.5" r="1.9" fill="var(--pet-ink)" />
            </g>
            <polygon points="27,30 37,30 32,40.5" fill="var(--accent)" stroke="var(--pet-ink)" strokeWidth="0.8" strokeLinejoin="round" />
            <path d="M29 33.6 H35" stroke={WARM} strokeWidth="1.7" strokeLinecap="round" />
          </g>
        </g>
        <ellipse className="pet-foot pet-foot-l" cx="25" cy="58" rx="5.2" ry="2.2" fill={WARM} />
        <ellipse className="pet-foot pet-foot-r" cx="39" cy="58" rx="5.2" ry="2.2" fill={WARM} />
        {pose === 'waiting' && <Bang x={52} y={10} />}
        {pose === 'celebrate' && (
          <Sparks
            spots={[
              [10, 16],
              [54, 18],
              [32, 5],
              [6, 34],
              [58, 36],
              [20, 8],
            ]}
          />
        )}
        {pose === 'sleeping' && <Zs x={46} y={18} />}
      </g>
      {pose === 'failed' && (
        <g className="pet-cloud" fill="#9aa3ad">
          <circle cx="26" cy="9" r="4.2" />
          <circle cx="32" cy="7" r="5.2" />
          <circle cx="38.5" cy="9.5" r="4" />
          <rect x="24" y="9" width="17" height="5" rx="2.5" />
        </g>
      )}
    </svg>
  );
}
