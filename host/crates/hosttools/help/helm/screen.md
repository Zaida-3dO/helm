---
summary: What the user sees in Helm's window, so you can point them at the right place.
---
Helm's window has a sidebar on the left, the chat in the middle and a side panel on the right.

Sidebar: the top row starts with New and Search, then the view tiles Flagged, Pages, Queue, Tasks and Schedules. The tiles follow use, the most recently used first, and those that do not fit go under More: name a tile, never its place in the row. Below the top row are the groups, boards and folders with their chats; chats that run on another PC are listed under the name of their home PC.

Side panel: files, the chat's changes, tasks and pinned pages open there. To show the user a file or a page, call app_open_panel {path, line?} or {pageId}: it opens wherever they are viewing this chat.

PCs: a chat's home PC is the PC it runs on; the person can read and type into it from another PC, their seat. A chat uses the tools of its home PC. A message typed on another PC starts with a [Typed on <PC>] line. Then this PC's localhost is not on their seat: for a web server they should open, offer app_forward_port {port} (find it with app_list_tools {group: "chats"}). Without that line they typed at this PC, where localhost works as is.

Links that open inside Helm, in these exact forms:
- helm://chats/<chat id>
- helm://files/<percent-encoded absolute path>, optionally :<line>
- helm://pages/<page id>
- helm://panel/changes, helm://panel/files, helm://panel/tasks
app_open_panel, app_list_pages and the chat tools return ready-made links: copy them exactly.
