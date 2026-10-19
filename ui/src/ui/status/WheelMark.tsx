// The wheel mark: Helm's brand mark is a ship's wheel on its own, transparent,
// no tile, with a brass hub. This is the small in-app drawing, simplified from the app icon art
// (host/app/icons/source/helm-detailed.svg) so it reads at 20-40 px: eight turned spokes with
// handles, a wooden rim, brass studs and a domed hub. Still; it does not animate.

import { useId } from 'react';

export interface WheelMarkProps {
  /** Box size in px (square). Default 24; the sidebar beacon uses about 34. */
  size?: number;
  label?: string;
  className?: string;
}

const SPOKES = [0, 45, 90, 135, 180, 225, 270, 315];
const STUDS = SPOKES.map((a) => a + 22.5);
const polar = (deg: number, r: number) => {
  const t = ((deg - 90) * Math.PI) / 180;
  return { x: 50 + r * Math.cos(t), y: 50 + r * Math.sin(t) };
};

export function WheelMark({ size = 24, label, className }: WheelMarkProps) {
  const id = useId().replace(/:/g, '');
  const wood = `wm-wood-${id}`;
  const brass = `wm-brass-${id}`;
  const dome = `wm-dome-${id}`;
  return (
    <span
      className={`ui-wheel${className ? ` ${className}` : ''}`}
      style={{ width: size, height: size }}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <svg viewBox="0 0 100 100" width={size} height={size} focusable="false">
        <defs>
          <radialGradient id={wood} cx="50" cy="50" r="36" gradientUnits="userSpaceOnUse">
            <stop offset="0.7" stopColor="#5a2b09" />
            <stop offset="0.78" stopColor="#9a5518" />
            <stop offset="0.86" stopColor="#e6a04a" />
            <stop offset="0.93" stopColor="#a65d1c" />
            <stop offset="1" stopColor="#4a2207" />
          </radialGradient>
          <linearGradient id={brass} x1="0.2" y1="0" x2="0.8" y2="1">
            <stop offset="0" stopColor="#fff2b8" />
            <stop offset="0.35" stopColor="#e3b450" />
            <stop offset="0.7" stopColor="#a87420" />
            <stop offset="1" stopColor="#5e3d0c" />
          </linearGradient>
          <radialGradient id={dome} cx="0.5" cy="0.5" r="0.5" fx="0.36" fy="0.3">
            <stop offset="0" stopColor="#fff0c4" />
            <stop offset="0.25" stopColor="#eebd6c" />
            <stop offset="0.6" stopColor="#bb7c34" />
            <stop offset="1" stopColor="#5e3510" />
          </radialGradient>
        </defs>
        {/* Spokes, each ending in a turned handle beyond the rim. */}
        <g strokeLinecap="round">
          {SPOKES.map((a) => {
            const end = polar(a, 46);
            const knob = polar(a, 40);
            return (
              <g key={a}>
                <line x1="50" y1="50" x2={end.x} y2={end.y} stroke="#4a2207" strokeWidth="6.4" />
                <line x1="50" y1="50" x2={end.x} y2={end.y} stroke="#b4681f" strokeWidth="4.2" />
                <line x1={knob.x} y1={knob.y} x2={end.x} y2={end.y} stroke="#4a2207" strokeWidth="9.6" />
                <line x1={knob.x} y1={knob.y} x2={end.x} y2={end.y} stroke="#cf812c" strokeWidth="7.2" />
              </g>
            );
          })}
        </g>
        {/* The rim: a dark keyline either side of a turned wooden ring. */}
        <circle cx="50" cy="50" r="30" fill="none" stroke="#4a2207" strokeWidth="10" />
        <circle cx="50" cy="50" r="30" fill="none" stroke={`url(#${wood})`} strokeWidth="8" />
        {STUDS.map((a) => {
          const p = polar(a, 30);
          return <circle key={a} cx={p.x} cy={p.y} r="1.9" fill={`url(#${brass})`} stroke="#5e3d0c" strokeWidth="0.5" />;
        })}
        {/* The hub: a brass collar and a domed bronze cap. */}
        <circle cx="50" cy="50" r="13" fill={`url(#${brass})`} stroke="#5e3d0c" strokeWidth="1" />
        <circle cx="50" cy="50" r="8" fill={`url(#${dome})`} />
      </svg>
    </span>
  );
}
