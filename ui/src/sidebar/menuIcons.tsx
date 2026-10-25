// Icons for the chat and project menus (a 14 px icon leads each item). The
// item lists stay pure in menus.ts, which node tests load; this adds the icons in the render layer.

import type { ReactNode } from 'react';
import type { ProjectNode } from '../api/projects.ts';
import type { MenuItem } from '../ui/index.ts';
import {
  IconArchive,
  IconChevronDown,
  IconChevronRight,
  IconCompact,
  IconFolder,
  IconFolderTree,
  IconFork,
  IconMute,
  IconNest,
  IconNewChat,
  IconPalette,
  IconPin,
  IconRefresh,
  IconRename,
  IconRestore,
  IconSparkles,
  IconTrash,
  IconUnmute,
  IconUnnest,
  IconUnpin,
  IconUnread,
} from '../ui/icons.tsx';

const CHAT: Record<string, (label: string) => ReactNode> = {
  fork: () => <IconFork />,
  rename: () => <IconRename />,
  compact: () => <IconCompact />,
  rollover: () => <IconRefresh />,
  move: () => <IconFolder />,
  nest: () => <IconNest />,
  top: () => <IconUnnest />,
  children: (l) => (l.startsWith('Collapse') ? <IconChevronDown /> : <IconChevronRight />),
  unread: () => <IconUnread />,
  pin: (l) => (l.startsWith('Unpin') ? <IconUnpin /> : <IconPin />),
  mute: (l) => (l.startsWith('Unmute') ? <IconUnmute /> : <IconMute />),
  archive: () => <IconArchive />,
  restore: () => <IconRestore />,
  delete: () => <IconTrash />,
};

const PROJECT: Record<string, (label: string, p: ProjectNode) => ReactNode> = {
  new: () => <IconNewChat />,
  pin: (l) => (l.startsWith('Unpin') ? <IconUnpin /> : <IconPin />),
  rename: () => <IconRename />,
  'primary-folder': () => <IconFolder />,
  // "Change icon…" shows the project's own icon.
  icon: (_l, p) => (p.icon && !p.icon.startsWith('lib:') ? <span className="sb-menu-emoji">{p.icon}</span> : <IconSparkles />),
  nest: () => <IconFolderTree />,
  top: () => <IconUnnest />,
  archive: () => <IconArchive />,
  restore: () => <IconRestore />,
  colour: () => <IconPalette />,
};

const text = (l: ReactNode) => (typeof l === 'string' ? l : '');

/** The chat menu with its icons (top-level items; submenu rows stay plain). */
export function withChatIcons(items: MenuItem[]): MenuItem[] {
  return items.map((it) => (it.icon || !CHAT[it.id] ? it : { ...it, icon: CHAT[it.id]!(text(it.label)) }));
}

/** The project menu with its icons. */
export function withProjectIcons(items: MenuItem[], p: ProjectNode): MenuItem[] {
  return items.map((it) => (it.icon || !PROJECT[it.id] ? it : { ...it, icon: PROJECT[it.id]!(text(it.label), p) }));
}
