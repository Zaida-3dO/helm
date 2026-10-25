# Session log format (`helm-session 1`)

Helm keeps each chat's history in its own append-only log, in a format of its own. Code:
`src/session_log.rs`.

## Files (per chat, in `<profile>/sessions/`)

| File | What | Rules |
|---|---|---|
| `<chat>.jsonl` | the canonical history | append-only; owner-only; one JSON object per line |
| `<chat>.jsonl.torn` | quarantine for torn fragments | one line per fragment: `{offset, hash, bytes}`; de-duplicated by offset and SHA-256 |
| `<chat>.claude.json` | the Claude Code binding | at most 16 KiB; written atomically |

## Lines

Line 1 is the **header**:
`{"type":"header","version":"helm-session 1","chatId","harness":"claude","cwd","createdAt"}`.

Every later line is an **entry** with an `id` and a `parentId` (`null` for the first). Helm writes
a linear chain: each entry's parent is the entry written before it. The entry id doubles as the
message row's `source_key`, which is what makes projection idempotent.

| `type` | Id | Fields | Written when |
|---|---|---|---|
| `message` | `u:<submissionId>` for a user row, `m:<messageId>` for an assistant row | `timestamp, role, text, thinking, status, details` | the worker ends the message (`message_end`) |
| `tool` | `t:<toolCallId>` | `timestamp, toolCallId, toolName, args, result, isError, startedAt, endedAt` | the tool call ends; start and end are merged into one entry |
| `shellChanges` | `s:<toolCallId>` | `timestamp, toolCallId, paths` | the worker found files a Bash call changed (compared the checkout before and after); no row, the turn's change card reads it |
| `context` | `c:<uuid>` | `timestamp, kind, text`; `kind` is `compaction`, `model_change`, `effort_change`, `notice` or `restart` | the host records a context row |

Reserved and not written: `fork`, `harness_switch`, `rewind`, `subagent_name`. A reader keeps an
entry of a kind it does not know in the chain, so its children still resolve, but does not project
it.

Streaming state is never logged. The live row exists only in the database (flushed every 80 ms)
until its `message_end`, and then the logged entry replaces it under the same key.

## Projection

The host appends an entry, then projects it in one database transaction:
- into `entries` (`chat_id, entry_id, parent_id, kind, raw`), insert-or-ignore;
- into `messages`, upserted by `(chat_id, source_key)`.

If the app dies between the two, startup `reconcile` projects every log entry missing from
`entries`. Running it again changes nothing.

A row whose text plus thinking passes 64 KiB keeps 64 KiB of text, 16 KiB of thinking and the
last 2,400 characters of thinking. The full content is written to
`<profile>/artifacts/<sha256>.json`, and the row's `artifact` names that file.

## Reading and torn lines

- A blank line is skipped, and a trailing `\r` is tolerated.
- **Trailing partial line** (a writer killed mid-append): left in place. The next append first
  writes a newline, so from then on it reads as a mid-file torn line.
- **Mid-file torn line:**
  1. The reader looks for a whole record later on the same line whose parent is already known,
     and keeps it.
  2. The bytes before it go to the quarantine.
  3. One inert notice row (`source_key` `torn:<offset>`) is posted the first time a fragment is
     quarantined.
- **Failures:** more than 8 torn lines, a repeated id, an unknown parent, or an entry with no id
  fail the read. The chat's rows are then left as they were, and the error is logged.
- **Half an emoji** (an unpaired UTF-16 surrogate escape) is not damage. It loads with U+FFFD.

There is no pre-launch repair rewrite of a damaged log yet.

## Claude Code binding (`<chat>.claude.json`)

`{nativeSessionId, cwd, lastLeafId, harnessSwitchId, updatedAt}`:
- `nativeSessionId` comes from the worker's `native_session` event.
- `lastLeafId` is the session-log id of the last settled entry.
- `harnessSwitchId` is always `null` until harness switching exists.

A launch resumes with `nativeSessionId` (`claude.resumeSessionId`) when the binding's `cwd` matches
the chat's folder. Otherwise it starts fresh.
