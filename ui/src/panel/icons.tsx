// The side panel's icons: the kit's Lucide set (ui/icons.tsx) under the names
// and default sizes the panel has always used, so every caller keeps its import. Large glyphs (the
// 25 px empty-state icons) draw a lighter stroke.

import type { ComponentType } from 'react';
import {
  IconFileDiff,
  IconFileText,
  IconFolderOpen,
  IconFolderTree,
  IconHelp,
  IconMaximize,
  IconMinimize,
  IconNote,
  IconPlus,
  IconX,
  type IconProps,
} from '../ui/icons.tsx';

type P = { size?: number };

function panelIcon(Glyph: ComponentType<IconProps>, fallback: number) {
  return function PanelIcon({ size = fallback }: P) {
    return <Glyph size={size} strokeWidth={size >= 20 ? 1.7 : 2} className="sp-icon" />;
  };
}

export const FileTextIcon = panelIcon(IconFileText, 13);
export const FolderOpenIcon = panelIcon(IconFolderOpen, 13);
export const FileDiffIcon = panelIcon(IconFileDiff, 13);
export const FolderTreeIcon = panelIcon(IconFolderTree, 13);
export const NoteIcon = panelIcon(IconNote, 13);
export const PlusIcon = panelIcon(IconPlus, 15);
export const MaximizeIcon = panelIcon(IconMaximize, 14);
export const RestoreIcon = panelIcon(IconMinimize, 14);
export const CloseIcon = panelIcon(IconX, 15);
export const QuestionIcon = panelIcon(IconHelp, 13);
