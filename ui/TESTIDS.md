# Stable test hooks (`data-testid`)

The end-to-end checks (`checks/e2e`) target only these hooks. Renaming or removing one
breaks those checks, so change them only together with the checks. Everything else in the DOM
(classes, text, structure) may change freely.

| Hook | Element | Attributes |
|---|---|---|
| `new-chat` | "New chat" button in the sidebar | |
| `folder-input` | Folder path input beside it (the last value is remembered) | |
| `chat-item` | One chat row in the sidebar | `data-chat-id`, `data-status`, `data-depth` (chat nesting 0–2), `data-group` (the project it is shown under) |
| `sidebar-group` | One project header in the sidebar | `data-project-id`, `data-kind` (`folder` `board`), `data-depth` (project nesting 0–2) |
| `board-sync` | The Agent Standup sync line under the view switch | `data-state` (`never` `unconfigured` `syncing` `ok` `error`) |
| `board-url`, `board-token`, `board-token-state`, `board-test`, `board-last-sync` | Settings → Projects connection fields: address, write-only token, "set"/"not set", Test connection, last sync | |
| `chat-view` | The open chat | `data-chat-id`, `data-chat-status` |
| `chat-status` | The chat's status pill | text = the status |
| `composer` | The editable message box (Lexical `contenteditable`) | |
| `send` | Send button (reads Send, Steer, Queue or Interrupt) | `disabled` while the draft is empty |
| `stop` | Stop button, shown only while a turn runs (Stop that pauses the queue) | |
| `transcript` | The transcript scroller | |
| `transcript-row` | One message row | `data-status` (`saved` `streaming` `complete` `error` `interrupted`), `data-role` (`user` `assistant` `toolResult` `context`), `data-row-id` |
| `work-fold` | "Worked for …" row of a settled turn; click to unfold | |
| `tool-card` | One tool card | `data-tool` (tool name), `data-status` (`running` `complete` `failed` `interrupted`) |
| `tool-stop` | Stop on a running tool card (only when supported) | |
| `work-fold-open` | The "Worked for …" row of a turn the reader has opened (click to fold again); the closed row keeps `work-fold` | |
| `work-failed` | The "N failed" count on a "Worked for" row | |
| `tool-status` / `tool-box` / `result-grid` | A tool row's trailing status, its opened output or diff box, and the key/value grid for host and MCP results | `tool-card` also carries `data-source` (`host` `mcp` `native`) |
| `thinking` | The Thinking disclosure of a reply | |
| `queue-grip` | Drag handle of a queued message (Alt+Up/Down moves it one place) | `queue-item` also carries `data-tone` (`amber` `steer` `muted` `error`) |
| `interaction-card` | An approval, question or plan card | `data-attention` (`approval` `question` `plan`) |
| `approval-button` | One approval option | `data-option` = the option text (`Allow once`, `Allow for this session`, `Deny`, `Stop turn`) |
| `question-button` | One question option, or the free-text Answer button | `data-option` = the option text, or `(other)` |
| `question-other` | Free-text input of a question | |
| `question-back` / `question-cancel` | Back to the previous step / skip the question | |
| `plan-button` | Plan approval option | `data-option` = `Approve plan` or `Keep planning` |
| `queue-item` | One pending or queued message under the transcript | `data-state` (`pending` `queued` `sending` `uncertain` `failed`), `data-submission` |
| `queue-pending` | The "Pending" label of an unread asap steer | |
| `queue-withdraw` | Withdraw on a pending steer (only when the worker can withdraw) | |
| `queue-steer-now` / `queue-send-now` / `queue-recover` | Queue item actions | |
| `queue-resume` | Resume on a held queue (after a restart) | |
| `background-chip` | Background-tasks chip in the activity dock | `data-count` = number of tasks |
| `activity-dock` | The live activity dock | |
| `dock-tool-stop` | Stop beside the latest tool line in the dock | |
| `interrupted-banner` / `recover-draft` | Banner on an interrupted chat / its Recover draft button | |
| `composer-error` | A refused send's message (the draft is kept) | |
| `model-picker` / `effort-picker` | Model and effort selects | |
| `permission-chip` | Permission-mode indicator | `data-mode` = the live mode, empty when none reported |
| `jump-latest` | "Jump to latest" button | |
| `side-panel` / `panel-tab` / `panel-slot` / `panel-tab-body` | Side panel: the panel, a document tab, an Inspector slot (Changes, Files), a tab's body. The link gate's refusals show in the shared `toast-slot` | `panel-tab`: `data-kind`, `data-intent` (`user` `program`), `aria-selected`; `panel-slot`: `data-slot`; `panel-tab-body`: `data-kind` |
| `file-viewer` / `code-view` / `folder-view` / `folder-entry` / `file-error` / `open-in-app` / `files-slot` / `file-modal` | File viewer: the viewer body, its CodeMirror source view, a folder listing and one entry, a read error, the Open in <App> header action, the Inspector's Files slot, the modal fallback | `file-viewer`: `data-path` (the path shown now); `folder-entry`: `data-kind` (`dir` `file` `link`) |
| `changes-view` / `changes-section` / `change-row` / `stage-button` / `diff-view` / `diff-gap` / `changes-empty` | Changes slot: the view, a Staged/Unstaged/Conflicts section, one file row, its stage or unstage button, an unfolded diff, a folded run of unchanged lines, the empty state | `changes-section`: `data-section`; `change-row`: `data-path`, `data-section` |
| `comment-editor` / `comment-save` / `review-comment` / `review-tray` / `review-add-to-message` / `panel-context-toggle` / `panel-context-chip` | Review comments and side panel context: the comment card being written and its save button, a saved comment, the composer's review tray and its Add to message, the composer's eye toggle and its "Looking at" chip | `panel-context-toggle`: `aria-pressed` |
| `turn-changes` / `turn-change-file` | The turn change card under a completed turn's last reply and one file in it | `turn-changes`: `data-files` |
| `quick-open` / `quick-open-row` | Ctrl+P "Go to file" and one result | `quick-open-row`: `data-kind` (`file` `dir` `link`) |
| `vault-doc` / `vault-candidates` / `vault-error` / `open-in-obsidian` / `settings-vaults` / `vault-row` | Vault notes: a note tab's body, the list shown for an ambiguous name, a read error, the header action, Settings → Vaults and one vault in it | `vault-doc`: `data-vault`, `data-path` |
| `message-actions` | A row's registered message actions (transcript slots) | each button has `data-action` = the action id; footers are `.row-footer[data-footer]`, composer accessories `.composer-accessory[data-accessory]` |
| `shell` / `topbar` / `topbar-title` / `panel-toggle` | App frame and top bar | `panel-toggle`: `aria-pressed` |
| `settings` / `settings-pane` / `appearance` | Settings dialog, a pane, the General pane's colour-mode control | `settings-pane`: `data-pane` = pane id; tabs are `.settings-tab[data-tab]`; `appearance` segments are `[data-value]` (`system` `light` `dark`) |
| `toast-slot` / `notice` | The generic toast slot (top-centre) and its toast | `notice`: `data-kind` (`error` `info` `restored` `undo` `reorganize`) |
| `toast` | An in-app completion notice, bottom-right | `data-chat-id`; its buttons carry `data-action` (`open` `continue` `reply`) |
| `caption` / `topbar-fork` / `topbar-pin` / `topbar-archive` | Helm's caption buttons and the top bar's chat actions | `topbar-pin`: `aria-pressed` |
| `terminal-dock` / `terminal-tab` / `terminal-session` | Terminal dock, a session tab, a session's view | `terminal-dock`: `data-open`; `terminal-tab`: `data-state` (`idle` `unseen` `running` `exited`), `aria-selected`; `terminal-session`: `data-session` = id, `data-tail` = the last lines of output (the WebGL renderer has no DOM rows) |
| `action-row` / `action-button` / `action-overflow` / `action-menu` | One-click buttons in the top bar: the row, a top-bar button, the "..." trigger, its menu | `action-button`: `data-action` = the seed id (`update` `needs-me` `ship-it` `close-session` `new-session`) or the button id; menu entries are `[role=menuitem][data-item]` with the seed id (`git-sync`) |
| `quick-replies` / `quick-reply` | Continue, Yes and No chips above the composer | `quick-reply`: `data-reply` = `continue` `yes` `no` |
| `option-buttons` / `option-button` / `option-amend` | Inline Option 1, Option A, ... under the latest reply, and the pencil that drafts the answer | `option-button`: `data-option` = the label |
| `explain-plainly` | The Explain plainly message action | |
| `search-dialog` / `search-input` / `search-status` / `search-results` / `search-group` / `search-hit` | Mod+K search: dialog, query box, live status line, result list, a group, a row | `search-group`, `search-hit`: `data-group` = `chats` `files`; `search-status`: `data-state` |

Keys (Windows): Send `Ctrl+Enter` (or `Enter` with the Enter send shortcut), Steer `Ctrl+Shift+Enter`,
Queue `Alt+Enter`, Send now `Ctrl+Alt+Enter`, Stop that continues the queue `Esc`.
