// The project icon library: 48 line icons drawn here, on a 24 x 24 grid with one stroke style. A
// project stores one as `lib:<name>`. Each entry below holds just the shapes; `LibraryIcon` wraps them.

import type { ReactNode } from 'react';

export interface LibraryEntry {
  /** The stored key, after `lib:`. */
  name: string;
  /** What the picker calls it (tooltip and screen readers). */
  label: string;
  paths: ReactNode;
}

export const ICON_LIBRARY: LibraryEntry[] = [
  {
    name: 'anchor',
    label: 'Anchor',
    paths: (
      <>
        <circle cx="12" cy="5" r="2.5" />
        <path d="M12 7.5V21M7 11h10M4 13a8 8 0 0 0 16 0" />
      </>
    ),
  },
  {
    name: 'compass',
    label: 'Compass',
    paths: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M15.5 8.5l-2 5-5 2 2-5z" />
      </>
    ),
  },
  {
    name: 'book',
    label: 'Book',
    paths: <path d="M5 4h14v14H6.5A1.5 1.5 0 0 0 5 19.5zM5 19.5A1.5 1.5 0 0 0 6.5 21H19v-3M9 8h6" />,
  },
  {
    name: 'briefcase',
    label: 'Briefcase',
    paths: (
      <>
        <rect x="3" y="7" width="18" height="13" rx="2" />
        <path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2M3 13h18" />
      </>
    ),
  },
  {
    name: 'code',
    label: 'Code',
    paths: <path d="M8 7l-5 5 5 5M16 7l5 5-5 5M13.5 5l-3 14" />,
  },
  {
    name: 'terminal',
    label: 'Terminal',
    paths: (
      <>
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M7 9l3 3-3 3M13 15h4" />
      </>
    ),
  },
  {
    name: 'bug',
    label: 'Bug',
    paths: <path d="M8 9a4 4 0 0 1 8 0v6a4 4 0 0 1-8 0zM12 9v10M4 9l3 2M20 9l-3 2M3 14h4M17 14h4M4 20l3-2.5M20 20l-3-2.5M9.5 5.5L8 3M14.5 5.5L16 3" />,
  },
  {
    name: 'flask',
    label: 'Flask',
    paths: <path d="M9 3h6M10 3v6l-5.5 9.5A2 2 0 0 0 6.2 21.5h11.6a2 2 0 0 0 1.7-3L14 9V3M7.5 15h9" />,
  },
  {
    name: 'leaf',
    label: 'Leaf',
    paths: <path d="M20 4C10 4 5 9 5 15a5 5 0 0 0 5 5c6 0 10-6 10-16zM5 20c3-5 6-8 10-10" />,
  },
  {
    name: 'house',
    label: 'House',
    paths: <path d="M3 11l9-8 9 8M5 9.5V20h14V9.5M10 20v-6h4v6" />,
  },
  {
    name: 'car',
    label: 'Car',
    paths: (
      <>
        <path d="M5.5 17H3v-5l2-5h14l2 5v5h-2.5M3 12h18M10 17h4" />
        <circle cx="8" cy="17" r="2" />
        <circle cx="16" cy="17" r="2" />
      </>
    ),
  },
  {
    name: 'money',
    label: 'Money',
    paths: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M14.5 9.2c-.5-.8-1.4-1.2-2.5-1.2-1.5 0-2.5.8-2.5 2s1 1.6 2.5 2 2.5.8 2.5 2-1 2-2.5 2c-1.1 0-2-.4-2.5-1.2M12 6.5V8M12 16v1.5" />
      </>
    ),
  },
  {
    name: 'chart',
    label: 'Chart',
    paths: <path d="M4 4v16h16M8 16v-4M12 16V8M16 16v-6" />,
  },
  {
    name: 'camera',
    label: 'Camera',
    paths: (
      <>
        <path d="M4 7h3l1.5-2h7L17 7h3a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1z" />
        <circle cx="12" cy="13" r="3.5" />
      </>
    ),
  },
  {
    name: 'music',
    label: 'Music',
    paths: (
      <>
        <path d="M9 18V5l11-2v13M9 9l11-2" />
        <circle cx="6.5" cy="18" r="2.5" />
        <circle cx="17.5" cy="16" r="2.5" />
      </>
    ),
  },
  {
    name: 'heart',
    label: 'Heart',
    paths: <path d="M12 20s-8-5-8-11a4.5 4.5 0 0 1 8-2.8A4.5 4.5 0 0 1 20 9c0 6-8 11-8 11z" />,
  },
  {
    name: 'star',
    label: 'Star',
    paths: <path d="M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.9L12 16.9l-5.2 2.8 1-5.9-4.3-4.1 5.9-.8z" />,
  },
  {
    name: 'globe',
    label: 'Globe',
    paths: (
      <>
        <circle cx="12" cy="12" r="9" />
        <ellipse cx="12" cy="12" rx="4" ry="9" />
        <path d="M3 12h18" />
      </>
    ),
  },
  {
    name: 'map',
    label: 'Map',
    paths: <path d="M3 6l6-2 6 2 6-2v14l-6 2-6-2-6 2zM9 4v14M15 6v14" />,
  },
  {
    name: 'mail',
    label: 'Mail',
    paths: (
      <>
        <rect x="3" y="5" width="18" height="14" rx="2" />
        <path d="M3.5 7l8.5 6 8.5-6" />
      </>
    ),
  },
  {
    name: 'chat',
    label: 'Chat',
    paths: <path d="M4 5h16a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1h-8l-5 4v-4H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z" />,
  },
  {
    name: 'calendar',
    label: 'Calendar',
    paths: (
      <>
        <rect x="3" y="5" width="18" height="16" rx="2" />
        <path d="M3 10h18M8 3v4M16 3v4" />
      </>
    ),
  },
  {
    name: 'clock',
    label: 'Clock',
    paths: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l3.5 2" />
      </>
    ),
  },
  {
    name: 'gift',
    label: 'Gift',
    paths: (
      <>
        <rect x="3" y="8" width="18" height="4" rx="1" />
        <path d="M5 12v8h14v-8M12 8v12M12 8C10 8 7 8 7 6.5S10.5 4 12 8zM12 8c2 0 5 0 5-1.5S13.5 4 12 8z" />
      </>
    ),
  },
  {
    name: 'cart',
    label: 'Cart',
    paths: (
      <>
        <path d="M3 4h2.5l2 11h10l2-8H7" />
        <circle cx="9" cy="19.5" r="1.3" />
        <circle cx="16.5" cy="19.5" r="1.3" />
      </>
    ),
  },
  {
    name: 'tool',
    label: 'Wrench',
    paths: <path d="M14.5 3.5a5 5 0 0 0-4.6 6.9L4 16.3a2.1 2.1 0 0 0 3 3l5.9-5.9A5 5 0 0 0 20.5 9.5l-3 3-3-.5-.5-3 3-3z" />,
  },
  {
    name: 'gear',
    label: 'Gear',
    paths: (
      <>
        <path d="M10.5 3h3l.6 2.4 1.7.7 2.1-1.3 2.1 2.1-1.3 2.1.7 1.7 2.4.6v3l-2.4.6-.7 1.7 1.3 2.1-2.1 2.1-2.1-1.3-1.7.7-.6 2.4h-3l-.6-2.4-1.7-.7-2.1 1.3-2.1-2.1 1.3-2.1-.7-1.7L3 13.5v-3l2.4-.6.7-1.7-1.3-2.1 2.1-2.1 2.1 1.3 1.7-.7z" />
        <circle cx="12" cy="12" r="3" />
      </>
    ),
  },
  {
    name: 'shield',
    label: 'Shield',
    paths: <path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6zM8.5 12l2.5 2.5 4.5-5" />,
  },
  {
    name: 'key',
    label: 'Key',
    paths: (
      <>
        <circle cx="8" cy="15" r="4" />
        <path d="M10.8 12.2L20 3M17 6l3 3M14.5 8.5l2 2" />
      </>
    ),
  },
  {
    name: 'lock',
    label: 'Lock',
    paths: (
      <>
        <rect x="5" y="11" width="14" height="10" rx="2" />
        <path d="M8 11V8a4 4 0 0 1 8 0v3M12 15v2" />
      </>
    ),
  },
  {
    name: 'cloud',
    label: 'Cloud',
    paths: <path d="M7.5 19a4.5 4.5 0 0 1-.4-9A6 6 0 0 1 18.5 11.5a3.8 3.8 0 0 1-.5 7.5z" />,
  },
  {
    name: 'server',
    label: 'Server',
    paths: (
      <>
        <rect x="3" y="4" width="18" height="7" rx="2" />
        <rect x="3" y="13" width="18" height="7" rx="2" />
        <path d="M7 7.5h.01M7 16.5h.01" />
      </>
    ),
  },
  {
    name: 'database',
    label: 'Database',
    paths: (
      <>
        <ellipse cx="12" cy="6" rx="8" ry="3" />
        <path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3" />
      </>
    ),
  },
  {
    name: 'phone',
    label: 'Phone',
    paths: (
      <>
        <rect x="7" y="2.5" width="10" height="19" rx="2" />
        <path d="M11 18.5h2" />
      </>
    ),
  },
  {
    name: 'laptop',
    label: 'Laptop',
    paths: (
      <>
        <rect x="5" y="5" width="14" height="10" rx="1.5" />
        <path d="M2 19h20" />
      </>
    ),
  },
  {
    name: 'game',
    label: 'Game controller',
    paths: <path d="M7 8h10a5 5 0 0 1 5 5v1a3 3 0 0 1-5.3 1.9L15 14H9l-1.7 1.9A3 3 0 0 1 2 14v-1a5 5 0 0 1 5-5zM7 11v3M5.5 12.5h3M16 11.5h.01M18 13.5h.01" />,
  },
  {
    name: 'paint',
    label: 'Paint palette',
    paths: <path d="M12 3a9 9 0 1 0 0 18c1.2 0 2-.8 2-1.8 0-.5-.2-.9-.5-1.3-.3-.4-.5-.8-.5-1.3 0-1 .8-1.8 1.8-1.8H17a4 4 0 0 0 4-4C21 6.4 17 3 12 3zM7.5 11h.01M10 7.5h.01M14.5 7.5h.01" />,
  },
  {
    name: 'pen',
    label: 'Pen',
    paths: <path d="M4 20l1.2-4.3L16.5 4.4a2.1 2.1 0 0 1 3 3L8.3 18.8zM14.5 6.5l3 3" />,
  },
  {
    name: 'puzzle',
    label: 'Puzzle piece',
    paths: <path d="M10 5V4a2 2 0 0 1 4 0v1h4v4h-1a2 2 0 0 0 0 4h1v6H4v-5h1a2 2 0 0 0 0-4H4V5z" />,
  },
  {
    name: 'rocket',
    label: 'Rocket',
    paths: (
      <>
        <path d="M12 3c3 2 5 5.5 5 9v3l-2 2H9l-2-2v-3c0-3.5 2-7 5-9zM7 13l-3 3v3l3-2M17 13l3 3v3l-3-2M10.5 20.5L12 22l1.5-1.5" />
        <circle cx="12" cy="10" r="1.8" />
      </>
    ),
  },
  {
    name: 'school',
    label: 'Graduation cap',
    paths: <path d="M2 9l10-5 10 5-10 5zM6 11.5V16c0 1.5 2.7 3 6 3s6-1.5 6-3v-4.5M22 9v6" />,
  },
  {
    name: 'baby',
    label: 'Baby',
    paths: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M9 11h.01M15 11h.01M9 15c1 1 2 1.5 3 1.5s2-.5 3-1.5M12 3.2c1 .8 1 2 0 3" />
      </>
    ),
  },
  {
    name: 'dog',
    label: 'Dog',
    paths: <path d="M6 8c0-2.5 2.5-4 6-4s6 1.5 6 4v5c0 4-2.7 7-6 7s-6-3-6-7zM6 8L3 11l1 5M18 8l3 3-1 5M9.5 11h.01M14.5 11h.01M10.5 15h3l-1.5 1.5z" />,
  },
  {
    name: 'plant',
    label: 'Plant',
    paths: <path d="M6.5 15h11l-1 6h-9zM12 15V9M12 11c0-3-2-5-6-5 0 3 2 5 6 5zM12 9c0-3 2-5 6-5 0 3-2 5-6 5z" />,
  },
  {
    name: 'sun',
    label: 'Sun',
    paths: (
      <>
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2.5V5M12 19v2.5M2.5 12H5M19 12h2.5M5.3 5.3l1.8 1.8M16.9 16.9l1.8 1.8M5.3 18.7l1.8-1.8M16.9 7.1l1.8-1.8" />
      </>
    ),
  },
  {
    name: 'moon',
    label: 'Moon',
    paths: <path d="M20 14.5A8.5 8.5 0 1 1 9.5 4a7 7 0 0 0 10.5 10.5z" />,
  },
  {
    name: 'flag',
    label: 'Flag',
    paths: <path d="M5 21V4M5 4h12l-2.5 4 2.5 4H5" />,
  },
  {
    name: 'sparkle',
    label: 'Sparkle',
    paths: <path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8zM18.5 15.5v4M16.5 17.5h4M5 17v3M3.5 18.5h3" />,
  },
];

export function isLibraryName(n: string | null | undefined): boolean {
  return !!n && ICON_LIBRARY.some((e) => e.name === n);
}

/** One library icon by name. An unknown name draws nothing. */
export function LibraryIcon({ name, size = 16 }: { name: string; size?: number }) {
  const entry = ICON_LIBRARY.find((e) => e.name === name);
  if (!entry) return null;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      data-lib-icon={name}
    >
      {entry.paths}
    </svg>
  );
}
