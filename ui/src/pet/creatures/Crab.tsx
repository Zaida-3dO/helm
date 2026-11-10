// Dock, the hermit crab: a reddish body with two eye stalks, carrying a small ship's wheel for a
// shell (eight spokes, in the wood and brass of the app's wheel mark). Drawn here, moved by pet.css.

import { Bang, Sparks, Zs, eyeState, type CreatureProps } from './shared.tsx';

const SHELL = { x: 38, y: 31 };
const SPOKES = [0, 45, 90, 135, 180, 225, 270, 315];
const at = (deg: number, r: number) => {
  const t = ((deg - 90) * Math.PI) / 180;
  return { x: SHELL.x + r * Math.cos(t), y: SHELL.y + r * Math.sin(t) };
};
const RED = '#c8553d';
const RED_LIGHT = '#dd7358';

export function Crab({ pose, blink }: CreatureProps) {
  const eyes = eyeState(pose, blink);
  return (
    <svg viewBox="0 0 64 64" className={`pet-svg pet-crab pet-pose-${pose}`} aria-hidden="true" focusable="false">
      <g className="pet-all">
        <g className="pet-shell">
          <g className="pet-wheel">
            {SPOKES.map((a) => {
              const end = at(a, 19);
              const knob = at(a, 15.5);
              return (
                <g key={a} strokeLinecap="round">
                  <line x1={SHELL.x} y1={SHELL.y} x2={end.x} y2={end.y} stroke="#4a2207" strokeWidth="3.4" />
                  <line x1={SHELL.x} y1={SHELL.y} x2={end.x} y2={end.y} stroke="#b4681f" strokeWidth="2" />
                  <line x1={knob.x} y1={knob.y} x2={end.x} y2={end.y} stroke="#4a2207" strokeWidth="5" />
                  <line x1={knob.x} y1={knob.y} x2={end.x} y2={end.y} stroke="#cf812c" strokeWidth="3.4" />
                </g>
              );
            })}
            <circle cx={SHELL.x} cy={SHELL.y} r="14" fill="none" stroke="#4a2207" strokeWidth="5.4" />
            <circle cx={SHELL.x} cy={SHELL.y} r="14" fill="none" stroke="#a65d1c" strokeWidth="3.6" />
            <circle cx={SHELL.x} cy={SHELL.y} r="6.4" fill="#e3b450" stroke="#5e3d0c" strokeWidth="0.7" />
            <circle cx={SHELL.x} cy={SHELL.y} r="3.8" fill="#eebd6c" />
          </g>
        </g>
        <g className="pet-crab-body">
          <g className="pet-legs" stroke={RED} strokeWidth="2" strokeLinecap="round" fill="none">
            <path className="pet-leg pet-leg-1" d="M13 51 L9 58" />
            <path className="pet-leg pet-leg-2" d="M19 53 L16 60" />
            <path className="pet-leg pet-leg-3" d="M26 54 L25 61" />
            <path className="pet-leg pet-leg-4" d="M33 53 L35 60" />
          </g>
          <ellipse cx="23" cy="47" rx="15" ry="8.6" fill={RED} />
          <ellipse cx="22" cy="49.5" rx="10" ry="4.6" fill={RED_LIGHT} />
          <g className="pet-claw pet-claw-l">
            <path d="M12 46 L7 43" stroke={RED} strokeWidth="2.6" strokeLinecap="round" />
            <circle cx="6.2" cy="41" r="4.6" fill={RED_LIGHT} />
            <path d="M6.2 41 L3.6 36.6" stroke={RED} strokeWidth="1.5" strokeLinecap="round" />
          </g>
          <g className="pet-stalks">
            <g stroke={RED} strokeWidth="2.2" strokeLinecap="round">
              <line x1="19" y1="41" x2="17.5" y2="31" />
              <line x1="26" y1="41" x2="27.5" y2="30" />
            </g>
            <g className={`pet-eyes ${eyes}`}>
              <circle cx="17.5" cy="29.5" r="3.1" fill="#fff" />
              <circle cx="17.5" cy="29.8" r="1.5" fill="var(--pet-ink)" />
              <circle cx="27.5" cy="28.5" r="3.1" fill="#fff" />
              <circle cx="27.5" cy="28.8" r="1.5" fill="var(--pet-ink)" />
            </g>
          </g>
        </g>
        {pose === 'waiting' && <Bang x={9} y={14} />}
        {pose === 'celebrate' && (
          <Sparks
            spots={[
              [8, 22],
              [58, 14],
              [32, 6],
              [4, 40],
              [60, 40],
              [48, 4],
            ]}
          />
        )}
        {pose === 'sleeping' && <Zs x={10} y={26} />}
      </g>
      {pose === 'failed' && (
        <g className="pet-cloud" fill="#9aa3ad">
          <circle cx="12" cy="14" r="3.6" />
          <circle cx="17" cy="12" r="4.4" />
          <circle cx="22.5" cy="14.5" r="3.4" />
          <rect x="10" y="14" width="15" height="4.4" rx="2.2" />
        </g>
      )}
    </svg>
  );
}
