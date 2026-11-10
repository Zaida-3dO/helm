# Stable test hooks (`data-testid`)

The end-to-end checks (`checks/e2e`) target only these hooks. Renaming or removing one
breaks those checks, so change them only together with the checks. Everything else in the DOM
(classes, text, structure) may change freely.

| Hook | Element | Attributes |
|---|---|---|
| `new-chat` | "New chat" nav entry in the sidebar; opens the new-chat page (in the open chat's project) | |
| `newchat-page` | The new-chat page: shown while a draft is open; nothing exists on the host until the first send | |
| `newchat-card`, `newchat-project`, `newchat-folder`, `newchat-folder-menu`, `newchat-copy-path` | Its card, the project picker trigger (boards only), the folder (place line) button, the folder menu and the copy-path button | `newchat-copy-path` carries `data-copied` for 1.3 s after a copy |
| `new-project`, `new-project-title`, `new-project-description`, `new-project-folder`, `new-project-submit`, `new-project-error` | The New project form (Ctrl+O, the picker's New project… row, Settings → Projects): a new board project's title, description and folder, submit and error line | |
| `add-project`, `add-project-mode`, `add-project-path`, `add-project-name`, `add-project-submit`, `add-project-error` | The Add folder dialog (Settings → Projects → Folders): the Existing folder / New folder switch, its fields, submit and error line | |
| `primary-folder`, `primary-folder-path`, `primary-folder-save`, `primary-folder-error` | The Primary folder… dialog | |
| `chat-item` | One chat row in the sidebar | `data-chat-id`, `data-status`, `data-depth` (chat nesting 0–2), `data-group` (the project it is shown under), `data-stalled`, `data-local` (a chat on a local model server), `data-ahead` (how many chats stand ahead of it at that server; present only while it waits) |
| `sidebar-group` | One project header in the sidebar (a board; a folder project only while chats are still shown under it) | `data-project-id`, `data-kind` (`folder` `board`), `data-depth` (project nesting 0–2) |
| `sidebar-set-folder` | "Set folder" on the header of a board with no folder yet; opens the Primary folder dialog | |
| `project-tags`, `project-tag`, `workspace-tag` | The strip under the top bar: the board the open chat is shown on (a boards-only picker) and the folder it runs in | `workspace-tag`: `title` = the full path |
| `sidebar-titlestrip`, `sidebar-beacon`, `workspace-status` | The sidebar's 48 px title strip, the beacon (mark, wordmark, chevron) and its Workspace status popover | beacon: `aria-expanded` |
| `nav-search`, `nav-attention` | The sidebar's Search chats and Needs attention nav entries | |
| `attention-inbox`, `attention-card`, `attention-snooze-menu` | The Needs attention dialog, one card in it, and a card's Snooze menu | card: `data-chat-id`, `data-group` (`needsYou` `flagged` `stalled` `unread`) |
| `attention-triage-setting`, `chat-flag` | The idle-check switch in the Needs attention dialog, and the flag on a sidebar row the idle check raised | |
| `board-refresh` | Settings → Projects: Refresh, syncs with Agent Standup now (the line also shows `board-last-sync`) | — |
| `board-url`, `board-token`, `board-token-state`, `board-test`, `board-last-sync` | Settings → Projects connection fields: address, write-only token, "set"/"not set", Test connection, last sync | |
| `default-board`, `settings-new-project`, `settings-boards`, `settings-set-folder`, `settings-add-folder`, `settings-folders` | Settings → Projects: the default board picker (Automatic or a board), New project…, the boards list and a folderless board's Set folder, Add folder… and the folders list | list rows carry `data-project-id` |
| `settings-closed`, `settings-reopen`, `shared-folder-warning` | Settings → Projects: the Closed list and a row's Reopen, and the note shown when two open boards share a folder | list rows carry `data-project-id` |
| `settings-hub`, `hub-state`, `hub-field-lanUrl`, `hub-field-fallbackUrl`, `hub-field-token`, `hub-save`, `hub-error` | Settings → Hub: the pane, the line state, the write-only address, fallback and token rows ("Set."/"Not set." in the description), Save and the error line | `hub-state`: `data-state` (`off` `connecting` `online` `offline`) |
| `update-pill`, `update-busy` | The top bar's "Restart to update" pill (shown only when the update folder holds a newer setup) and the list of running chats in its confirm dialog | |
| `setting-update-folder`, `update-folder-path`, `update-folder-choose`, `update-folder-check`, `update-folder-clear` | Settings → General's Update folder row: the path, Choose/Change, Check now and Clear | |
| `hub-telegram-chat`, `hub-telegram-reset`, `hub-telegram-limits`, `hub-telegram-model`, `hub-telegram-effort`, `hub-away-alerts`, `hub-away-alerts-box`, `hub-approvals`, `hub-approvals-box` | Settings → Hub's Telegram section: the chat Telegram messages go to (with Open), Start a new chat, the age and token limits, the model and effort pickers for new Telegram chats (their trigger; App default leaves the other new-chat default), the away-alerts row and the Approve from Telegram row, each with its checkbox | |
| `hub-voice`, `hub-voice-chat`, `hub-voice-reset`, `hub-voice-model`, `hub-voice-effort`, `hub-voice-idle`, `hub-voice-watch`, `hub-voice-error` | Settings → Hub's Voice block: the chat the phone's voice screen talks to (with Open), Start a new chat, the model and effort pickers for new voice chats (their trigger), the idle-minutes field and the watch picker | |
| `settings-websearch`, `websearch-key`, `websearch-key-input`, `websearch-key-save`, `websearch-key-forget`, `websearch-test`, `websearch-test-button`, `websearch-test-result`, `websearch-import`, `websearch-path`, `websearch-import-button`, `websearch-offer`, `websearch-offer-box`, `websearch-note`, `websearch-error` | Settings → Web search: the pane, the write-only Brave key row ("A key is set."/"No key is set." in the description) with Save or Replace and Forget, Test with its one-line result, Import from OpenClaw with its editable path, the Offer in Claude chats row with its checkbox, and the note and error lines | `websearch-test-result`: `data-ok` |
| `settings-data`, `data-profile`, `data-profile-path`, `data-backup`, `data-backup-button`, `data-backups`, `data-backup-row`, `data-restore-button`, `data-restore-pending`, `data-note`, `data-error` | Settings → Data: the pane, the data folder row with its path, Make a backup, the backups found (one `data-backup-row` each, with Restore…, whose confirm button reads the phrase the host demands), the line saying a restore waits for the next start, and the note and error lines | `data-backup-row`: `data-path` |
| `settings-localmodels`, `localmodels-check`, `localmodels-checking`, `localmodels-server-<id>`, `localmodels-name`, `localmodels-kind`, `localmodels-enabled`, `localmodels-remove`, `localmodels-url`, `localmodels-model`, `localmodels-key-input`, `localmodels-key-save`, `localmodels-key-forget`, `localmodels-key-state`, `localmodels-reach`, `localmodels-test`, `localmodels-test-result`, `localmodels-draft-problem`, `localmodels-add-strata`, `localmodels-add-ollama`, `localmodels-discard`, `localmodels-save`, `localmodels-tools`, `localmodels-tools-picker`, `localmodels-compare`, `localmodels-compare-picker`, `localmodels-context`, `localmodels-keepalive`, `localmodels-strata-need` (each number row also `<id>-input`), `localmodels-note`, `localmodels-error` | Settings → Local models: the pane, the Check servers button (the only thing here besides Test that contacts a server: opening the pane asks nothing) and its "Checking the servers…" line, one card per server (name, kind, Enabled, Remove, address, Strata's model name, the write-only key row, whether it answered, Test and its result), Add and Save/Discard for the list, and the settings below it | `localmodels-server-<id>`: `data-server-id` (empty until saved); `localmodels-reach`: `data-tone` = `ok`, `bad` or `idle`; `localmodels-key-state`: `data-has-key`; `localmodels-test-result`: `data-ok` |
| `chat-view` | The open chat | `data-chat-id`, `data-chat-status` |
| `chat-status` | The chat's status, a visually hidden span (screen readers and tests; not a visible pill) | text = the status |
| `chat-local-status` | A busy chat on a local model server, a visually hidden span: whether its turn waits queued there or is being answered | `data-ahead` (count ahead, 0 when unknown); text = the wording |
| `composer` | The editable message box (Lexical `contenteditable`) | |
| `send` | Send button (reads Send, Steer or Interrupt) | `disabled` while the draft is empty |
| `stop` | Stop button, shown only while a turn runs (Stop; whatever Claude had not read is sent again right away) | |
| `transcript` | The transcript scroller | |
| `transcript-row` | One message row | `data-status` (`saved` `streaming` `complete` `error` `interrupted`), `data-role` (`user` `assistant` `toolResult` `context`), `data-row-id` |
| `peer-card` | A user row that another chat sent (origin `other_chat`): the "From <chat>" card with the sender's link, the body and a note that the user did not write it | `data-from` (the sending chat's id) |
| `row-origin` | Above a user bubble that did not come from the composer: "Telegram" for a message from the hub's bot (the bubble hides the note for the agent) | `data-origin` (`telegram`) |
| `message-images` / `message-image` | The thumbnails in a user bubble, and one of them (`message-image-viewer` is the image viewer opened from it, `message-image-full` the image in it: `data-state` `loading` `ready` `failed`) | `message-image`: `data-state` (`loading` `ready` `failed`); a `ready` one is a button |
| `image-viewer-count` / `image-viewer-prev` / `image-viewer-next` / `image-viewer-facts` | In the image viewer (attached or sent images): "2 of 3" and the Previous and Next buttons (shown only for several images; the arrow keys step too), and the size and pixel dimensions line | |
| `work-fold` | "Worked for …" row of a settled turn; click to unfold | |
| `tool-card` | One tool card | `data-tool` (tool name), `data-status` (`running` `complete` `failed` `interrupted`) |
| `tool-stop` | Stop on a running tool card (only when supported) | |
| `tool-save-full` / `tool-saved` | Save full output on a long output (the row holds only its start), and the line beside it | |
| `work-fold-open` | The "Worked for …" row of a turn the reader has opened (click to fold again); the closed row keeps `work-fold` | |
| `work-failed` | The "N failed" count on a "Worked for" row | |
| `tool-status` / `tool-box` / `result-grid` | A tool row's trailing status, its opened output or diff box, and the key/value grid for host and MCP results | `tool-card` also carries `data-source` (`host` `mcp` `native`) |
| `thinking` | The Thinking disclosure of a reply | |
| `interaction-card` | An approval, question or plan card | `data-attention` (`approval` `question` `plan`) |
| `approval-button` | One approval option | `data-option` = the option text (`Allow once`, `Allow for this session`, `Deny`, `Stop turn`) |
| `question-button` | One question option, or the free-text Answer button | `data-option` = the option text, or `(other)` |
| `question-other` | Free-text input of a question | |
| `question-back` / `question-cancel` | Back to the previous step / skip the question | |
| `plan-button` | Plan approval option | `data-option` = `Approve plan` or `Keep planning` |
| `queue-item` | One in-flight message under the transcript | `data-state` (`pending` `sending` `uncertain` `failed`), `data-tone` (`amber` `muted` `error`), `data-submission` |
| `queue-pending` | The "Pending" label of an unread asap steer | |
| `queue-withdraw` | Withdraw on a pending steer (only when the worker can withdraw) | |
| `queue-recover` | Recover an unsent message back to the composer | |
| `background-chip` | Background-tasks chip in the activity dock | `data-count` = number of tasks |
| `activity-dock` | The live activity dock | |
| `dock-loading` | The activity dock's second line while a local model is being loaded (its first line is the host's "Loading <model>…"); `data-phase` is `loading` | |
| `cost-local-headline`, `cost-local-tokens`, `cost-local-usd`, `cost-local`, `cost-chat-local`, `cost-local-note` | The Cost dialog's local track: the headline when a report is all local, the "Local: 1.2M tokens, about $4.10 on Sonnet" line beside Claude's figures, a local chat's cell in a project's list, and the note saying what the figure is | |
| `subagent-card` | The subagent card under an Agent/Task tool row | `data-status` (`running` `completed` `failed` `stopped`), `data-tool-call` (the Agent call id) |
| `subagent-status`, `subagent-preview`, `subagent-detail`, `subagent-stop` | A subagent card's status mark, its preview line (step, error or result), its opened detail, and its Stop (only when the worker reports `stopSubagent`) | status: `data-status` |
| `subagent-chip`, `subagent-roster`, `subagent-roster-row` | The dock's subagent chip, its "Across this chat" roster and one row in it | chip: `data-running` (count); row: `data-status` |
| `dock-tool-stop` | Stop beside the latest tool line in the dock | |
| `interrupted-banner` / `interrupted-roll-over` / `recover-draft` | Banner on an interrupted chat, its Roll over button (shown when the host says the prompt was too large for the model's window), and the Recover draft button | |
| `archived-banner` / `archived-restore` | Strip above the composer of an archived chat, and its Restore button | |
| `flag-note` / `flag-note-reason` / `flag-note-snooze` / `flag-note-complete` / `flag-note-menu` | The idle check's note above the composer on a flagged chat, its reason, its Snooze button and menu, and Mark complete | `flag-note`: `aria-busy` while an action runs |
| `composer-error` | A refused send's message (the draft is kept) | |
| `model-picker` | The unified "Model · Effort" picker trigger (Ctrl+Shift+M opens it) | |
| `model-picker-pop` | Its popover: model rows are `[role="option"]` with `data-value` = the model id | |
| `model-roll-over-offer` | The "Roll over into it" button beside the picker's error, shown when the host says the conversation is larger than the chosen model's window; it runs the roll over after a confirmation | |
| `effort-picker` | The effort row in that popover's footer: `[role="radio"]` stops | |
| `local-check`, `local-hint`, `check-servers` | The popover's footer row for local models: "Local models: not checked yet" (or "none found", or just "Local models") and the Check servers button, the only thing in the picker that contacts a local server: opening it asks nothing and the list shows what the servers said last time | |
| `permission-chip` | Permission-mode chip (wraps `permission-picker`, the picker trigger) | `data-mode` = the live mode, empty when none reported; `data-picked` = a pick not yet live |
| `attach-files` | The composer's one Attach files or folders button (opens `attach-menu`: Files… for the native multi-file dialog, Folder… for the folder dialog) | disabled at 20 attachments |
| `attach-menu` | The menu under the attach button | |
| `mention-menu` | The `@` file list above the box while an `@word` is typed | rows are `role="option"` with `data-path` |
| `autocomplete-unavailable` | The dim marker at the editor's bottom right after autocomplete failed | the reason is its `title` |
| `settings-scroll-on-send` | Settings → Chat: scroll to the latest message when I send | |
| `user-bubble` | The reader's own message bubble in the transcript; its `title` is the time tooltip ("Today at 3:41 PM · 5 minutes ago") | |
| `dock-compacting` | The activity dock's second line while the chat compacts its conversation: what is happening and how long it has taken | |
| `topbar-suggest-title` | The Suggest a title button beside the chat title box while it is being renamed; the suggestion fills the box and is saved only on Enter | `aria-busy` while the host works |
| `settings-double-shift` | Settings → Chat: tap Shift twice to open Go to file | |
| `attachment-thumb` | An attached image's thumbnail in the composer: a button (click, Enter or Space) that opens the image viewer, `attachment-viewer` (its image is `image-viewer-image`); the corner button only removes | |
| `composer-refs` | The tray of file, folder and chat chips in the composer box; each chip is `.composer-ref` | `data-kind` = `file`, `folder` or `chat` |
| `composer-setup` / `composer-setup-details` / `composer-setup-open` | The setup card above the box while Claude Code cannot be found (Send is disabled), its details toggle and its Set up button | |
| `composer-context-status` | The "Side panel context on." line inside the box | |
| `make-default` | The star that makes the chat's model and effort the default for new chats | `aria-pressed` while it already is |
| `composer-drop` | The drop target shown over the composer box while files (from Explorer) or a sidebar chat row are dragged over it | the box carries `data-drop` = `files` or `chat` |
| `settings-send-mode` / `settings-wait-limit` / `settings-long-commands` / `settings-hidden-skills` / `setting-background-model` / `settings-background-claude` / `settings-background-local` / `changed-by-chats` / `shortcuts-sheet` / `settings-nomatch` | Settings → Chat's send-behaviour pickers, the hidden-skills box and the Background model row with its two name boxes; Settings → General's Changed by chats list (one `changed-<id>` row each, with `changed-open-<id>` to open the chat); the Ctrl+/ shortcuts sheet; the empty state of a search with no match | |
| `mcp-test` / `mcp-test-result` | Settings → MCP's server editor: the Test connection button and the answer line under the buttons | `mcp-test-result`: `data-state` = `running`, `ok` or `failed` |
| `google-accounts` / `google-account-<address>` | Settings → MCP, under a Google accounts server: the Accounts block and one signed-in address | the block carries `data-server` and `data-state` = `loading`, `no-uv`, `no-client`, `empty` or `ready`; also `google-email`, `google-add`, `google-remove-<address>`, `google-label-<address>`, `google-permission` (Segmented), `google-setup`, `google-signin-note`, `google-auth-url`, `google-notice`, `google-error`, `google-uv-missing` |
| `settings-send-key` | Settings → Chat: the Send message with picker (Ctrl+Enter or Enter) | |
| `setting-ide-context` / `settings-ide-context` | Settings → Chat: the Agents can read your editor selection row and its switch (off by default) | |
| `jump-latest` | "Jump to latest" button | |
| `side-panel` / `panel-tab` / `panel-slot` / `panel-tab-body` | Side panel: the panel, a document tab, an Inspector slot (Changes, Files), a tab's body. The link gate's refusals show in the shared `toast-slot` | `panel-tab`: `data-kind`, `data-intent` (`user` `program`), `aria-selected`; `panel-slot`: `data-slot`; `panel-tab-body`: `data-kind`. The tab's right-click menu is `tab-menu` |
| `file-viewer` / `code-view` / `folder-view` / `folder-entry` / `file-error` / `open-in-app` / `files-slot` / `file-modal` | File viewer: the viewer body, its CodeMirror source view, a folder listing and one entry, a read error, the Open in <App> header action, the Inspector's Files slot, the modal fallback | `file-viewer`: `data-path` (the path shown now); `folder-entry`: `data-kind` (`dir` `file` `link`) |
| `files-tree` / `files-row` / `files-note` | The Files tab's folder tree, one row of it (a file, folder or link), and a loading, empty or error line | `files-row`: `data-path`, `data-kind` (`dir` `file` `link`), `aria-selected`, `aria-expanded` (folders); a double-click or Enter on a file opens a tab |
| `show-in-files` / `back-to-files` / `live-dot` / `file-retry` / `panel-tab-badge` | A file tab's Show in Files button (disabled outside the chat's folder), the Back to files arrow on a tab opened from the tree, the "updated Ns ago" dot in the breadcrumb (5 s after a change on disk), the Try again button on a read error, and the dot on a tab whose file changed while another tab was open | |
| `turn-review` / `turn-review-row` / `mode-working-tree` / `mode-review` / `diff-rendered` / `json-preview` / `table-preview` / `image-preview` | Changes slot's review of one finished turn (the list, a file row, the Working tree and Review switch) and a Markdown file's rendered view; the viewer's JSON tree, CSV/TSV table and image previews | `turn-review-row`: `data-path` |
| `pdf-preview` / `audio-preview` / `video-preview` / `font-preview` / `archive-preview` / `archive-row` / `archive-error` | The viewer's PDF frame, audio and video players, font specimen and ZIP contents listing (with one row per entry and the refusal message for a damaged or unsupported archive) | `archive-row`: `data-kind` (`dir` or `file`) |
| `changes-view` / `changes-section` / `change-row` / `stage-button` / `diff-view` / `diff-gap` / `changes-empty` | Changes slot: the view, a Staged/Unstaged/Conflicts section, one file row, its stage or unstage button, an unfolded diff, a folded run of unchanged lines, the empty state | `changes-section`: `data-section`; `change-row`: `data-path`, `data-section` |
| `comment-editor` / `comment-save` / `review-comment` / `review-tray` / `review-add-to-message` / `panel-context-toggle` / `panel-context-chip` | Review comments and side panel context: the comment card being written and its save button, a saved comment, the composer's review tray and its Add to message, the composer's eye toggle and its "Looking at" chip | `panel-context-toggle`: `aria-pressed` |
| `turn-changes` / `turn-change-file` | The turn change card under a completed turn's last reply and one file in it | `turn-changes`: `data-files` |
| `quick-open` / `quick-open-row` | Ctrl+P "Go to file" and one result | `quick-open-row`: `data-kind` (`file` `dir` `link`) |
| `vault-doc` / `vault-candidates` / `vault-error` / `open-in-obsidian` / `settings-vaults` / `vault-row` | Vault notes: a note tab's body, the list shown for an ambiguous name, a read error, the header action, Settings → Vaults and one vault in it | `vault-doc`: `data-vault`, `data-path` |
| `message-actions` | A row's registered message actions (transcript slots) | each button has `data-action` = the action id (`copy` is the built-in Copy); footers are `.row-footer[data-footer]`, composer accessories `.composer-accessory[data-accessory]` |
| `token-chip` | A row footer's new-token chip (transcript) with its usage tooltip |  |
| `turn-changes-review` | The Review button on a turn's change card (`turn-changes`); opens the Changes slot |  |
| `todo-list` | The TodoWrite checklist card in the transcript |  |
| `gallery-card` / `gallery-image` / `gallery-open` / `gallery-prev` / `gallery-next` / `gallery-dot` / `gallery-strip` / `gallery-count` / `gallery-caption` | A picture gallery the agent showed: the card, its large picture, the click-to-open-full-size button, previous and next, a dot, the thumbnail strip, the "n of m" count and the caption | `gallery-card`: `data-state` (`loading` `ready` `failed`), `data-count`, `data-index`; the full-size viewer is `shown-image-viewer` (picture `shown-image-full`) |
| `option-board` / `option-tile` / `option-other` / `option-other-input` / `option-other-send` / `option-send` / `option-chosen` / `option-error` | An option board the agent offered: the card, one tile, the Other tile and its text field and Send, the Send button of a several-choices board, the "Chosen: …" line and a failure note | `option-board`: `data-state` (`loading` `open` `answered` `failed`); `option-tile`: `data-label`; tiles take the number keys 1 to 9 |
| `live-checklist` / `checklist-steps` | The turn's checklist pinned above the activity dock while it runs, and a list of its steps (also inside the recap) | `live-checklist`: `data-done`, `data-total`; each step is `li.todo[data-state]` |
| `recap-card` / `recap-line` / `recap-steps-toggle` / `recap-pictures` | The recap under a settled turn's final reply, its one-line summary, the toggle for the checklist's last state and the turn's picture thumbnails | `recap-card`: `data-steps`, `data-files`, `data-pictures` |
| `find-bar` / `find-input` / `find-count` | Find in chat (Ctrl+F): the bar, its text field, and the "n of m" count (an `aria-live` region) |  |
| `timeline-rail` / `timeline-band` / `timeline-card` | The work-timeline rail in the transcript's right gutter, its visible-range band, and the marker preview card (hover or click a marker); markers are `.tl-marker[data-row-id]` |  |
| `shell` / `topbar` / `topbar-title` / `panel-toggle` | App frame and top bar | `panel-toggle`: `aria-pressed` |
| `topbar-local-status` | The top bar's wording for a busy chat on a local model server (queued behind other work, or being answered) | `data-ahead` (count ahead, 0 when unknown) |
| `settings` / `settings-pane` / `appearance` / `theme` / `motion` / `sound-level` | Settings dialog, a pane, the General pane's colour-mode control, its Theme skin picker, its Motion and flair picker (Quiet / Standard / Maximal) and its Sound level picker | `settings-pane`: `data-pane` = pane id; tabs are `.settings-tab[data-tab]`; `appearance` segments are `[data-value]` (`system` `light` `dark`) |
| `toast-slot` / `notice` | The generic toast slot (top-centre) and its toast | `notice`: `data-kind` (`error` `info` `restored` `undo` `reorganize`) |
| `toast` | An in-app completion notice, bottom-right | `data-chat-id`; its buttons carry `data-action` (`open` `continue` `reply`) |
| `caption` / `topbar-fork` / `topbar-pin` / `topbar-archive` | Helm's caption buttons and the top bar's chat actions | |
| `terminal-dock` / `terminal-tab` / `terminal-session` | Terminal dock, a session tab, a session's view | `terminal-dock`: `data-open`; `terminal-tab`: `data-state` (`idle` `unseen` `running` `exited`), `aria-selected`; `terminal-session`: `data-session` = id, `data-tail` = the last lines of output (the WebGL renderer has no DOM rows) |
| `action-row` / `action-button` / `action-overflow` / `action-menu` | One-click buttons in the top bar: the row, a top-bar button, the "..." trigger, its menu | `action-row`: `data-folded="true"` when the top bar is too narrow for the buttons; then no `action-button` is drawn, and `action-overflow` is the "Actions" menu trigger (class `action-fold`), whose `action-menu` lists the top-bar buttons first and then the overflow entries. Unfolded, `action-overflow` is the "More actions" icon and its menu holds only the overflow entries. `action-button`: `data-action` = the seed id (`update` `needs-me` `ship-it` `close-session` `new-session`) or the button id; menu entries are `[role=menuitem][data-item]` with the seed id (`git-sync`) |
| `action-edit` / `action-summary` / `action-up` / `action-down` / `action-hide` / `action-delete` / `action-delete-confirm` / `action-add` / `actions-save` | Settings → Actions (`settings-pane` with `data-pane="actions"`): one list item per button, its summary row that opens the editor, move up, move down, hide or show, delete, the in-place "Delete" confirm, Add a button, Save | `action-edit`: `data-action` = the seed id or the button id; `action-summary`: `aria-expanded`; `action-up` is disabled on the first row and `action-down` on the last; `action-hide` toggles the button between Hide and Show again (its label says which); `action-delete` is replaced by `action-delete-confirm` (and a Cancel) while a delete is being confirmed; the editor's fields are `#label-<id>`, `#project-<id>` and `#text-<id>`; `action-add` is disabled at the button limit and `actions-save` until there is a valid change |
| `saved-pane` / `saved-search` / `saved-filter` / `saved-item` / `saved-due` / `saved-start` / `saved-remind-hour` / `saved-remind-tonight` / `saved-remind-tomorrow` / `saved-remind-clear` / `saved-notes` / `saved-done` / `saved-remove` / `saved-note-area` / `saved-note-input` / `saved-note-add` | Settings → Saved (`settings-pane` with `data-pane="saved"`): the pane, the search box, the state filter tabs, one row per item, its due line, Start (opens a chat with the item in the draft), the three reminder presets and Clear reminder (shown while a reminder is set), Notes, Mark done (hidden on a done row) and Remove, and the open note area with its input and Add | `saved-item`: `data-state` = `saved` `filed` `done` `dismissed`; the pager (Newer / Older) appears only past one page |
| `quick-replies` / `quick-reply` | Continue, Yes and No chips above the composer | `quick-reply`: `data-reply` = `continue` `yes` `no` |
| `explain-plainly` | The Explain plainly message action | |
| `search-dialog` / `search-input` / `search-status` / `search-results` / `search-group` / `search-hit` | Mod+K search: dialog, query box, live status line, result list, a group, a row | `search-group`, `search-hit`: `data-group` = `chats` `claudechats` `files` `board`; `search-status`: `data-state` |
| `claude-chat-viewer` / `claude-chat-lines` / `claude-chat-line` / `claude-chat-folder` / `claude-chat-note` / `claude-chat-continue` | The read-only view of one of Claude Code's own chats: the dialog, its line list, one line, the folder line, the status or refusal line, and Continue in Helm | `claude-chat-line`: `data-kind` = `message` `tool` `context` |

Keys (Windows): Send `Ctrl+Enter` (or `Enter` with the Enter send shortcut), Steer `Ctrl+Shift+Enter`,
Send now `Ctrl+Alt+Enter`, Stop and send waiting messages `Esc`.

## Orchestrator

| Hook | Element | Attributes |
|---|---|---|
| `afk-pill` / `afk-pill-label` / `afk-pill-exit` / `afk-pill-error` | The AFK pill in the composer toolbar while AFK is on for all chats, its text, its Exit button and an inline refusal | the pill's `title` is the tooltip |
| `orchestrator-switch` | The Orchestrator switch in the composer toolbar (Claude Code chats, while Settings → Orchestrator is on). Inert states use `aria-disabled`, never `disabled` | `data-look` (`off` `auto` `on-user` `on-agent` `unavailable`), `data-mode` (`on` `off` `auto`), `aria-pressed`, `title` = the tooltip |
| `orchestrator-error` | The red line after the switch when the host refused a click | text = the host's sentence |
| `orchestrator-pane` | Settings → Orchestrator (`settings-pane` with `data-pane="orchestrator"`) | |
| `context-size` / `context-bar` / `roll-over-button` | The quiet context size above the composer (from 50k tokens, or from 60% of the model's window when one is known — the line then names the window and the percent), and the prominent bar with its Roll over button (from 90% of the window, or from 250k where no window is known; the button is disabled while the chat works). The menu entry is the chat menu item with id `rollover` | |
| `runtime-subagent-limits` / `runtime-subagent-turns` / `runtime-subagent-hard-stop` / `runtime-output-cap` | Settings → Runtime: which chats get sub-agent limits (off, Orchestrator chats, all), the tool-call picker, the refuse-past-the-limit switch, and the long tool output cut switch | |
| `settings-runtime` / `runtime-node` / `runtime-claude` / `runtime-recheck` / `runtime-file-problem` / `runtime-executable` / `runtime-executable-save` / `runtime-permission` / `runtime-fast-mode` / `runtime-worktree-tools` / `runtime-schedule-tools` / `runtime-shell-changes` / `runtime-repos` / `runtime-repo-worktrees-<repo id>` / `runtime-model` / `runtime-effort` / `runtime-verbosity` | Settings → Runtime: the pane, the Node and Claude Code status cards, Check again, an unreadable `claude-runtime.json`, the executable field and its Save, the launch permission mode, Fast mode, Claude Code's worktree and schedule tools, the repositories list and each repository's worktree switch, and the new-chat model, effort and response length | `runtime-node`, `runtime-claude`: `data-ready` = `true` or `false` once checked |
| `orch-enabled` / `orch-movefast` / `orch-dispatchgate` | The master switch, Move fast and dispatch gate checkboxes | |
| `orch-guard` | The landing guard checkbox (Settings → Orchestrator) | |
| `orch-newchats` | The Always / Never / Auto group for new chats | segments carry `data-value` (`always` `never` `auto`) |
| `orch-use-<stage>` | One stage's Required / Auto / Off group, stage = `plan` `planReview` `review` `cleanup` `smoketest` | segments carry `data-value` |
| `orch-edits` / `orch-signoff` / `orch-landdefault` / `orch-method` | The other segmented settings | segments carry `data-value` |
| `orch-save` / `orch-revert` / `orch-saved` / `orch-error` | Save, Revert, the "Saved" word, a refused save's message | |
| `workflow-panel` | The workflow panel in the composer dock (a Claude Code chat with a task, master switch on) | `data-status` (the task's status, or `none`), `data-needs` (how many Needs-you items), `aria-busy` while one of your steps runs |
| `workflow-needs` / `workflow-board-queue` / `workflow-stages` / `workflow-stage` | The Needs-you line, the count of records waiting for the task board (shown only when some are), the stage row, one stage | `workflow-stage`: `data-stage`, `data-state` (`done` `current` `pending` `off`) |
| `workflow-signoff` | Sign off plan (only while the plan waits for your sign-off) | |
| `workflow-waiting` | One change waiting on you, with its buttons | `data-branch` |
| `workflow-change-action` | A button on that change | `data-action` (`merge` `decline` `drop` `retry` `ready`) |
| `workflow-menu` / `workflow-task-menu` | The task's ⋯ button and its menu | menu items carry `data-item` (`skip`, `skip:<stage>`, `allowMore`, `allowMore:<stage>`, `park`, `reopen`, `visual`, `notVisual`, `drop`, `drop:<repo> <branch>`, `end`) |
| `workflow-other-waiting` | A task that is not the chat's current one and has a change waiting on you; its changes appear as `workflow-waiting` rows inside it, with the same buttons | `data-run-id`, `data-status` |
| `workflow-other-task` / `workflow-other-reopen` | A non-current task's line in the detail, and its Reopen button (parked or handed-off tasks) | `data-run-id`, `data-status` |
| `workflow-toggle` / `workflow-body` / `workflow-change` | The title that opens the detail, the detail, one change in it | `workflow-change`: `data-state`, `data-branch` |
| `pr-tab` / `pr-state` / `pr-merge-line` / `pr-view-<view>` / `pr-updated` / `pr-refresh` / `pr-error` | The pull request tab body (`data-pr`, `data-view`), its state pill, the merge line, one view button (`conversation` `commits` `checks` `files`; the selected one has `aria-selected`), the "updated N ago" note, Refresh, and the error block with Retry | |
| `pr-add-to-chat` / `pr-new-chat` / `pr-comment-button` / `pr-open-github` / `pr-note` | The tab's actions (Add to chat, New chat, Comment, Open on GitHub) and the four-second status line | |
| `pr-conversation` / `pr-description` / `pr-timeline` / `pr-issue-comment` / `pr-review` / `pr-thread` / `pr-resolve` / `pr-reply` | Conversation: the view, the description box, the timeline, a comment, a review with its threads, a review thread (`data-thread-id`) and its Resolve and Reply buttons | |
| `pr-comment-tools` / `pr-reactions` / `pr-reaction` / `pr-react-add` / `pr-react-pick` / `pr-comment-edit` / `pr-comment-delete` / `pr-delete-confirm` / `pr-delete-yes` / `pr-comment-outcome` | Under a comment: the tools row, the reaction buttons (`pr-reaction` carries `data-reaction`, `aria-pressed` is the user's own), the add-reaction button and its eight choices, Edit and Delete (own comments only), the delete confirmation with its Delete button, and the line for a refused or unconfirmed write. The edit box is `pr-box-edit:<comment id>` with a `pr-box-edit:<comment id>-save` button | |
| `pr-picture` / `pr-picture-loading` / `pr-picture-failed` / `pr-picture-retry` / `pr-picture-viewer` / `pr-picture-full` | A GitHub-hosted picture in comment text: the image, its fixed-size placeholder while loading, the failure box with Try again, and the full-size viewer with its image | |
| `pr-status` / `pr-merge` / `pr-merge-button` / `pr-merge-confirm` / `pr-merge-anyway` / `pr-merge-msg` / `pr-merged` / `pr-ready` / `pr-draft` | The status card, the merge box, its button, the inline Confirm merge and Merge anyway, the outcome line, the merged line, and Ready for review and Convert to draft | |
| `pr-commits` / `pr-commit` / `pr-checks` / `pr-check` | The Commits and Checks views and their rows | |
| `pr-files` / `pr-files-stats` / `pr-file-row` / `pr-file` / `pr-file-add` / `pr-diff` / `pr-line-plus` / `pr-selbar` / `pr-sel-add` / `pr-sel-newchat` / `pr-new-commits` | Files changed: the view, the counts line, a list row, a file section (`data-path`), its Add file to chat, a diff, a line's comment "+", the selected-lines bar with Add to chat and New chat, and the new-commits banner | |
| `pr-diff-mode` / `pr-mode-unified` / `pr-mode-split` / `pr-group-by` / `pr-file-group` / `pr-group-viewed` / `pr-load-whole` / `pr-hide-whole` / `pr-gap` / `pr-pair` | Files changed layout and arranging: the Unified and Split toggle (`data-mode` on `pr-diff` says which is showing), the Group by select, a group of the file list and its Viewed box, Load whole file and Show changes only, a folded run of unchanged lines with its Show buttons, and one side-by-side row | |
| `pr-review-button` / `pr-review-panel` / `pr-box-<box>` / `pr-box-<box>-<button>` / `pr-box-<box>-outcome` | The Review changes button and panel, and each typed-text box (`comment`, `review`, `reply:<thread>`, `line:<path>:<side>:<range>`) with its buttons and its outcome line | |

## Tasks

| Hook | Element | Attributes |
|---|---|---|
| `nav-tasks` | The sidebar footer's Tasks entry (drawn only once a board address and token are saved) | |
| `tasks-view` / `tasks-status` / `tasks-stale` / `tasks-error` / `tasks-refresh` | The Tasks tab, its status line, the out-of-date banner, a failed first read, and the Refresh button in the panel header | `tasks-view`: `data-connection` (`unknown` `ready` `missing`), `data-loaded` |
| `tasks-rail` / `tasks-project-all` / `tasks-project` | The project list, its All projects button, one project | `tasks-project`: `data-project-id`; `data-active` on both buttons |
| `tasks-board` / `tasks-column` / `tasks-column-count` / `tasks-column-more` | The columns, one column, its count ("30 / 41"), its Load more | `tasks-column`: `data-column` (`backlog` `in_progress` `waiting` `completed`) |
| `tasks-card` / `tasks-card-checkpoint` | A card, and the latest checkpoint line under it (read when the card scrolls into view) | `tasks-card`: `data-item-id`, `data-selected` |
| `tasks-drawer` / `tasks-drawer-close` / `tasks-drawer-body` / `tasks-drawer-section` | The item drawer, its close button, the description, and a paged section | `tasks-drawer`: `data-item-id`; `tasks-drawer-section`: `data-section` (the heading) |
| `tasks-open-chat` / `tasks-chat-menu` / `tasks-chat-menu-list` | The drawer's Open chat button (disabled while no chat is linked), its chevron, and the menu with New chat in this task | |

## Sealed pages

| Hook | Element | Attributes |
|---|---|---|
| `nav-pages` / `nav-pages-count` | The sidebar footer's Pages entry and its pinned-page count | |
| `pages-list` / `pages-row` / `pages-empty` | The Pages dialog, one pinned page in it (click opens it as a tab), and the empty note | |
| `page-frame-root` / `page-frame` / `page-error` | A page tab's body (`data-network` is `on` or `off`), its sandboxed frame, and the error shown when the page could not open | |
| `page-reload` / `page-network` / `page-pin` / `page-link-chat` / `page-reset` | The toolbar's Reload, network globe (`aria-pressed`), Pin and Unpin (`aria-pressed`), "send messages to the chat in view" and Reset storage buttons | |
| `page-network-notice` / `page-network-hint` / `page-runtime-error` | The notice while the network is on, the hint after an offline page tried the network, and the dismissible script-error strip | |
| `page-handoff` / `page-handoff-text` / `page-handoff-send` / `page-handoff-add` / `page-handoff-cancel` / `page-handoff-open` / `page-handoff-url` | Helm's confirmation for a page's message or link: the dialog, the editable text, Send to agent, Add to message, Cancel, Open link and the address | |
| `run-page` | The file tab's "Run page" button, shown for an HTML file | |

## Profiles and the household

| Hook | Element | Attributes |
|---|---|---|
| `profile-chip` / `profile-chip-text` / `profile-menu` | The sidebar footer's profile chip, its words ("Name · Plan", the name, or "All profiles"), and the switcher menu | |
| `profile-first-run` / `profile-first-name` / `profile-first-save` | The inline "Who uses this Helm?" form shown in place of the chip until a profile exists | |
| `profile-view` / `profile-view-name` / `profile-view-counts` / `profile-view-show` / `profile-view-manage` | The profile view dialog, its name and counts lines, "Show this profile" and "Manage profiles…" | |
| `household` / `household-group` / `household-chat` | The sidebar's household block, one computer's group in it, and one shared chat row | `household-group`: `data-offline` while the computer is off |
| `remote-chat` / `remote-chat-banner` / `remote-chat-messages` / `remote-chat-earlier` / `remote-chat-error` / `remote-chat-send` | The read-only remote chat dialog, its "On <computer>. Read only." banner, the messages, Load earlier messages, an error line, and the message box (only when household messages are allowed) | |
| `settings-profiles` / `profile-row-<id>` / `profile-name` / `profile-link` / `profile-delete` / `profile-new-name` / `profile-add` / `profiles-error` | Settings → Profiles: the pane, one profile's row and its name, hub-person picker and Delete, the add field and button, and the error line | |
| `hub-pairing` / `hub-pair-address` / `hub-pair-fallback` / `hub-pair-code` / `hub-pair` / `hub-paired` / `hub-pair-error` | Settings → Hub → Pair with a code: the block, its three fields, the Pair button, the "Connected as … on …" row and the error line | |
| `household-switches` / `household-chats-box` / `household-transcripts-box` / `household-messages-box` | The three household switches (Settings → Hub, and the home profile's view) | |
