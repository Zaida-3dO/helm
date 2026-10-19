// The working face, still. A faceted robot head that says "this chat is working".
// No animation here. Everything below
// is the static drawing: the head with its features cut through by a mask, the facet shade, the
// volume light, the glow behind the cut-outs and the antenna ball.
//
// Sizes: `large` is the activity dock's 72 x 48 field (face box 72 x 34); `compact` is a sidebar
// row's 26 x 26 field (face box 26 x 20, drawn at 90%).

import { useId } from 'react';
import { FACE, glintPaths, gumPath, headPath, lipPath, teethPath } from './geometry.ts';

export type FaceState = 'working' | 'stopping' | 'calm';

export interface WorkingFaceProps {
  size?: 'large' | 'compact';
  /** stopping dims to .55; calm (background work) to .6. */
  state?: FaceState;
  /** An accessible name; without one the face is decorative. */
  label?: string;
  className?: string;
}

const HEAD = headPath();
const LIP = lipPath();
const GUM = gumPath();
const TEETH = teethPath();
const GLINTS = glintPaths();

export function WorkingFace({ size = 'compact', state = 'working', label, className }: WorkingFaceProps) {
  const id = useId().replace(/:/g, '');
  const cut = `wf-cut-${id}`;
  const lipClip = `wf-lip-${id}`;
  const shape = `wf-shape-${id}`;
  const vol = `wf-vol-${id}`;
  const { stem, ball } = FACE;
  return (
    <span
      className={`ui-face ui-face-${size} ui-face-${state}${className ? ` ${className}` : ''}`}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      data-face-state={state}
    >
      <svg className="ui-face-svg" viewBox={FACE.faceWindow} preserveAspectRatio="xMidYMid meet" focusable="false">
        <defs>
          <clipPath id={lipClip}>
            <path d={LIP} />
          </clipPath>
          <clipPath id={shape}>
            <path d={HEAD} />
            <rect x={stem.x} y={stem.y} width={stem.w} height={stem.h} rx={stem.r} />
          </clipPath>
          <radialGradient id={vol} cx="0.36" cy="0.3" r="0.85" gradientUnits="objectBoundingBox">
            <stop offset="0" className="ui-face-vol-hi" />
            <stop offset="0.55" stopColor="#fff" stopOpacity="0" />
            <stop offset="1" stopColor="#000" stopOpacity="0.3" />
          </radialGradient>
          <mask id={cut} maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="100">
            <rect width="100" height="100" fill="#fff" />
            {FACE.eyes.map((e, i) => (
              <ellipse key={`e${i}`} cx={e.cx} cy={e.cy} rx={FACE.eye.rx} ry={FACE.eye.ry} fill="#000" />
            ))}
            {FACE.cheeks.map((c, i) => (
              <ellipse key={`c${i}`} cx={c.cx} cy={c.cy} rx={FACE.cheek.rx} ry={FACE.cheek.ry} fill="#fff" />
            ))}
            {GLINTS.map((d, i) => (
              <path key={`g${i}`} d={d} fill="#fff" />
            ))}
            <path d={LIP} fill="none" stroke="#000" strokeWidth="2.3" strokeLinecap="round" strokeLinejoin="round" />
            <g clipPath={`url(#${lipClip})`} fill="none" stroke="#000">
              <path d={GUM} strokeWidth="5" strokeLinecap="round" />
              <path d={TEETH} strokeWidth="1.9" strokeLinecap="round" />
            </g>
          </mask>
        </defs>
        {/* The glow behind the cut-outs: it shows through the eyes and the grin. */}
        <g className="ui-face-glow">
          {FACE.eyes.map((e, i) => (
            <ellipse key={i} cx={e.cx} cy={e.cy} rx={FACE.eye.rx} ry={FACE.eye.ry} />
          ))}
          <path d={LIP} strokeWidth="3.5" strokeLinejoin="round" />
        </g>
        <g mask={`url(#${cut})`}>
          <g className="ui-face-head">
            <path d={HEAD} />
            <rect x={stem.x} y={stem.y} width={stem.w} height={stem.h} rx={stem.r} />
          </g>
          <g clipPath={`url(#${shape})`}>
            <rect className="ui-face-facet-dark" x="50" y="0" width="37" height="100" />
            <rect className="ui-face-facet-light" x="13" y="0" width="37" height="100" />
            <rect className="ui-face-vol" x="13" y="24" width="74" height="66" fill={`url(#${vol})`} />
          </g>
        </g>
        <circle className="ui-face-ball" cx={ball.cx} cy={ball.cy} r={ball.r} />
      </svg>
    </span>
  );
}
