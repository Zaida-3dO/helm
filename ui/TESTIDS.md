# Stable test hooks (`data-testid`)

The end-to-end checks (`checks/e2e`) target only these hooks. Renaming or removing one
breaks those checks, so change them only together with the checks. Everything else in the DOM
(classes, text, structure) may change freely.

| Hook | Element | Attributes |
|---|---|---|
| `new-chat` | "New chat" nav entry in the sidebar; opens the new-chat page (in the open chat's project) | |
| `newchat-page` | The new-chat page: shown while a draft is open; nothing exists on the host until the first send | |
| `newchat-card`, `newchat-project`, `newchat-folder`, `newchat-folder-menu`, `newchat-copy-path` | Its card, the project picker trigger, the folder (place line) button, the folder menu and the copy-path button | `newchat-copy-path` carries `data-copied` for 1.3 s after a copy |
| `add-project`, `add-project-mode`, `add-project-path`, `add-project-name`, `add-project-submit`, `add-project-error` | The Add project dialog (Ctrl+O): the Existing folder / New folder switch, its fields, submit and error line | |
| `primary-folder`, `primary-folder-path`, `primary-folder-save`, `primary-folder-error` | The Primary folder… dialog | |
| `chat-item` | One chat row in the sidebar | `data-chat-id`, `data-status`, `data-depth` (chat nesting 0–2), `data-group` (the project it is shown under) |
| `sidebar-group` | One project header in the sidebar | `data-project-id`, `data-kind` (`folder` `board`), `data-depth` (project nesting 0–2) |
| `sidebar-titlestrip`, `sidebar-beacon`, `workspace-status` | The sidebar's 48 px title strip, the beacon (mark, wordmark, chevron) and its Workspace status popover | beacon: `aria-expanded` |
| `nav-search`, `nav-attention` | The sidebar's Search chats and Needs attention nav entries | |
| `attention-inbox`, `attention-card`, `attention-snooze-menu` | The Needs attention dialog, one card in it, and a card's Snooze menu | card: `data-chat-id`, `data-group` (`needsYou` `flagged` `stalled` `unread`) |
| `attention-triage-setting`, `chat-flag` | The idle-check switch in the Needs attention dialog, and the flag on a sidebar row the idle check raised | |
| `board-sync` | The Agent Standup sync line under the view switch | `data-state` (`never` `unconfigured` `syncing` `ok` `error`) |
| `board-url`, `board-token`, `board-token-state`, `board-test`, `board-last-sync` | Settings → Projects connection fields: address, write-only token, "set"/"not set", Test connection, last sync | |
| `settings-hub`, `hub-state`, `hub-field-lanUrl`, `hub-field-fallbackUrl`, `hub-field-token`, `hub-save`, `hub-error` | Settings → Hub: the pane, the line state, the write-only address, fallback and token rows ("Set."/"Not set." in the description), Save and the error line | `hub-state`: `data-state` (`off` `connecting` `online` `offline`) |
| `update-pill`, `update-busy` | The top bar's "Restart to update" pill (shown only when the update folder holds a newer setup) and the list of running chats in its confirm dialog | |
| `setting-update-folder`, `update-folder-path`, `update-folder-choose`, `update-folder-check`, `update-folder-clear` | Settings → General's Update folder row: the path, Choose/Change, Check now and Clear | |
| `hub-telegram-chat`, `hub-telegram-reset`, `hub-telegram-limits` | Settings → Hub's Telegram section: the chat Telegram messages go to (with Open), Start a new chat, and the age and token limits | |
| `chat-view` | The open chat | `data-chat-id`, `data-chat-status` |
| `chat-status` | The chat's status, a visually hidden span (screen readers and tests; not a visible pill) | text = the status |
| `composer` | The editable message box (Lexical `contenteditable`) | |
| `send` | Send button (reads Send, Steer, Queue or Interrupt) | `disabled` while the draft is empty |
| `stop` | Stop button, shown only while a turn runs (Stop that pauses the queue) | |
| `transcript` | The transcript scroller | |
| `transcript-row` | One message row | `data-status` (`saved` `streaming` `complete` `error` `interrupted`), `data-role` (`user` `assistant` `toolResult` `context`), `data-row-id` |
| `row-origin` | Above a user bubble that did not come from the composer: "Telegram" for a message from the hub's bot (the bubble hides the note for the agent) | `data-origin` (`telegram`) |
| `message-images` / `message-image` | The thumbnails in a user bubble, and one of them (`message-image-viewer` is the full-size dialog, `message-image-full` the image in it: `data-state` `loading` `ready` `failed`) | `message-image`: `data-state` (`loading` `ready` `failed`); a `ready` one is a button |
| `work-fold` | "Worked for …" row of a settled turn; click to unfold | |
| `tool-card` | One tool card | `data-tool` (tool name), `data-status` (`running` `complete` `failed` `interrupted`) |
| `tool-stop` | Stop on a running tool card (only when supported) | |
| `tool-save-full` / `tool-saved` | Save full output on a long output (the row holds only its start), and the line beside it | |
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
| `queue-edit` / `queue-steer-now` / `queue-send-now` / `queue-recover` | Queue item actions | |
| `queue-resume` | Resume on a held queue (after a restart) | |
| `background-chip` | Background-tasks chip in the activity dock | `data-count` = number of tasks |
| `activity-dock` | The live activity dock | |
| `subagent-card` | The subagent card under an Agent/Task tool row | `data-status` (`running` `completed` `failed` `stopped`), `data-tool-call` (the Agent call id) |
| `subagent-status`, `subagent-preview`, `subagent-detail`, `subagent-stop` | A subagent card's status mark, its preview line (step, error or result), its opened detail, and its Stop (only when the worker reports `stopSubagent`) | status: `data-status` |
| `subagent-chip`, `subagent-roster`, `subagent-roster-row` | The dock's subagent chip, its "Across this chat" roster and one row in it | chip: `data-running` (count); row: `data-status` |
| `dock-tool-stop` | Stop beside the latest tool line in the dock | |
| `interrupted-banner` / `recover-draft` | Banner on an interrupted chat / its Recover draft button | |
| `composer-error` | A refused send's message (the draft is kept) | |
| `model-picker` | The unified "Model · Effort" picker trigger (Ctrl+Shift+M opens it) | |
| `model-picker-pop` | Its popover: model rows are `[role="option"]` with `data-value` = the model id | |
| `effort-picker` | The effort row in that popover's footer: `[role="radio"]` stops | |
| `permission-chip` | Permission-mode chip (wraps `permission-picker`, the picker trigger) | `data-mode` = the live mode, empty when none reported; `data-picked` = a pick not yet live |
| `attach-files` / `attach-folder` | The composer's Attach files (native multi-file dialog) and Attach a folder buttons | disabled at 20 attachments |
| `composer-refs` | The tray of file, folder and chat chips in the composer box; each chip is `.composer-ref` | `data-kind` = `file`, `folder` or `chat` |
| `composer-setup` / `composer-setup-details` / `composer-setup-open` | The setup card above the box while Claude Code cannot be found (Send is disabled), its details toggle and its Set up button | |
| `composer-context-status` | The "Side panel context on." line inside the box | |
| `make-default` | The star that makes the chat's model and effort the default for new chats | `aria-pressed` while it already is |
| `composer-drop` | The drop target shown over the composer box while files (from Explorer) or a sidebar chat row are dragged over it | the box carries `data-drop` = `files` or `chat` |
| `settings-send-mode` / `settings-wait-limit` / `settings-long-commands` / `settings-hidden-skills` / `changed-by-chats` / `shortcuts-sheet` / `settings-nomatch` | Settings → Chat's send-behaviour pickers and the hidden-skills box; Settings → General's Changed by chats list (one `changed-<id>` row each); the Ctrl+/ shortcuts sheet; the empty state of a search with no match | |
| `mcp-test` / `mcp-test-result` | Settings → MCP's server editor: the Test connection button and the answer line under the buttons | `mcp-test-result`: `data-state` = `running`, `ok` or `failed` |
| `settings-send-key` | Settings → Chat: the Send message with picker (Ctrl+Enter or Enter) | |
| `jump-latest` | "Jump to latest" button | |
| `side-panel` / `panel-tab` / `panel-slot` / `panel-tab-body` | Side panel: the panel, a document tab, an Inspector slot (Changes, Files), a tab's body. The link gate's refusals show in the shared `toast-slot` | `panel-tab`: `data-kind`, `data-intent` (`user` `program`), `aria-selected`; `panel-slot`: `data-slot`; `panel-tab-body`: `data-kind`. The tab's right-click menu is `tab-menu` |
| `file-viewer` / `code-view` / `folder-view` / `folder-entry` / `file-error` / `open-in-app` / `files-slot` / `file-modal` | File viewer: the viewer body, its CodeMirror source view, a folder listing and one entry, a read error, the Open in <App> header action, the Inspector's Files slot, the modal fallback | `file-viewer`: `data-path` (the path shown now); `folder-entry`: `data-kind` (`dir` `file` `link`) |
| `turn-review` / `turn-review-row` / `mode-working-tree` / `mode-review` / `diff-rendered` / `json-preview` / `table-preview` / `image-preview` | Changes slot's review of one finished turn (the list, a file row, the Working tree and Review switch) and a Markdown file's rendered view; the viewer's JSON tree, CSV/TSV table and image previews | `turn-review-row`: `data-path` |
| `changes-view` / `changes-section` / `change-row` / `stage-button` / `diff-view` / `diff-gap` / `changes-empty` | Changes slot: the view, a Staged/Unstaged/Conflicts section, one file row, its stage or unstage button, an unfolded diff, a folded run of unchanged lines, the empty state | `changes-section`: `data-section`; `change-row`: `data-path`, `data-section` |
| `comment-editor` / `comment-save` / `review-comment` / `review-tray` / `review-add-to-message` / `panel-context-toggle` / `panel-context-chip` | Review comments and side panel context: the comment card being written and its save button, a saved comment, the composer's review tray and its Add to message, the composer's eye toggle and its "Looking at" chip | `panel-context-toggle`: `aria-pressed` |
| `turn-changes` / `turn-change-file` | The turn change card under a completed turn's last reply and one file in it | `turn-changes`: `data-files` |
| `quick-open` / `quick-open-row` | Ctrl+P "Go to file" and one result | `quick-open-row`: `data-kind` (`file` `dir` `link`) |
| `vault-doc` / `vault-candidates` / `vault-error` / `open-in-obsidian` / `settings-vaults` / `vault-row` | Vault notes: a note tab's body, the list shown for an ambiguous name, a read error, the header action, Settings → Vaults and one vault in it | `vault-doc`: `data-vault`, `data-path` |
| `message-actions` | A row's registered message actions (transcript slots) | each button has `data-action` = the action id (`copy` is the built-in Copy); footers are `.row-footer[data-footer]`, composer accessories `.composer-accessory[data-accessory]` |
| `token-chip` | A row footer's new-token chip (transcript) with its usage tooltip |  |
| `turn-changes-review` | The Review button on a turn's change card (`turn-changes`); opens the Changes slot |  |
| `todo-list` | The TodoWrite checklist card in the transcript |  |
| `find-bar` / `find-input` / `find-count` | Find in chat (Ctrl+F): the bar, its text field, and the "n of m" count (an `aria-live` region) |  |
| `timeline-rail` / `timeline-band` / `timeline-card` | The work-timeline rail in the transcript's right gutter, its visible-range band, and the marker preview card (hover or click a marker); markers are `.tl-marker[data-row-id]` |  |
| `shell` / `topbar` / `topbar-title` / `panel-toggle` | App frame and top bar | `panel-toggle`: `aria-pressed` |
| `settings` / `settings-pane` / `appearance` | Settings dialog, a pane, the General pane's colour-mode control | `settings-pane`: `data-pane` = pane id; tabs are `.settings-tab[data-tab]`; `appearance` segments are `[data-value]` (`system` `light` `dark`) |
| `toast-slot` / `notice` | The generic toast slot (top-centre) and its toast | `notice`: `data-kind` (`error` `info` `restored` `undo` `reorganize`) |
| `toast` | An in-app completion notice, bottom-right | `data-chat-id`; its buttons carry `data-action` (`open` `continue` `reply`) |
| `caption` / `topbar-fork` / `topbar-pin` / `topbar-archive` | Helm's caption buttons and the top bar's chat actions | |
| `terminal-dock` / `terminal-tab` / `terminal-session` | Terminal dock, a session tab, a session's view | `terminal-dock`: `data-open`; `terminal-tab`: `data-state` (`idle` `unseen` `running` `exited`), `aria-selected`; `terminal-session`: `data-session` = id, `data-tail` = the last lines of output (the WebGL renderer has no DOM rows) |
| `action-row` / `action-button` / `action-overflow` / `action-menu` | One-click buttons in the top bar: the row, a top-bar button, the "..." trigger, its menu | `action-row`: `data-folded="true"` when the top bar is too narrow for the buttons; then no `action-button` is drawn, and `action-overflow` is the "Actions" menu trigger (class `action-fold`), whose `action-menu` lists the top-bar buttons first and then the overflow entries. Unfolded, `action-overflow` is the "More actions" icon and its menu holds only the overflow entries. `action-button`: `data-action` = the seed id (`update` `needs-me` `ship-it` `close-session` `new-session`) or the button id; menu entries are `[role=menuitem][data-item]` with the seed id (`git-sync`) |
| `action-edit` / `action-summary` / `action-up` / `action-down` / `action-hide` / `action-delete` / `action-delete-confirm` / `action-add` / `actions-save` | Settings → Actions (`settings-pane` with `data-pane="actions"`): one list item per button, its summary row that opens the editor, move up, move down, hide or show, delete, the in-place "Delete" confirm, Add a button, Save | `action-edit`: `data-action` = the seed id or the button id; `action-summary`: `aria-expanded`; `action-up` is disabled on the first row and `action-down` on the last; `action-hide` toggles the button between Hide and Show again (its label says which); `action-delete` is replaced by `action-delete-confirm` (and a Cancel) while a delete is being confirmed; the editor's fields are `#label-<id>`, `#project-<id>` and `#text-<id>`; `action-add` is disabled at the button limit and `actions-save` until there is a valid change |
| `quick-replies` / `quick-reply` | Continue, Yes and No chips above the composer | `quick-reply`: `data-reply` = `continue` `yes` `no` |
| `option-buttons` / `option-button` / `option-amend` | Inline Option 1, Option A, ... under the latest reply, and the pencil that drafts the answer | `option-button`: `data-option` = the label |
| `explain-plainly` | The Explain plainly message action | |
| `search-dialog` / `search-input` / `search-status` / `search-results` / `search-group` / `search-hit` | Mod+K search: dialog, query box, live status line, result list, a group, a row | `search-group`, `search-hit`: `data-group` = `chats` `files`; `search-status`: `data-state` |

Keys (Windows): Send `Ctrl+Enter` (or `Enter` with the Enter send shortcut), Steer `Ctrl+Shift+Enter`,
Queue `Alt+Enter`, Send now `Ctrl+Alt+Enter`, Stop that continues the queue `Esc`.

## Orchestrator

| Hook | Element | Attributes |
|---|---|---|
| `orchestrator-switch` | The Orchestrator switch in the composer toolbar (Claude Code chats, while Settings → Orchestrator is on). Inert states use `aria-disabled`, never `disabled` | `data-look` (`off` `auto` `on-user` `on-agent` `unavailable`), `data-mode` (`on` `off` `auto`), `aria-pressed`, `title` = the tooltip |
| `orchestrator-error` | The red line after the switch when the host refused a click | text = the host's sentence |
| `orchestrator-pane` | Settings → Orchestrator (`settings-pane` with `data-pane="orchestrator"`) | |
| `settings-runtime` / `runtime-node` / `runtime-claude` / `runtime-recheck` / `runtime-file-problem` / `runtime-executable` / `runtime-executable-save` / `runtime-permission` / `runtime-fast-mode` / `runtime-model` / `runtime-effort` / `runtime-verbosity` | Settings → Runtime: the pane, the Node and Claude Code status cards, Check again, an unreadable `claude-runtime.json`, the executable field and its Save, the launch permission mode, Fast mode, and the new-chat model, effort and response length | `runtime-node`, `runtime-claude`: `data-ready` = `true` or `false` once checked |
| `orch-enabled` / `orch-movefast` | The master switch and Move fast checkboxes | |
| `orch-newchats` | The Always / Never / Auto group for new chats | segments carry `data-value` (`always` `never` `auto`) |
| `orch-use-<stage>` | One stage's Required / Auto / Off group, stage = `plan` `planReview` `review` `cleanup` `smoketest` | segments carry `data-value` |
| `orch-edits` / `orch-signoff` / `orch-landdefault` / `orch-method` | The other segmented settings | segments carry `data-value` |
| `orch-save` / `orch-revert` / `orch-saved` / `orch-error` | Save, Revert, the "Saved" word, a refused save's message | |
| `workflow-panel` | The workflow panel in the composer dock (a Claude Code chat with a task, master switch on) | `data-status` (the task's status, or `none`), `data-needs` (how many Needs-you items), `aria-busy` while one of your steps runs |
| `workflow-needs` / `workflow-stages` / `workflow-stage` | The Needs-you line, the stage row, one stage | `workflow-stage`: `data-stage`, `data-state` (`done` `current` `pending` `off`) |
| `workflow-signoff` | Sign off plan (only while the plan waits for your sign-off) | |
| `workflow-waiting` | One change waiting on you, with its buttons | `data-branch` |
| `workflow-change-action` | A button on that change | `data-action` (`merge` `decline` `drop` `retry` `ready`) |
| `workflow-menu` / `workflow-task-menu` | The task's ⋯ button and its menu | menu items carry `data-item` (`skip`, `skip:<stage>`, `allowMore`, `allowMore:<stage>`, `park`, `reopen`, `visual`, `notVisual`, `drop`, `drop:<repo> <branch>`, `end`) |
| `workflow-other-waiting` | A task that is not the chat's current one and has a change waiting on you; its changes appear as `workflow-waiting` rows inside it, with the same buttons | `data-run-id`, `data-status` |
| `workflow-other-task` / `workflow-other-reopen` | A non-current task's line in the detail, and its Reopen button (parked or handed-off tasks) | `data-run-id`, `data-status` |
| `workflow-toggle` / `workflow-body` / `workflow-change` | The title that opens the detail, the detail, one change in it | `workflow-change`: `data-state`, `data-branch` |
