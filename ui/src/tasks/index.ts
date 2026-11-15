// The Tasks area's public surface. Other areas open Tasks and read whether it is available only
// through these.
//
//   openTasks(api)        open the Tasks tab (maximised), no item selected
//   openTasks(api, id)    open it with that item's drawer showing
//   openTasks(api, id, remoteChatId)   …reading the board of the PC that runs a mirrored chat
//   tasksStoreFor(api)    this PC's store; with a chat id, that chat's PC's store (the sidebar
//                         entry and Ctrl+K read it)
//   useTasksEntry         whether the board is connected, and how to open Tasks (the sidebar's tile)
//
// The `tasks` panel kind itself registers from panel/index.ts (register.tsx).

export { useTasksEntry } from './TasksEntry.tsx';
export { openTasks, tasksStoreFor } from './open.ts';
