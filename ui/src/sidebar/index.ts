// The sidebar's slot exports: the sidebar for the app frame's left column.

export { Sidebar, SidebarView, type SidebarProps } from './Sidebar.tsx';
export { ChatMenu } from './ChatMenu.tsx';
export { ProjectTags } from './ProjectTags.tsx';

// Settings → Projects registers itself on import.
import '../settings/projects/index.ts';
