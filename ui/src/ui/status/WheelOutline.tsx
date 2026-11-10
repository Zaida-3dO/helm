// The wheel mark as bare strokes (rim, eight spokes with handles, hub), so a screen can draw it on
// by animating each stroke's dash. The geometry is WheelMark's; this file only changes how it is
// painted, and WheelMark itself is untouched. Every stroke has `pathLength="1"` and a class
// (`wo-rim`, `wo-spoke`, `wo-knob`, `wo-hub`, `wo-dome`), and each spoke carries its index in `--i`,
// so a stylesheet can stagger the drawing. Without one the outline simply shows.

import type { CSSProperties } from 'react';

export interface WheelOutlineProps {
  /** Box size in px (square). Default 160. */
  size?: number;
  className?: string;
}

const SPOKES = [0, 45, 90, 135, 180, 225, 270, 315];
const polar = (deg: number, r: number) => {
  const t = ((deg - 90) * Math.PI) / 180;
  return { x: 50 + r * Math.cos(t), y: 50 + r * Math.sin(t) };
};

export function WheelOutline({ size = 160, className }: WheelOutlineProps) {
  return (
    <span className={`ui-wheel-outline${className ? ` ${className}` : ''}`} style={{ width: size, height: size, display: 'inline-block' }} aria-hidden="true">
      <svg viewBox="0 0 100 100" width={size} height={size} focusable="false" fill="none" strokeLinecap="round">
        {SPOKES.map((a, i) => {
          const end = polar(a, 46);
          const knob = polar(a, 40);
          const style = { '--i': i } as CSSProperties;
          return (
            <g key={a} style={style}>
              <line className="wo-spoke" pathLength="1" x1="50" y1="50" x2={end.x} y2={end.y} stroke="#b4681f" strokeWidth="3" />
              <line className="wo-knob" pathLength="1" x1={knob.x} y1={knob.y} x2={end.x} y2={end.y} stroke="#cf812c" strokeWidth="6" />
            </g>
          );
        })}
        <circle className="wo-rim" pathLength="1" cx="50" cy="50" r="30" stroke="#a65d1c" strokeWidth="5" />
        <circle className="wo-hub" pathLength="1" cx="50" cy="50" r="13" stroke="#c99a38" strokeWidth="2.4" />
        <circle className="wo-dome" pathLength="1" cx="50" cy="50" r="7" stroke="#e3b450" strokeWidth="4" />
      </svg>
    </span>
  );
}
