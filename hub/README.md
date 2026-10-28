# Helm hub

The always-on service that runs on a home server, beside the PC that runs Helm. It serves three
things:

- **Shared memory**: SQLite with an FTS5 index and optional vectors, as MCP over streamable
  HTTP.
- **The relay line**: Helm on the PC keeps one outbound WebSocket open to `/relay`. Helm
  tools the hub offers over MCP travel down it, so the PC needs no inbound port.
- **Helm's Telegram bot**: messages from the allowed user go down the same line; the live
  status and the answers come back up it.

It is its own Cargo workspace and builds from `hub/` alone, so the container build needs nothing
else from the repo.

## Tools

| Tool | What it does |
|---|---|
| `memory_search {query, k?, vector?, model?, tags?}` | Keyword search (FTS5, porter stemming, bm25 with title > description > tags > body). With `vector`, a hybrid: keyword and cosine rankings fused by reciprocal rank. Only vectors of the same dimension (and `model`, if given) are compared. Every listed tag must match. |
| `memory_read {id}` | One memory in full, by numeric id or by name |
| `memory_write {title, body, tags?, source?, name?, vector?, model?}` | New memory. A vector must come with the id of the model that made it |
| `memory_list {tag?, since?, limit?}` | Newest first; `since` is an ISO-8601 date or UTC timestamp |
| `memory_update`, `memory_delete` | Only when `HUB_ALLOW_MUTATE=1`. A new body without a new vector clears the old vector |

**The hub never computes an embedding.** Callers that have one (Helm on the PC) pass it; callers
that do not (another assistant, or anyone while the PC is asleep) get keyword search.

## Auth

`HUB_TOKENS=client:token[:scope],...`. Tokens are at least 24 characters, unique per client, held
only as SHA-256 digests and compared in constant time.

| Scope | Reaches |
|---|---|
| `rw` (default) | the memory tools |
| `ro` | the three memory read tools |
| `rw+app`, `ro+app` | the above, plus the Helm tools forwarded down the line (below) |
| `relay` | `/relay` only: Helm's own line. It cannot call `/mcp`, and no other token can open the line |
| `telegram` | `POST /telegram/update` only: for a forwarder on a bot another service already runs (below). It cannot call `/mcp` or open the line, and no other token can post updates |

`GET /health` is the one open route and reports only status and version. The server refuses to
start with no tokens configured.

## Relay line and Helm tools

Helm connects to `ws://<hub>/relay` with its `relay` token (both are set in Helm's Settings →
Hub, which also shows whether the line is up); a newer line replaces an older one. The
frames are listed in `src/relay.rs`. A client with `+app` sees six Helm tools: `app_list_chats`,
`app_read_chat`, `app_list_projects`, `app_list_attention`, `app_send_message` and
`app_stop_chat`. The hub never forwards any other name, and Helm checks every call again on the PC
(its outside-caller deny-list and argument refusals, `host/crates/relay/src/gate.rs`). A message
sent this way reaches the chat framed as sent by another agent, never as the user and never as
approval.

- **Line down:** the call answers "Helm offline, nothing sent or changed." and nothing is sent.
- **Line drops mid-call, or no answer in 60 s:** "may or may not have run; check the current state
  before trying again".

## Telegram bot

| Variable | |
|---|---|
| `HUB_TELEGRAM_BOT_TOKEN` | The bot's token from BotFather. Without it the bot is off |
| `HUB_TELEGRAM_ALLOWED_USERS` | Comma-separated numeric Telegram user ids. Empty: the bot is off (it denies everyone) |
| `HUB_TELEGRAM_API` | Bot API base, default the public API (tests point it at `fakes/telegram`) |
| `HUB_TELEGRAM_QUEUE_MINUTES` | How long a message waits while Helm is offline (default 120) |
| `HUB_TELEGRAM_QUEUE_MAX` | How many messages wait (default 50) |
| `HUB_TELEGRAM_RECEIVE` | `poll` (default) or `off`, below. Anything else: the hub refuses to start |
| `HUB_TELEGRAM_STT_URL` | An OpenAI-compatible speech-to-text endpoint for voice notes, e.g. `http://<host>:<port>/v1/audio/transcriptions`. Unset: recordings go without a transcript. Not an `http://` or `https://` URL: the hub refuses to start |
| `HUB_TELEGRAM_STT_MODEL` | The model the server is asked for (default `large-v3-turbo`) |
| `HUB_TELEGRAM_STT_LANGUAGE` | The language of the voice notes (default `en`) |
| `HUB_TELEGRAM_STT_TIMEOUT_MINUTES` | How long one transcription may take, 1-60 (default 10). A long recording is split up by the server and can take minutes |

- A sender not on the allow-list gets nothing: no forward and no reply.
- While Helm is offline a message is queued in order and the sender is told at once. On reconnect
  the queue is delivered, each message carrying its age so Helm marks it "sent N min ago". An
  expired message is dropped and the user is told how many. `/cancel` drops the queue; `/stop`
  stops Helm's running turn. The queue is in memory: a container restart drops it.
- Each message is held until Helm acknowledges it. A line that drops before the acknowledgement
  resends it on the next line, and a message the current line has not acknowledged within 45 s
  is sent again on that line, every 45 s (Helm gives up on a busy chat sooner than that, without
  acknowledging). A message still unacknowledged after the queue lifetime is dropped and the user
  is told it could not be delivered. A resend carries the same id.
- While a turn runs, ONE status message is edited in place, at most every 3 s. The answer arrives
  as a new message (so the phone notifies) and the status message is deleted.
- Helm can also speak first (`telegram_notify`: a chat telling the user its work is finished, or
  an alert while the user is away). That arrives as a new message too, and leaves a live status
  message where it is.
- **Files.** Every message that carries a file is fetched with `getFile` and goes down the line
  as one `telegram` frame with the file attached: a document of any type, a photo, a video, an
  animation, a video note, an audio file or a voice note. Pictures Helm shows (still PNG, JPEG and
  WebP up to 20 megapixels, judged by the file's own bytes; of a photo's sizes the largest that
  fits) go in an `images` list, anything else (a GIF or HEIC picture included) in a `files` list,
  both of `name`, `mediaType` and base64 `data`. The caption is the text, and a line says what
  came in (`[File] report.pdf, 1.2 MB`; `[Photo]` or `[Image]` for a picture with no caption).
  The Bot API lets a bot download at most 20 MB, so a larger file gets a plain reply ("report.pdf
  was not sent to Helm: ...") and is left out. Several files sent together (an album) are gathered
  for 1.5 s after the last one and go as one message, its caption the one the album carries. One
  message carries at most 10 files and 50 MB; about 150 MB may wait for Helm at once. A file over
  any of these is left out with a reply. A message keeps its place in the queue while its files
  are fetched, so a message sent after it waits for it. File bytes are never logged. Helm makes
  each name safe for its disk and saves the file where the chat's agent can read it.
- **Recordings.** With `HUB_TELEGRAM_STT_URL` set, a voice note, an audio file or a video note is
  also sent to the speech-to-text server as a multipart form (`file` as uploaded, Telegram's
  OGG/Opus is fine; `model`; `language`; `response_format=text`; no auth). The transcript goes
  beside the attached recording: `[Voice note] <transcript>`, or a `Transcript:` line under an
  audio file or video note, with any markers the server writes (`[gap: ...]`, `[truncated: ...]`)
  as they are. The server has 5 s to accept the connection and `HUB_TELEGRAM_STT_TIMEOUT_MINUTES`
  to answer. One that cannot be reached, runs out of time, answers anything but 200 or returns a
  JSON error gets a plain reply ("Transcription is unavailable right now: ..."), and the recording
  still goes, marked as not transcribed; a video note it cannot transcribe just goes as a file.
  Without a server the recordings go untranscribed, and a voice note gets a reply saying so.
- **Quoted replies.** A message that replies to another, or quotes part of one, reaches Helm with
  `Replying to: "<quoted text>"` on its own first line: the quoted part if there is one, else the
  replied-to message's text or caption, on one line and cut to 300 characters. Files and recordings too.
- **Approval buttons.** Helm sends `telegram_ask {id, text, options}` and the bot posts a new
  message with one inline button per option (at most 4, each cut to 40 characters). A tap from an
  allowed user in a private chat goes to Helm once, as `telegram_answer {id, index}`, and the
  message then shows the choice and loses its buttons. With the line down the tap answers "Helm
  is offline; nothing was sent." and the buttons stay. A tap on an ask that no longer waits (used,
  settled, or among more than the 50 newest) answers "This request is no longer waiting." and the
  buttons go. `telegram_ask_done {id, text}` (settled in Helm) adds `text` to the message and
  removes the buttons. A tap is the only way to answer an ask: a text reply never is. The hub
  checks only the Telegram side (who tapped, where, and that the ask still waits); Helm checks the
  rest. The asks are in memory too.
  - **With a shared bot** (`HUB_TELEGRAM_RECEIVE=off`) the taps come from the other service's
    forwarder, and the hub can only trust what it posts: who tapped and which button. Approving
    from Telegram then trusts that forwarder as much as the bot's own token.

### Receiving updates: polling or a shared bot

Only one process may consume a bot's updates. With `HUB_TELEGRAM_RECEIVE=poll` (the default) the
hub is that process and long-polls `getUpdates` itself. To use a bot that another service already
runs, set `HUB_TELEGRAM_RECEIVE=off`: the hub never calls `getUpdates`, but it still calls `getMe`
for the bot's name and still sends, edits and deletes messages with the bot token. That service's
forwarder then posts the updates meant for Helm:

```
POST /telegram/update
Authorization: Bearer <a telegram-scope token>     # HUB_TOKENS=forwarder:<token>:telegram
Content-Type: application/json

<one Telegram Update object, or an array of them>
```

The answer is `200 {"ok":true,"handled":[...]}`, one entry per update: `forwarded`, `queued`,
`command`, `voice` (a recording now being fetched and transcribed; it is forwarded or queued once
it has its text), `image` (a picture now being fetched, likewise), `file` (any other file, likewise),
`button` (a tap on one of Helm's buttons), `ignored` (not a private message or tap from an allowed
user) or `duplicate`. No token or an
unknown one: 401; any other scope: 403; the bot off on this hub: 503; a body that is not an
Update or an array of them: 400. The endpoint exists in both modes.

- The hub reads two kinds of update: `message` and `callback_query` (a button tap). Polling asks
  for just those, and a forwarder should post both.
- Posted and polled updates take the same path: the allow-list, private chats only, and the
  commands. Each `update_id` is handled once (the last 256 are remembered), so a forwarder's
  retry, or an update that arrives both ways, is dropped as `duplicate`.
- `/helm <text>` (also `/helm@<botname> <text>`) is read as `<text>`, including when `<text>` is
  itself a command such as `/helm /stop`, so a forwarder can route Helm's messages by that one
  command. `/helm` alone answers the hello text.

## Commands

```
helm-hub serve                       # HUB_DB, HUB_BIND, HUB_TOKENS, HUB_ALLOW_MUTATE, HUB_TELEGRAM_*
helm-hub import <dir> [--source S]   # S: the source tag the imported memories carry
```

`import` reads the top-level `*.md` files of a folder (subfolders are not read). Frontmatter `name`,
`description` and `type` (or `metadata.type`) are kept, and so are the body and its `[[wiki links]]`.
The memory's title is the body's first `# heading`, else its name. A file with no frontmatter is
keyed on its file stem. The import is idempotent on `name`: unchanged files are left alone, changed
files are updated in place, and a changed body drops that memory's stored vector, since the vector
described the old text. `MEMORY.md` and `README.md` are index files and are skipped. Two files
claiming one name are reported as failures rather than overwriting each other. Imported memories
are tagged `imported`, the source tag, and their type.

## Build, test, run

```
cd hub
cargo test
cargo clippy --all-targets -- -D warnings
docker build -t helm-hub:latest .
```

`compose.snippet.yml` is the service block for the server's compose group. The bot's
long poll, the line and the memory store share the one-thread runtime; Bot API calls run on the
blocking pool. Run the import inside
the container: `docker exec helm-hub helm-hub import /mnt/memory`.

## Budget

One-thread runtime, one SQLite connection, a 2 MB page cache. A release build idles at about 7 MB
of working set with ~340 memories loaded (measured on Windows). Vector search is a brute-force
scan, which is fine below roughly 50k rows.
