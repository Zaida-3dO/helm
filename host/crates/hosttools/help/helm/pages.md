---
summary: How a pinned page is built, what it can do and how to change it.
---
A pinned page is one local .html file that runs its own scripts in a sealed frame in the side panel (inline CSS and JS; images, styles and scripts beside it are served from its folder). It has no cookies or localStorage, and it reaches the network only when it is pinned for the profile that opens it: an unpinned page is offline.

It gets a `helm` object:
- `await helm.ready`
- `helm.state` and `await helm.setState(json)`: up to 256 KiB, kept across restarts; use it instead of localStorage
- `helm.theme`, `helm.on('themechange', fn)` and the CSS variables --helm-bg, --helm-text, --helm-surface, --helm-muted, --helm-accent, --helm-border
- `await helm.sendMessage({text, data?, title?})`: asks the user to confirm, then posts the text into this chat as a message from the page, never as the user's own words
- `helm.openLink(url)`

To change a page, rewrite its file: the open tab reloads and keeps its saved state. A page cannot start work by itself. Pin one with app_pages {action: "pin", path, title?}; show it to the user with app_open_panel {pageId}. Each page has a link, helm://pages/<id>, that opens it from a reply.
