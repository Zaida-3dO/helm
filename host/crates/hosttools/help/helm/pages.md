---
summary: How a pinned page is built, what it can do and how to change it.
---
A pinned page is one local .html file that runs its own scripts in a sealed frame in the side panel (inline CSS and JS; images, styles and scripts beside it are served from its folder). It has no cookies or localStorage, and it reaches the network only when it is pinned for the profile that opens it: an unpinned page is offline.

It gets a `helm` object:
- `await helm.ready`
- `helm.state` and `await helm.setState(json)`: up to 256 KiB, kept across restarts; use it instead of localStorage
- `helm.theme`, `helm.on('themechange', fn)` and the CSS variables --helm-bg, --helm-text, --helm-surface, --helm-muted, --helm-accent, --helm-border
- `await helm.sendMessage({text, overview?, title?, data?})`: posts the text straight into this chat, no confirmation. The chat shows a card naming the page with `title` and summarising with `overview` (without them the card uses the page's title and the text's first lines); the agent receives `text` with a line naming the page as its author, never the user's own words.
- `helm.openLink(url)`

To change a page, rewrite its file: the open tab reloads and keeps its saved state. A page cannot start work by itself. Pin one with app_pages {action: "pin", path, title?}; show it to the user with app_open_panel {pageId}. Each page has a link, helm://pages/<id>, that opens it from a reply.
