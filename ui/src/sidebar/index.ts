// The sidebar's slot exports: the sidebar for the app frame's left column.
// The chat menu for the top bar's "…" follows with the menus.

export { Sidebar, SidebarView, type SidebarProps } from './Sidebar.tsx';

// Settings → Projects registers itself on import.
import '../settings/projects/index.ts';
