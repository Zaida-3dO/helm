// The completion star, still. Marks an unread result. There is no turning
// corona, counter-orbit or hover glint; this is the prism core over a dim aura, frozen at
// its starting angle. Drawn on a 26 x 26 field and allowed to overflow its box a little.

import { useId } from 'react';
import { polygonPath, starPoints } from './geometry.ts';

export interface StarMarkProps {
  /** Box size in px. Default 26 (a sidebar row's indicator field). */
  size?: number;
  label?: string;
  className?: string;
}

const C = 13;
/** The eight-point face: long tips on the axes, shorter on the diagonals. */
const FACE_PTS = starPoints(C, C, 8, [7, 4.6], 2.1, -18);
const FACE_D = polygonPath(FACE_PTS);
/** Facet lines from the centre to each tip. */
const FACETS = FACE_PTS.filter((_, i) => i % 2 === 0)
  .map((p) => `M${C} ${C}L${p.x.toFixed(2)} ${p.y.toFixed(2)}`)
  .join('');
/** Rays between the tips, reaching past the face. */
const RAYS = starPoints(C, C, 8, [10.5], 0, 4.5)
  .filter((_, i) => i % 2 === 0)
  .map((p) => `M${C} ${C}L${p.x.toFixed(2)} ${p.y.toFixed(2)}`)
  .join('');
const CORE_D = polygonPath(starPoints(C, C, 4, [2.4], 0.7, -18));

export function StarMark({ size = 26, label, className }: StarMarkProps) {
  const id = useId().replace(/:/g, '');
  const aura = `sm-aura-${id}`;
  return (
    <span
      className={`ui-star${className ? ` ${className}` : ''}`}
      style={{ width: size, height: size }}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <svg viewBox="0 0 26 26" width={size} height={size} overflow="visible" focusable="false">
        <defs>
          <radialGradient id={aura}>
            <stop offset="0" stopColor="#f4fcff" />
            <stop offset="0.32" className="ui-star-ice" stopOpacity="0.96" />
            <stop offset="0.64" className="ui-star-violet" stopOpacity="0.68" />
            <stop offset="1" className="ui-star-violet" stopOpacity="0" />
          </radialGradient>
        </defs>
        <circle className="ui-star-aura" cx={C} cy={C} r="12" fill={`url(#${aura})`} />
        <path className="ui-star-rays" d={RAYS} />
        <path className="ui-star-face" d={FACE_D} />
        <path className="ui-star-facets" d={FACETS} />
        <path className="ui-star-core" d={CORE_D} />
      </svg>
    </span>
  );
}
