// Small line icons for the transcript rows. The shared kit's Icon only holds
// the glyphs the kit itself needs, and the kit does not have them, so these live here.

import type { ReactNode } from 'react';

export type GlyphName =
  | 'chevron'
  | 'terminal'
  | 'file-code'
  | 'cable'
  | 'check'
  | 'alert'
  | 'pause'
  | 'settings'
  | 'chat'
  | 'project'
  | 'bell'
  | 'bookmark'
  | 'clock'
  | 'reload'
  | 'search'
  | 'globe'
  | 'list'
  | 'grip'
  | 'wrench'
  | 'arrow-down';

const PATHS: Record<GlyphName, ReactNode> = {
  chevron: <path d="m9 18 6-6-6-6" />,
  terminal: (
    <>
      <path d="m4 17 6-6-6-6" />
      <path d="M12 19h8" />
    </>
  ),
  'file-code': (
    <>
      <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
      <path d="M14 3v5h5" />
      <path d="m10 13-2 2 2 2M14 13l2 2-2 2" />
    </>
  ),
  cable: (
    <>
      <path d="M4 9a2 2 0 0 1-2-2V5h6v2a2 2 0 0 1-2 2zM3 5V3M7 5V3" />
      <path d="M19 15V6.5a3.5 3.5 0 0 0-7 0v11a3.5 3.5 0 0 1-7 0V9" />
      <path d="M17 21v-2M21 21v-2M22 19h-6v-2a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2z" />
    </>
  ),
  check: <path d="M20 6 9 17l-5-5" />,
  alert: (
    <>
      <circle cx="12" cy="12" r="10" />
      <path d="M12 8v4M12 16h.01" />
    </>
  ),
  pause: (
    <>
      <circle cx="12" cy="12" r="10" />
      <path d="M10 15V9M14 15V9" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2v3M12 19v3M4.9 4.9 7 7M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1 7 17M17 7l2.1-2.1" />
    </>
  ),
  chat: <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />,
  project: <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />,
  bell: <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.9 1.9 0 0 0 3.4 0" />,
  bookmark: <path d="m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" />,
  clock: (
    <>
      <circle cx="12" cy="12" r="10" />
      <path d="M12 6v6l4 2" />
    </>
  ),
  reload: <path d="M21 12a9 9 0 1 1-3-6.7L21 8M21 3v5h-5" />,
  search: (
    <>
      <circle cx="11" cy="11" r="8" />
      <path d="m21 21-4.3-4.3" />
    </>
  ),
  globe: (
    <>
      <circle cx="12" cy="12" r="10" />
      <path d="M2 12h20M12 2a15 15 0 0 1 0 20M12 2a15 15 0 0 0 0 20" />
    </>
  ),
  list: <path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01" />,
  grip: <path d="M9 5h.01M9 12h.01M9 19h.01M15 5h.01M15 12h.01M15 19h.01" strokeWidth="3" />,
  'arrow-down': <path d="M12 5v14M19 12l-7 7-7-7" />,
  wrench: <path d="M14.7 6.3a4 4 0 0 0 5 5L21 17l-4 4-5.7-1.3a4 4 0 0 0-5-5L3 7l4-4z" />,
};

export function Glyph({ name, size = 14, className }: { name: GlyphName; size?: number; className?: string }) {
  return (
    <svg
      className={`glyph${className ? ` ${className}` : ''}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {PATHS[name]}
    </svg>
  );
}

/** The 12 px turning ring shown on a streaming tool row. */
export function Spinner({ size = 12 }: { size?: number }) {
  return <span className="spinner" style={{ width: size, height: size }} aria-hidden="true" />;
}
