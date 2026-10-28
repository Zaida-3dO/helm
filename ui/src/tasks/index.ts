// The Tasks area's public surface. Other areas open Tasks and read whether it is available only
// through these.
//
//   openTasks(api)        open the Tasks tab (maximised), no item selected
//   openTasks(api, id)    open it with that item's drawer showing
//   tasksStoreFor(api)    the one store (the sidebar entry and Ctrl+K read it)
//   TasksEntry            the sidebar footer button; shown only once a board connection is saved
//
// The `tasks` panel kind itself registers from panel/index.ts (register.tsx).

export { TasksEntry } from './TasksEntry.tsx';
export { openTasks, tasksStoreFor } from './open.ts';
