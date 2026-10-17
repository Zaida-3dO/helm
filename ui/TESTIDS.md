# Stable test hooks (`data-testid`)

The end-to-end checks (`checks/e2e`) target only these hooks. Renaming or removing one
breaks those checks, so change them only together with the checks. Everything else in the DOM
(classes, text, structure) may change freely.

| Hook | Element | Attributes |
|---|---|---|
| `new-chat` | "New chat" button in the chat list | |
| `folder-input` | Folder path input beside it (the last value is remembered) | |
| `chat-item` | One chat in the chat list | `data-chat-id`, `data-status` |
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

Keys (Windows): Send `Ctrl+Enter` (or `Enter` with the Enter send shortcut), Steer `Ctrl+Shift+Enter`,
Queue `Alt+Enter`, Send now `Ctrl+Alt+Enter`, Stop that continues the queue `Esc`.
