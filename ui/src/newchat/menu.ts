// The project menu item the new-chat area provides for the sidebar's project menu (a seam: the sidebar
// places it, after Rename).

import type { ProjectNode } from '../api/projects.ts';
import type { MenuItem } from '../ui/Menu.tsx';
import { newChat } from './controller.ts';

/**
 * The project menu's "Primary folder…" item (placed after Rename). It
 * opens a dialog to choose or type the folder new chats in the project start in.
 */
export function primaryFolderMenuItem(project: ProjectNode): MenuItem {
  return {
    id: 'primary-folder',
    label: 'Primary folder…',
    disabled: project.archived,
    onSelect: () => newChat.openDialog({ kind: 'primary-folder', projectId: project.id }),
  };
}
