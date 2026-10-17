// Line icons for the side panel, drawn on a 24-unit grid in the current text colour.

import type { ReactNode } from 'react';

function Icon({ size, children, label }: { size: number; children: ReactNode; label?: string }) {
  return (
    <svg
      className="sp-icon"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden={label ? undefined : true}
      aria-label={label}
      role={label ? 'img' : undefined}
    >
      {children}
    </svg>
  );
}

type P = { size?: number };

export const FileTextIcon = ({ size = 13 }: P) => (
  <Icon size={size}>
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
    <path d="M14 3v5h5M9 13h6M9 17h6M9 9h2" />
  </Icon>
);

export const FolderOpenIcon = ({ size = 13 }: P) => (
  <Icon size={size}>
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v1" />
    <path d="M3 7v11a1 1 0 0 0 1 1h13.5a1 1 0 0 0 .95-.68L21 11H7.5a1 1 0 0 0-.95.68L4 19" />
  </Icon>
);

export const FileDiffIcon = ({ size = 13 }: P) => (
  <Icon size={size}>
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
    <path d="M12 9v6M9 12h6M9 18h6" />
  </Icon>
);

export const FolderTreeIcon = ({ size = 13 }: P) => (
  <Icon size={size}>
    <path d="M4 4v13a1 1 0 0 0 1 1h4M4 9h5" />
    <rect x="11" y="6" width="9" height="6" rx="1.5" />
    <rect x="11" y="15" width="9" height="6" rx="1.5" />
  </Icon>
);

export const NoteIcon = ({ size = 13 }: P) => (
  <Icon size={size}>
    <path d="M6 3h9l4 4v14H6z" />
    <path d="M9 11h7M9 15h5" />
  </Icon>
);

export const PlusIcon = ({ size = 15 }: P) => (
  <Icon size={size}>
    <path d="M12 5v14M5 12h14" />
  </Icon>
);

export const MaximizeIcon = ({ size = 14 }: P) => (
  <Icon size={size}>
    <path d="M14 4h6v6M10 20H4v-6M20 4l-7 7M4 20l7-7" />
  </Icon>
);

export const RestoreIcon = ({ size = 14 }: P) => (
  <Icon size={size}>
    <path d="M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7" />
  </Icon>
);

export const CloseIcon = ({ size = 15 }: P) => (
  <Icon size={size}>
    <path d="M6 6l12 12M18 6L6 18" />
  </Icon>
);

export const QuestionIcon = ({ size = 13 }: P) => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="9" />
    <path d="M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.6.3-1 .9-1 1.6V14M12 17.5v.01" />
  </Icon>
);
