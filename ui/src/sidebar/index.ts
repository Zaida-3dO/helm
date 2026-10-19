// The sidebar's slot exports: the sidebar for the app frame's left column.

export { Sidebar, SidebarView, type SidebarProps } from './Sidebar.tsx';
export { ChatMenu } from './ChatMenu.tsx';
export { ProjectTags } from './ProjectTags.tsx';
// For the shell's Ctrl+Tab / Ctrl+Shift+Tab: the chats the sidebar draws, in order.
export { stepVisibleChat, visibleOrder } from './rowOrder.ts';
export { stepChat, visibleChatOrder } from './model.ts';

// Settings → Projects registers itself on import.
import '../settings/projects/index.ts';
