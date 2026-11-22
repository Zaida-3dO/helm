# Helm hub

The always-on service that runs on a home server, beside the PCs that run Helm. It is shared by
the people of one household, each with one or more machines. It serves:

- **Memory**: SQLite with an FTS5 index and optional vectors, as MCP over streamable HTTP. Each
  memory belongs to one person and is shared with the household unless they mark it private.
- **The relay lines**: every machine running Helm keeps its own outbound WebSocket open to
  `/relay`. Helm tools the hub offers over MCP travel down it, so the PC needs no inbound port.
- **The household view**: one person's Helm asks the hub about another person's chats, over the
  same lines (below).
- **Helm's Telegram bot**: one bot for everyone; each person's messages go to that person's Helm,
  and the live status and the answers come back to that person only.
- **The voice line**: a phone talks to its owner's Helm (spoken or typed turns up, spoken-style
  answers back), through `/voice/*` and a live WebSocket (below).
- **The Remote page**: a small web page at `/remote` where a phone or another computer can see the
  person's chats, send a message, stop a turn and answer what is waiting (below).

It is its own Cargo workspace and builds from `hub/` alone, so the container build needs nothing
else from the repo.

## Tools

| Tool | What it does |
|---|---|
| `memory_search {query, k?, sources?, tier?, chat?, folder?, after?, before?, parseDates?, vector?, model?, tags?, person?}` | One search over memories, watched notes and chat history (see [Search](#search)): keywords and meaning together, dates in the query understood, results ranked by source, person and age. A caller's own `vector` still works |
| `memory_read {id}` or `{ref, around?}` | One memory in full, by numeric id or by name; or a search hit by its `ref`: the chunk and `around` chunks (default 1) either side |
| `memory_write {title, body, tags?, source?, name?, scope?, visibility?, vector?, model?}` | New memory, owned by the caller's person and for that person only unless `scope` is `everyone`. A vector must come with the id of the model that made it |
| `memory_list {tag?, since?, limit?, person?}` | Newest first; `since` is an ISO-8601 date (`2026-11-01`) or UTC timestamp (`2026-11-01T09:30:00Z`), and anything else is refused |
| `memory_update`, `memory_delete` | Only when `HUB_ALLOW_MUTATE=1`, and only on the caller's own memories (`memory_update` may also change `scope`). A new body without a new vector clears the old vector |

**Whose memories.** `memory_search`, `memory_read` and `memory_list` return the caller's own
memories plus the ones other people made visible to everyone; each result says its `visibility`,
its `owner` (the person's name) and whether it is `mine`. Someone else's private memory answers
exactly like one that does not exist. The owner is always the caller's person (or the person the
call acts as, below), never something a caller can set. A memory's optional `name` is unique per
owner: two people may each keep a memory of the same name, and `memory_read` by name finds the
caller's own first, else a shared one. Memories stored before people existed (no owner) become
the legacy person's (`HUB_LEGACY_PERSON`, default `owner`), shared, the first time the hub starts,
so set `HUB_LEGACY_PERSON` to the name you pair your own PC under **before the first start** (see
`HUB_TOKENS` below; `helm-hub person merge` fixes a hub that started with the wrong one).

**Scope and person.** `scope` on `memory_write` and `memory_update` is `everyone` (the default: the
household sees it) or `me` (only the owner sees it, chosen on purpose); `private` and `shared`, and the older `visibility`, mean the same, and
naming two that disagree is an error. `person` on `memory_search` and `memory_list` is `me` or `any`
(the default: the caller's own memories and the ones shared with everyone) or a person's name or id
(only what that person wrote, among what the caller may see). Nothing crosses privacy: another
person's private memories are never returned, whatever `person` says. The descriptions tell agents
to look at other people's memories only when the user asks.

**Acting as a person.** A `/mcp` request that carries `X-Helm-Person: <personId>` (a name works
too) from a token with the `peer` scope is answered as that person: the memories it reads are that
person's and the ones it writes belong to them. A machine may always name its own person. It may
name another person only once it is allowed to act for them (`helm-hub client acts-for <machine>
<person>`, or `--acts-for` when the machine is added or paired); that is how a chat of one person,
running on another person's computer, keeps to its own person's memories. Without that allowance
the header is refused with 403, so another person's private memories never reach a machine of
someone else by default; the memories people shared with everyone stay readable by the whole
household as before. Without the `peer` scope the header is refused with 403, an unknown person
with 400; without the header nothing changes. Every call made for another person is logged with the
machine and the person (never the arguments). The forwarded Helm tools (`app_*`) never act as
anyone else: they still go down the caller's own lines.

**Saving a memory for someone else.** `memory_write` also takes `for` (a person's name or id),
which is not listed among the tool's arguments. The hub honours it only when the request also
carries `X-Helm-Approved-For: <the same person>` and the token has the `peer` scope; Helm sends that
header only after the user approved a card showing the exact text and whose memory it becomes. The
memory is then owned by that person and shared with the household (`scope` me is refused), and
`memory_read` shows `saved_by`, the person who saved it. The audit is the `memory_saved_for` table,
written in the same transaction as the memory and removed with it. The header is consent, not
security: any holder of a `peer` token can send it. `for` naming the caller's own person is a plain
write. A hub that honours `for` says so in `initialize`, under
`capabilities.experimental.helmHub.saveFor`; Helm checks that before it writes, because an older hub
ignores `for` and would save the text as the caller's own. Only the owner changes or deletes a
memory, as for any memory.

**The hub never computes an embedding.** Callers that have one (Helm on the PC) pass it; callers
that do not (another assistant, or anyone while the PC is asleep) get keyword search.

## People, machines and tokens

A **person** is a human; a **machine** (a *client*) is one Helm install, or one outside caller,
that belongs to one person and has its own token. The registry lives in the hub's SQLite file and
is read on every request, so a revoke takes effect at once.

| Scope | Reaches |
|---|---|
| `memory:ro` | the three memory read tools |
| `memory:rw` | the memory tools |
| `relay` | `/relay`: this machine's own line |
| `app` | the Helm tools forwarded down the caller's own person's lines (below) |
| `peer` | the household questions a machine's line may ask (below) |
| `telegram` | `POST /telegram/update` only: a forwarder for a shared bot (below). It stands alone |
| `voice` | `/voice/*` only: a phone's voice line (below). It stands alone |
| `remote` | `POST /remote/api/call` only: the Remote page's calls (below). It stands alone |

A paired machine gets `memory:rw,relay,app,peer`. `/mcp` needs a memory or `app` scope, `/relay`
needs `relay`, and a token with neither cannot reach what it has no scope for. A `voice` token
reaches nothing but `/voice/*`, and a `remote` token nothing but the Remote page's call: `/mcp`,
`/relay`, `/telegram/update` and the other scopes' routes answer them with 403. A new kind of
caller is a new scope in `src/auth.rs` that its route checks with `Client::has`.

Tokens are held only as SHA-256 digests and compared in constant time. `GET /health` is the one
open route and reports only status and version.

### Admin commands

Run on the server, for example `docker exec helm-hub helm-hub pair --person Sam --machine laptop`.
They need no restart.

```
helm-hub person add <name> | person list
helm-hub person merge <from> <into>                       # fold one person into another
helm-hub client add --person P --machine M [--scopes S] [--acts-for A]   # prints the token once
helm-hub client list
helm-hub client revoke <machine or id> [--person P]
helm-hub client acts-for <machine or id> <person> [--person P]
helm-hub client stop-acting-for <machine or id> <person> [--person P]
helm-hub pair --person P --machine M [--scopes S] [--acts-for A]         # prints a one-time code
```

A person that does not exist yet is made by `client add` and `pair`. Scopes are comma separated
(`memory:rw,relay,app,peer`, the default). A machine name is unique per person while it is active.
`client revoke` stops the token at once; an open line of that machine drops within 5 seconds.

`--acts-for` (people who exist, comma separated) and `client acts-for` let a machine act for other
people as well as its own: read their private memories and change their rules, skills and agents
(see "Acting as a person" and "Knowledge files"). `client stop-acting-for` takes that back, and
`client list` shows it. A machine always acts for its own person.

`person merge <from> <into>` gives everything of `from` to `into` in one go: their machines (with
their tokens, which keep working), pairing codes, the people their machines act for, their memories,
and their rules, skills and agents (each file moves to `into`'s layer, and its old copy is deleted,
so every device hears of both). Then `from` is removed. It changes nothing, and says why, when both
have an active machine of the same name, a memory of the same name or a file at the same path.
Restart the hub afterwards, so the lines and the voice line pick up the change.

### Pairing

`helm-hub pair` prints an 8-character code (`ABCD-EFGH`, no look-alike characters) that works once,
for 15 minutes. The new machine posts it:

```
POST /pair   {"code": "ABCD-EFGH"}
200 {"token", "personId", "personName", "machineId", "machineName", "scopes"}
```

The token is shown only in that answer and is not stored in the clear. Any other 4xx means the code
is bad (403 wrong, expired or already used, all alike; 409 the machine name is taken, and the code
is left unspent); 429 means too many wrong codes in the last minute (10), which locks even a good
code out for a while. The route answers only callers on this machine, the home network or Tailscale
(loopback, private, link-local and 100.64.0.0/10 addresses). Behind a reverse proxy every caller
has the proxy's address, so a request that carries `Forwarded`, `X-Forwarded-For` or `X-Real-IP`
is refused with the same 403: the proxy must not route `/pair`. Pair from the home network, against
the hub's own address and port.

### `HUB_TOKENS` (legacy)

`HUB_TOKENS=client:token[:scope],...` still works and is optional. Each entry becomes a client of
the person named by `HUB_LEGACY_PERSON` (default `owner`), with its entry name as the machine
name. The scope is `rw` (the default), `ro`, `relay`, `app`, `peer`, `telegram`, `voice` or
`remote`, joined with `+` for several (`rw+app`, `rw+relay+app+peer`; `telegram`, `voice` and
`remote` stand alone). The environment is the only place these are added or
removed: the hub mirrors it into the registry at start-up, revokes entries no longer listed, and
`client revoke` refuses them. Tokens are at least 24 characters and unique. A client name or a
token must not contain a comma, which ends the entry, or a colon, which ends the field (a token is
cut at its first colon and the rest is read as its scope, so the hub refuses to start or holds a
shorter token than you meant); use letters and digits, and `-` or `_`. A list that does not
parse stops the hub from starting. With no clients at all the hub starts and says so; nobody can
connect until one is added.

**Set `HUB_LEGACY_PERSON` to the name you pair your own PC under before the first start.** These
clients (a phone's `voice` token among them), the memories stored before people existed, the
imported memories and Telegram users with no person all belong to that person. Left at `owner`
while your PC is paired as, say, `Sam`, a phone's turns go to `owner`, who has no Helm line ("Helm
is offline" for ever), and the old memories are not Sam's. To fix a hub that already started that
way: run `helm-hub person merge owner Sam`, then set `HUB_LEGACY_PERSON=Sam` and restart the hub.
The order matters: at every start the hub gives the `HUB_TOKENS` clients back to
`HUB_LEGACY_PERSON`.

## Relay line and Helm tools

Helm connects to `ws://<hub>/relay` with its machine's token (set in Helm's Settings → Hub, which
also shows whether the line is up). Each machine has its own line: a newer connection from the
*same* machine replaces its older one, and different machines coexist. The frames are listed in
`src/relay.rs`; the `welcome` frame says which person and machine the line is, and a Helm sends a
`sharing` frame first on every line. New frames go in that table and in `Line::receive`; the
lines of all machines are kept by `src/lines.rs`. A client with the `app` scope sees six Helm tools: `app_list_chats`,
`app_read_chat`, `app_list_projects`, `app_list_attention`, `app_send_message` and
`app_stop_chat`, each with an optional `machine` argument (the hub removes it before forwarding).
A call goes only down a line of the caller's *own person*: the named machine's, or with no name
the person's most recently active one. It can never reach another person's Helm. The hub never
forwards any other name, and Helm checks every call again on the PC
(its outside-caller deny-list and argument refusals, `host/crates/relay/src/gate.rs`). A message
sent this way reaches the chat framed as sent by another agent, never as the user and never as
approval.

- **Line down (or the person has no machine online):** the call answers "Helm offline, nothing
  sent or changed." and nothing is sent.
- **Line drops mid-call, or no answer in 60 s:** "may or may not have run; check the current state
  before trying again".

## The household view

Chats stay on the machine that runs them; the hub only passes questions. A Helm sends a `peer`
frame on its line and gets a `peer_result` back (`src/peer.rs` has the rules):

| `op` | args | answer |
|---|---|---|
| `people` | none | `{people: [{id, name, you, machines: [{id, name, online, lastSeen, sharing}]}]}` for everyone |
| `list_chats` | `personId`, `machineId?`, `limit?`, `offset?` | the other Helm's own answer |
| `read_chat` | `personId`, `machineId?`, `chatId`, `beforeMessageId?`, `limit?` | likewise |
| `send_message` | `personId`, `machineId?`, `chatId`, `message` | likewise |

The hub turns the last three into a `peer_call` frame on the target machine's line (`person` is the
asker's display name) and relays that Helm's ordinary `result` as `peer_result`. With no
`machineId` it uses the person's most recently active machine that allows the op.

Each Helm says what the household may do with its chats in `sharing` frames (`chats`,
`transcripts`, `messages`; all off until sent, and reset when the line drops). The hub enforces
them for other people's machines: `list_chats` needs `chats`, `read_chat` needs `chats` and
`transcripts`, `send_message` needs `transcripts` and `messages`. A person's own other machines need
no switch. A refusal is always a plain sentence (`ok: false, error`), never silence; the asker's
token needs the `peer` scope; an offline machine says it is offline. Nothing is stored on the hub.

## Devices

The whole hub is one household. Each Helm tells the hub which device it is with a `device` frame
(`uid`: an id the install made for itself, 8 to 64 letters, digits or dashes; `name`, at most 40
characters; `run`: whether other devices may run its chats), after `sharing` on every line-up and
when something changes. Only a token with the `peer` scope takes part; the hub keeps the device on
the machine's registry row (columns `device_uid`, `device_name`, `run`), so a device is still listed,
offline, while its Helm is away. A device id held by another person's machine is refused; the same
person's older machine gives it up (a Helm paired again keeps its id). Revoking a machine removes its
device. The hub never uses a client's address to tell devices apart: it runs behind a reverse proxy.

| Frame | Fields |
|---|---|
| hub → Helm `devices` | `list: [{uid, name, personId, person, online, lastSeenAt, run, self}]`, to every `peer` line when a `device` frame arrives and whenever a line comes up or drops. `personId` is the hub's person id as a string (here and in `from` below) |
| Helm → hub `dev` | `id`, `to` (a device uid), `op`, `args` |
| hub → Helm `dev_call` | `id` (the hub's own), `from: {uid, name, personId, person}`, `op`, `args` |
| Helm → hub `dev_result` | `id` (the `dev_call`'s), `ok`, `result` or `error: {code, message}` |
| hub → Helm `dev_result` | `id` (the `dev` frame's, as sent), `ok`, `result` or `error: {code, message}` |
| Helm → hub `dev_push` | `to` (a device uid), `sub`, `seq`, `event` |
| hub → Helm `dev_push` | `from` (the sender's device uid), `sub`, `seq`, `event` |

The hub never reads `op` or `args`; it carries the call to the device with that uid, waits up to 30
seconds, and returns the answer under the sender's id. Its own errors are `scope` (no `peer` scope),
`bad_request` (the sender has not announced a device, or `op` is empty or over 64 bytes), `offline`
(no such device, or it is not up), `timeout` (no answer in 30 s, or the target's line dropped mid
call: it may or may not have run), `too_large` (a call or an answer over 1 MiB) and `busy` (the
sender already has 8 calls waiting; the call was not passed on). A `dev_push` for
a device that is not up is dropped without a word. Frames the hub does not know are ignored, so an
older Helm is not disturbed. The largest frame the hub takes from a Helm is 4 MiB (it was 2 MiB; a
knowledge file needs the room); every voice limit is unchanged.

## Knowledge files

The rules, skills and agents of a household travel through the hub (`src/knowledge.rs`), in the
same SQLite file as the memories (table `knowledge_files`). A *layer* is `everyone` or
`person:<personId>`; a file is a path under `rules/`, `skills/<name>/` or `agents/` (at most 8
folders deep and 200 bytes; no segment starting with a dot or ending in a dot or space; none of
`\ : * ? " < > |`; no Windows device names; two paths may not differ only by letter case), up to 2
MiB, with up to 64 MiB and 5000 files in a layer. Every change takes the next number of one counter
(`seq`); a deletion stays as a row without a body, so devices that were off still hear of it. All
four frames need the `peer` scope. Every `peer` line reads every layer and may write `everyone`; a
person's layer is changed only by that person's own machines, or by a machine allowed to act for
them (`helm-hub client acts-for`, above), and such a change from another person's machine is
logged. Any other put to it is refused with a plain sentence.

| Frame | Fields |
|---|---|
| Helm → hub `kn_pull` | `id`, `since` (a `seq`), `limit` (at most 200) |
| hub → Helm `kn_files` | `id`, `files: [{layer, path, seq, sha256, size, deleted, body}]` (body in base64, left out when deleted), `cursor`, `more`; or `id` and `error` |
| Helm → hub `kn_put` | `id`, `layer`, `path`, `baseSeq` (the `seq` last seen for the path, 0 for a new file), `sha256`, `deleted`, `body` (base64) |
| hub → Helm `kn_put_result` | `id`, `ok: true`, `seq`; or `ok: false` with `conflict: {seq, sha256, deleted}`; or `ok: false` with `error` (a plain sentence) |
| hub → every `peer` line `kn_changed` | `seq`, after a put that changed something |

A `kn_files` reply holds about 1 MiB at most (a bigger single file is sent alone); ask again from
its `cursor` while `more` is true. A put whose content equals what is stored is accepted as it is
(same `seq`, no `kn_changed`). A put from a stale `baseSeq` is a `conflict` and nothing is written:
the hub keeps its copy and the writer decides what to do with its own. A deleted path can be written
again with `baseSeq` 0.

## Telegram bot

| Variable | |
|---|---|
| `HUB_TELEGRAM_BOT_TOKEN` | The bot's token from BotFather. Without it the bot is off |
| `HUB_TELEGRAM_ALLOWED_USERS` | Comma-separated numeric Telegram user ids, each optionally followed by `:person` (`123456:Sam`). A person's messages go to that person's Helm and the replies, status and approval buttons go back to that Telegram user only; an id with no person belongs to `HUB_LEGACY_PERSON`. Empty: the bot is off (it denies everyone) |
| `HUB_TELEGRAM_API` | Bot API base, default the public API (tests point it at `fakes/telegram`) |
| `HUB_TELEGRAM_QUEUE_MINUTES` | How long a message waits while Helm is offline (default 120) |
| `HUB_TELEGRAM_QUEUE_MAX` | How many messages wait (default 50) |
| `HUB_TELEGRAM_RECEIVE` | `poll` (default) or `off`, below. Anything else: the hub refuses to start |
| `HUB_TELEGRAM_STT_URL` | An OpenAI-compatible speech-to-text endpoint for voice notes, e.g. `http://<host>:<port>/v1/audio/transcriptions`. Unset: recordings go without a transcript. Not an `http://` or `https://` URL: the hub refuses to start |
| `HUB_TELEGRAM_STT_MODEL` | The model the server is asked for (default `large-v3-turbo`) |
| `HUB_TELEGRAM_STT_LANGUAGE` | The language of the voice notes (default `en`) |
| `HUB_TELEGRAM_STT_TIMEOUT_MINUTES` | How long one transcription may take, 1-60 (default 10). A long recording is split up by the server and can take minutes. Optional |

**Upgrading to a hub that forwards files:** install the matching Helm on the PC first, then
redeploy the hub. An older Helm drops any frame over 32 MB (a message with files can be up to
about 67 MB as base64) and ignores the `files` list, so files would be lost or stall the line.
`HUB_TELEGRAM_STT_TIMEOUT_MINUTES` is new and optional; nothing else in the env file changes.

- A sender not on the allow-list gets nothing: no forward and no reply.
- With several machines, a person's message goes to their most recently active connected line (it
  attached, or handled a Telegram frame, last). A message that line has not acknowledged stays with
  it, and is sent again only there, while that line is up, so two machines never both answer it;
  it moves on only when that line drops. Each person has their own queue, status message and asks.
- While Helm is offline a message is queued in order and the sender is told at once, with what the
  hub can do about the PC (see "Starting and waking the PC"): that Helm is being started, that the
  PC looks off or asleep (with a Wake button), or, when the hub can do nothing, the one setting that
  would let it. On reconnect the queue is delivered, each message carrying its age so Helm marks it
  "sent N min ago". An expired message is dropped and the user is told how many, and to send it
  again if it still matters. `/cancel` drops the queue; `/stop` stops Helm's running turn;
  `/starthelm` and `/wake` start Helm on, or wake, the person's PC while Helm is offline. The queue
  is in memory: a container restart drops it.
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
  message carries at most 10 files and 50 MB; about 150 MB (counted as base64, which is what the
  hub holds, a third more than the files) may wait for Helm at once, for the whole household
  together. A file over
  any of these is left out with a reply, judged from the sizes the message gives before anything is
  downloaded and again once it has arrived. Two files are fetched at a time; a file's turn is given
  back as soon as it is downloaded, so a long transcription never holds up the downloads behind it.
  A message keeps its place in the queue while its files
  are fetched, so a message sent after it waits for it. A message whose files never finish arriving
  is dropped after 75 minutes with a reply of its own. When every file of a message fails, its
  caption still goes as text, beside the replies that say which files were left out. A reconnect
  sends the queued messages one frame at a time, and stops after about 64 MB of base64 until the
  next half-second tick, so it never builds every frame at once. File bytes are never logged. Helm
  makes each name safe for its disk and saves the file where the chat's agent can read it.
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
  One recording is transcribed at a time, and the hub asks nothing of Helm first: it assumes
  nothing about where the speech-to-text server runs. When a transcription is still running after
  30 s, the bot sends one quiet message, "Still transcribing your voice note…", to that chat and
  deletes it when the transcription ends, whether it gave a transcript or a failure.
- **Long messages arrive whole.** Telegram cuts a long message at 4,096 UTF-16 units and delivers
  the parts in a burst. A text message of 3,500 units or more (counted in UTF-16 units, as
  Telegram counts, so emoji count twice) is therefore held for 2 s. A text message that follows it,
  while it is still the last in that person's queue, joins it with nothing put between the parts;
  a part that is itself that long keeps the window open, and a shorter part ends it and the joined
  message goes at once. An ordinary message is never held, and a command or a file is never held
  or joined. A burst gathers at most 300,000 units (the rest starts a message of its own), and a
  message of more than 100,000 units goes to Helm as an attached `message.txt` with a one-line
  text saying so, not as text. Each person's bot has its own queue, so one person's burst never
  joins another's. A message sent within the window after an unsplit long one joins it too: that
  is the cost of the wait.
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

## Starting and waking the PC

Nothing on the PC listens for the hub, so the hub cannot start Helm by itself. Instead each PC can
run a small launcher that keeps one outbound WebSocket to `GET /launcher` (the same `relay`-scope
token as `/relay`; the frames are in the table at the top of `src/launcher.rs`) and starts Helm when
the hub asks. The launcher is on in Helm's Settings → Hub ("Start Helm when a message arrives"). A
reverse proxy in front of the hub must route `/launcher` as it does `/relay`; the home network
address needs nothing.

When Helm is offline and a Telegram message or a voice turn waits, the hub picks the person's PC (the
one they pinned, the one that took their last message, their own PCs, then PCs that act for them;
one whose launcher is up first, else one it knows how to wake) and does one of:

| What the hub sees | What happens |
|---|---|
| Launcher up, Helm not running | Asks the launcher to start Helm by itself, at most once every 2 minutes for a PC, and says "Helm is starting on <PC>". Helm starts hidden, in the tray |
| A launch is under way | Says so; sends nothing again |
| Launcher up, Helm running, not connected yet | Says Helm is running and reconnects by itself |
| No launcher, and the hub knows how to wake the PC | Says the PC looks off or asleep, with a **Wake** button. Waking is always a person's tap or command, never automatic, and at most once a minute for a PC |
| Anything else | Tells the sender the one step that would let the hub start Helm |

A launch that has not brought Helm's relay line up after 2 minutes is reported once, on Telegram,
with a **Try again** button (and **Wake** when the launcher line has gone too); the hub does not
launch by itself again for that PC until a person asks or its relay line has come up. Telegram's
`/starthelm` and `/wake` do the same as the buttons; they, and the buttons, only act on one of the
person's own PCs, or one that acts for them.

The phone is told too: while Helm is offline `GET /voice/status`, the `202` of `POST /voice/turn`,
the `409` of `POST /voice/fresh`, the `429` of a turn when the queue is full, the live socket's
`welcome` and its `helm` frame carry `"pc": {"state": "starting"|"connecting"|"asleep"|"closed"|"unknown",
"machine": <name or null>, "canStart": bool, "canWake": bool}` (`closed`: the launcher is up and
nothing is under way). `POST /voice/pc` with `{"action": "start"|"wake"}` answers `202 {"ok", "pc"}`,
`409 {"error"}` when it cannot be done, or `429` when the PC was asked a moment ago. The hub picks
the PC. A phone that does not know the field ignores it.

| Variable | |
|---|---|
| `HUB_WAKE` | `off` (the default), `udp` or `ha`. Off: no Wake button is ever shown. Anything else: the hub refuses to start |
| `HUB_WAKE_BROADCAST` | For `udp`: the LAN's broadcast address (`192.168.1.255`); without it the hub uses the address the PC's launcher reported. Also sent to Home Assistant for `ha` when set |
| `HUB_WAKE_HA_URL`, `HUB_WAKE_HA_TOKEN` | For `ha`: Home Assistant's address (`http://...` or `https://...`) and a long-lived access token. The hub calls `wake_on_lan.send_magic_packet`. The token is a secret: it belongs in the env file, never in a command line, and the hub never logs it |

A hub in a container on Docker's default bridge network often cannot send a broadcast out to the
LAN. Home Assistant, which normally runs on the host network, can: prefer `ha` there. The PC's
hardware address always comes from its own launcher (it is kept with the machine and survives a
dropped line); the hub only keeps a hardware address of six hex pairs and a broadcast address in a
private IPv4 range, so a PC cannot aim the hub at an outside address.

**On the PC, for a wake to work:** enable Wake on LAN in the BIOS, and on the wired network adapter in
Windows Device Manager turn on Power Management → "Allow this device to wake the computer" (and,
if you like, "Only allow a magic packet") and Advanced → "Wake on Magic Packet". Wi-Fi usually
cannot wake a PC. After a wake the PC has to reach Helm's launcher (it starts at logon), and the hub
then starts Helm for what waits.

**The hub restarts itself when it hangs.** The image's health check only marks a hung container
"unhealthy"; Docker does not restart it for that. A watchdog inside the hub exits the process when its
async runtime has not answered for `HUB_WATCHDOG_SECS` (default 180; `0` turns it off), and Docker
brings it back. This needs `restart: unless-stopped` on the container, as in `compose.snippet.yml`;
check that the compose file the home server really runs has it. The queues are in memory, so a
restart drops what waited.

## The voice line

A phone (the phone app) talks to its owner's Helm: it uploads a recording or typed text, the hub
turns a recording into text on the speech-to-text server, hands the turn to Helm down the relay
line, and carries the answer back on a live WebSocket. Each phone has a token with the `voice`
scope, made like any other client (`helm-hub client add --person Sam --machine phone-voice
--scopes voice`, or a `HUB_TOKENS` entry `phone-voice:<token>:voice`, which belongs to
`HUB_LEGACY_PERSON`: set that to the person the phone is for, see `HUB_TOKENS` above); it reaches
`/voice/*` and nothing else, and it speaks for its token's person only. Several phones of one person each get their own live
socket, their own acknowledgements and their own screen state, and all of them see that person's
replies. The protocol is "voice v1" and the phone app is built against it: the route paths, the
frames, the limits and the token shape do not change without the phone app changing too.

| Variable | |
|---|---|
| `HUB_VOICE` | `on` (the default) or `off`. On, the routes exist whether or not any voice token does; with none, every call to them answers 401 or 403 like any route without its token. Off: every `/voice/*` route answers 404 (to every caller, a voice token too) and the relay line drops the voice frames. Anything else: the hub refuses to start |
| `HUB_VOICE_QUEUE_MINUTES` | How long a turn waits for Helm to take it, 1-1440 (default 30). The phone is told `expired` after that |
| `HUB_VOICE_QUEUE_MAX` | How many turns may wait for Helm at once, 1-200 (default 20); one more answers 429 |
| `HUB_VOICE_OUTBOX_MINUTES` | How long a reply or news item waits for the phone to acknowledge it, 1-10080 (default 120) |

Recordings are transcribed by the same server as Telegram's voice notes (`HUB_TELEGRAM_STT_URL`,
`_MODEL`, `_LANGUAGE`, `_TIMEOUT_MINUTES`), whether or not the bot is on. One recording is
transcribed at a time, apart from Telegram's, and Helm is asked to free the PC's graphics memory
first (see the bot's Recordings). Without `HUB_TELEGRAM_STT_URL` a recording is refused
(503) and typed turns still work. Recordings waiting to be transcribed share one budget of 16 MiB
for the whole hub: a recording that does not fit is refused at once with 429 "The hub is busy; try
again in a moment." (judged by its `Content-Length` before it is read, and by its real size after),
and each gives its share back once it is transcribed. An upload has 120 s to arrive. Everything is
held in memory: a restart drops the queue and the outbox. The outbox's `seq` then starts from the
clock (the time the hub started, in milliseconds), so it is above any `lastSeq` a phone kept from
before the restart and the phone is sent every new entry.

Every route needs `Authorization: Bearer <voice token>` (the WebSocket upgrade too). No or a bad
token: 401 with `WWW-Authenticate: Bearer`. A token of another scope: 403. Error bodies are
`{"error": "<a short sentence for people>"}`.

| Route | |
|---|---|
| `POST /voice/turn?turn=<id>&mode=conversation\|chat[&continues=<id>]` | A new turn. The body is `audio/ogg` (Opus), `audio/wav` (16 kHz mono) or `application/json` `{"text": "..."}` (1-8000 characters, no speech-to-text). The `turn` id is the phone's own (8-64 of `A-Za-z0-9_-`), so a retry is safe. `202 {"ok", "turn", "already": false, "helm": "online"\|"offline", "queued": true}` (and `"pc"` while Helm is offline, see "Starting and waking the PC"); the same id again `200 {"ok", "turn", "already": true, "state"}` (a turn that ended `failed` or `expired` is accepted again). 400 for a bad id, mode, empty or too-long text or an unknown `continues`; 413 for a body over 8 MiB or a recording over 130 s; 415 for another type; 429 when the queue is full ("Helm is offline and the queue is full, so this wasn't kept." while Helm is offline, "Too many turns are waiting; try again in a moment." while it is online but has not taken them yet) or the hub holds too many recordings; 503 for audio with no speech-to-text. Only this route lifts the 4 MiB request limit, to 8 MiB; a larger upload is read to its end and thrown away so the phone gets its 413 |
| `POST /voice/heard` | `{"reports": [{"reply", "state": "all"\|"partial"\|"none", "sentence"?, "charOffset"?, "replay"?, "at"?}]}`, 1-50 reports, at most 64 KiB (413 over). The latest report per reply is kept (at most 200 are held) and forwarded to Helm as `voice_heard` when a line is up. `200 {"ok", "forwarded": n}` |
| `GET /voice/status` | `200 {"ok", "helm": "online"\|"offline", "version": 1, "stt": true\|false}`, and `"pc"` while Helm is offline. Proves the address and token work; never touches speech-to-text |
| `POST /voice/fresh` | Tell Helm to start a new voice conversation: `202 {"ok": true}`, or `409 {"error", "pc"}` when Helm is offline (it is not queued) |
| `POST /voice/pc` | `{"action": "start"\|"wake"}`: ask the hub to start Helm on the PC, or wake the PC. `202 {"ok", "pc"}`, `409` when it cannot be done, `429` when it was asked a moment ago (see "Starting and waking the PC") |
| `GET /voice/live` | The WebSocket (below) |

**The live socket.** One per voice token: a newer socket of the same token replaces the older, which
is closed with code 4000 "replaced"; the person's other phones keep theirs. The first frame from the
phone must be a `hello` (`lastSeq`, `live`, `mode`, `app`) within 10 s, or the socket is closed
(1008). The hub answers `welcome` (`version`, `helm`, `seq`, the newest seq it holds) and replays
every kept frame above `lastSeq`. Because a restarted hub's seqs start from the clock, its
`welcome.seq` is above the phone's `lastSeq` and the phone keeps its place; a `lastSeq` above the
hub's newest seq (a clock set back) gets everything the hub holds. Then the phone sends `ack` (cumulative), `presence` and
`cancel`, and the hub sends `turn` (a turn's state: `transcribing`, `heard`, `empty`, `queued`,
`delivered`, `failed`, `expired`, `cancelled`), `status` (Helm's working line, at most every 2.5 s,
never kept), `reply` (an answer or news, kept and replayed) and `helm` (the line came up or went
down). The hub **pings every 20 s**, well inside the 60 s after which a reverse proxy in front of
it closes an idle socket (the phone pings on its own schedule too), and drops a socket silent for
90 s; a revoked token drops its socket within 5 seconds. Phone frames are at most 16 KiB. Frames
to the phone have no cap of their own: a reply with four 1 MiB attachments is about 6 MB, which
the phone takes. The hub never looks at the caller's address: behind a proxy it is always the
proxy's.

**Queues.** A turn goes to Helm in upload order and is sent again every 45 s until Helm sends
`voice_ack`; a turn still being transcribed holds back the ones behind it. With several PCs, a turn
goes to the person's most recently active line and stays with that PC (it is sent again, and a
cancel's `voice_stop` goes, only there) while its line is up, so two PCs never both answer it; it
moves to another PC only when that line drops. While Helm is offline turns wait (`turn: queued`)
and go when a line comes up, each marked with its age. Replies and the final states of turns wait
in the outbox (at most 200 entries per person, and 24 MiB of attachment data for the whole hub; the
oldest attachment bytes of the person whose reply went over go first, and the attachment is marked
`dropped`) until every phone of the person has acknowledged them or the outbox time passes. An
attachment Helm sends before its reply is held for 60 s waiting for it (at most 6 MiB of them for
the whole hub). Helm's frames are in the table in `src/relay.rs` (`voice`, `voice_stop`,
`voice_heard`, `voice_presence`, `voice_fresh` down; `voice_ack`, `voice_status`,
`voice_attachment`, `voice_final`, `voice_notify` up). The relay line accepts frames up to 4 MiB,
so one picture of 1 MiB fits easily.

**Upgrading:** install the matching Helm on the PC first, then redeploy the hub; an older Helm
ignores the voice frames and never acknowledges a turn. Put the `voice` token in the phone's
settings, never in the repo or the chat.

## The Remote page

A small page for a phone or another computer, served by the hub at `/remote` (`/remote/app.js` and
`/remote/app.css` are its other two files, all built into the hub). It shows the person's chats and
what is waiting for them, lets them read a transcript, send a message, stop a turn and answer what
waits, all against the person's own Helm over the relay line. The hub keeps nothing about chats.

**Turning it on.** Nothing is open until a person has a `remote` token: make one with
`helm-hub client add --person Sam --machine phone-remote --scopes remote` (or a `HUB_TOKENS` entry
`phone-remote:<token>:remote`) and paste it into the page, which keeps it for that browser tab only. The
token stands alone: it reaches nothing but the page's call. **Answering is a second switch, off
until the person turns it on in Helm's Settings → Hub** ("Allow answering waiting prompts from the
Remote page"); with it off the page shows what waits and says so. A permission, a plan or a
question can then be answered with a tap; one whose details are too long to read on a phone offers
only the options that allow nothing; secret inputs, folder-trust prompts and consent requests are
never offered, and an answer from the page is never consent. A message typed on the page reaches the
chat framed as the user's own words from the Remote page, and still as no approval.

**Reach.** The page needs no address check: it is served wherever the hub is (the home network and
Tailscale), and in production behind the same HTTPS reverse proxy as the rest. Nothing in it relies
on the client's address; the token is the only credential, so keep the hub off the open internet
unless the proxy asks for a login.

| Route | |
|---|---|
| `GET /remote`, `/remote/app.js`, `/remote/app.css` | The page. Open (a browser cannot send a header when it opens a page) and secret-free. Every response carries `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`, `nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store` and `X-Frame-Options: DENY`; no CORS; no inline script; chat text is shown as text only |
| `POST /remote/api/call` | `{"name": "app_list_chats", "args": {...}, "machine": "Desk"?}`. A `remote` token only (401 no or bad token, 403 another scope or a tool the page may not call, 400 a body that is not a call, 413 over 64 KiB, 429 over the allowance with `Retry-After`). Always `200 {"ok", "result"\|"error", "offline"?, "unknown"?, "line", "machines"}` for what Helm says: `line` is whether the chosen line is up, `machines` the person's connected computers |

Only the six Helm tools of the `app` scope and the page's own two (`app_list_waiting`,
`app_answer_waiting`) can be called. The two are not offered over MCP, so an agent cannot answer a
prompt through the hub. A call goes down one of the token's **own person's** lines (`machine`
names one, the page shows a choice when more than one is connected, the default is the most
recently active), never another person's. The hub stamps each call `"scope": "remote"` on the
line, and Helm refuses the page's two tools (and the household too) without it, then checks the
call again on the PC. Line down: "Helm offline, nothing sent or changed." No answer in 60 s: "may
or may not have run". The log line is the token's machine name, the tool and the time, never the
arguments or the token.

**Limits.** Per token, 30 calls a minute that change something (a message, a stop, an answer) and
240 reads a minute. The page reads at most a few times every five seconds (the open view every
tick, the Waiting list every third tick when another view is open), skips a read while the last
one is still out, and pauses when told to wait, so looking never uses up what an answer needs.
What is being typed in an answer survives every refresh.

## Commands

```
helm-hub serve                       # HUB_DB, HUB_BIND, HUB_TOKENS, HUB_LEGACY_PERSON, HUB_ALLOW_MUTATE, HUB_TELEGRAM_*, HUB_VOICE*, HUB_WAKE*, HUB_WATCHDOG_SECS
helm-hub import <dir> --source S [--prune] [--retag]   # S: the source tag the imported memories carry (required)
helm-hub share-existing [--dry-run] [--keep ID,ID] [--list FILE] [--revert]   # the one-off private -> shared move (below)
helm-hub person|client|pair ...      # people, machines and pairing codes (see above)
```

`import` reads the `*.md` files of a folder and its subfolders (hidden folders such as `.git` are left out). Frontmatter `name`,
`description` and `type` (or `metadata.type`) are kept, and so are the body and its `[[wiki links]]`.
The memory's title is the body's first `# heading`, else its name. A file with no frontmatter is
keyed on its file stem. The import is idempotent on `name`: unchanged files are left alone, changed
files are updated in place, and a changed body drops that memory's stored vector, since the vector
described the old text. `MEMORY.md` and `README.md` are index files and are skipped. Two files
claiming one name are reported as failures rather than overwriting each other. Imported memories
are tagged `imported`, the source tag, and their type, and carry the file's path below the folder
as their source (`<tag>:sub/a.md`). They belong to `HUB_LEGACY_PERSON` and are shared with the
household, so a file whose text looks like a credential (a key block, a token, a password line) is
reported as failed and left out, whatever else it holds.

Retired memories are never imported: anything under a folder named `archived` (or ending `.bak`),
any `*.bak.md` file, and any file whose frontmatter `status` (top level or under `metadata`) is
`archived` or `superseded`. They are listed as skipped. With `--prune`, a memory of the source that
was imported before such a rule applied is removed on the next run.

A folder keeps the source tag it was first imported under, recorded once an import has finished (a
failed import leaves it as it was): importing the same folder (by its real
path) under another tag is refused, because every memory would be written a second time beside the
old ones. `--retag` says the source really did change; the run then says how many memories it
rewrote. A hub from before this check has no record of its folders, so the first import after an
upgrade is not compared with anything: always pass the source tag the folder was imported under
before (the tag its memories carry), never a new one. The index door follows the same rule: a
`folder` source's doc whose key sits under `archived` or a `.bak` folder, or is a `*.bak.md` file, is
refused with "not indexed", whichever machine sends it.

`--source` is required, so a re-import cannot re-tag memories under another source by accident; keep
using the same tag for the same folder. `--prune` then removes the memories of that source (and
only that source) whose files are gone or whose `name` changed. Memories written by agents, or
imported under another source, are never touched. A file that failed to import keeps its old
memory, and a run that finds no memory files prunes nothing (an unmounted folder looks empty).

### Moving existing private memories to shared

Memories written before sharing was the default are private to their owner, and importing a folder
again does not change a memory's visibility, so one source can end up mixed. `helm-hub
share-existing` moves them in one reversible step, at the redeploy:

1. `helm-hub share-existing --dry-run` lists every private, owned memory (id, name, source, title)
   and changes nothing. Read it: the hub does not record whether a memory was made private on
   purpose or only by default, so any you want to keep private go in `--keep`.
2. `helm-hub share-existing --keep 12,40` backs the database up next to it
   (`hub.db.before-share-<time>.bak`), moves the rest (and their search copies) to shared, writes
   the moved ids to `share-existing-<time>.txt` beside the database (or the `--list` file) and
   records them in the `visibility_flips` table.
3. `helm-hub share-existing --revert` puts the latest run back to private. A memory someone has
   changed since stays as it is.

Notes and sessions that a PC indexes are not touched: their visibility is the source's own setting
on that PC and arrives with its next index pass.

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
the container: `docker exec helm-hub helm-hub import /mnt/memory --source my-memory`.

**Who owns the data folder.** The compose file runs the container as user `1000` and group `100`
(`user: "1000:100"`), not as root. The folder mounted at `/data` (it holds `hub.db` and its WAL
files) must therefore be writable by that user and group: `chown -R 1000:100 ./helm-hub/data` once,
before the first start, or the hub cannot open its database. A folder left owned by root, for
example one the Docker daemon made itself, fails the same way. The memory folder at `/mnt/memory`
is mounted read-only and only needs to be readable by that user.

## Search

Everything searchable is a *doc* made of *chunks* in `ix_*` tables beside `memories` (`src/index.rs`).
A memory is mirrored as a doc of the `memory` source in the same transaction as every write
(insert, update, delete, import, person merge), and a store from before the index gets its
mirrors when it opens, once. Notes and chat history are pushed by a person's Helm over the relay
line. Nothing here changes what the memory tools did: a memory hit keeps its old fields.

**Privacy.** A viewer sees the docs they own, the ones shared with everyone, and the ones that name
them as a reader. That filter is part of every candidate query, so another person's private text is
never a candidate. Their *shared* items are down-weighted to 0.3 but still win when they are the
only match.

**Ranking** (`src/rank.rs`, one table of constants): keyword (bm25: title 8, heading 4, text 1)
and vector candidates are fused by reciprocal rank, then multiplied by the source (memory 1.0, note
0.8, conversation 0.5), by 0.3 for another person's item unless `person` names them, and for
conversations with no date window by age (half-life 120 days, floor 0.6). A doc scores its best
chunk plus a tenth of its second and shows at most two chunks (a memory one); a chunk whose vector
is within 0.95 of a higher one is dropped. A result is `{kind: memory|note|chat, ref, title, where,
at, owner, snippet, score, matched}`, with `ref` `m:<id>` or `d:<doc>#<chunk>`, and the answer's
`searched` says `{sources, window?, widened?, meaning}`. `meaning` is `pc` or `nas` (who embedded
the query), `caller` (a `vector` was sent) or `off: <why>` (keywords only).

**Dates.** `after`/`before` (ISO) win; otherwise a phrase in the query (`when.rs`) becomes a window
on the passage's time. A window with fewer than three docs widens by half, once (`widened`).
`parseDates: false` turns the phrases off. The zone is the container's `TZ` (`Europe/London`, else UTC).

**Vectors.** int8 on disk (a scale and a byte per dimension), keyed by model (the `:tag` is
dropped, so `nomic-embed-text:latest` is `nomic-embed-text`); the hub also keeps one sign bit per
dimension of every vector in RAM (about 20 MB at 200k chunks), scans the allowed docs' codes by
Hamming distance and re-scores the closest 200 exactly. It never embeds documents. A query is
embedded by the asker's own PC (`ix_embed`, 600 ms; a PC with busy local models answers `locked`),
else by the server at `HUB_EMBED_URL` (OpenAI-style `/v1/embeddings`, 2.5 s, one query at a time),
else not at all. Queries go out as `search_query: <text>`, documents carry no prefix. The last 512
query vectors are kept.

**Trace.** `tier: "trace"` with `chat` searches the working steps of one chat. The hub checks the
asker may read the chat, then asks the PC that holds it (`ix_trace`, 3 s); with the PC off the
answer says `trace: "unavailable while the PC is off"`.

**Index frames** (peer scope; see the table in `src/relay.rs`): `ix_hello` registers sources and
returns each one's doc count and digest (sha256 of the lines `key:hash`, sorted by key bytes and
joined by newlines); `ix_manifest` pages `{key, hash}`; `ix_put` stores up to 200 chunks and 2 MiB
of docs, one transaction per doc (`inserted`, `replaced`, `unchanged` or `refused`: the owner must
be the machine's person or one it may act for, and a source belongs to the first machine that names
it; a chunk whose text is unchanged keeps its vector); `ix_delete`; `ix_pending` lists chunks with
no vector for the hub's model, only of docs the machine's people may read; `ix_vectors` stores
them. The welcome frame carries `features: ["index"]`, so a Helm on an older hub knows to wait.

Environment: `HUB_EMBED_URL` (unset: only a PC embeds queries), `HUB_EMBED_MODEL` (default
`nomic-embed-text`), `TZ`. The database work of a search and of every index write runs on the
blocking pool, so the hub's one thread keeps serving Telegram and the lines meanwhile.

## Budget

One-thread runtime, two SQLite connections (memories, and the registry that token checks read), a
16 MB page cache, plus about 20 MB for the vector codes at 200k chunks (search is a Hamming scan of
the codes, not of the vectors). A release build idles at about 7 MB
of working set with ~340 memories loaded (measured on Windows). Vector search is a Hamming
scan of the codes, a few milliseconds at 200k chunks.

**The peak.** Idle is small, but the queues may fill, and every one of them is capped for the whole
hub: Telegram files waiting for Helm about 150 MB (as base64), plus up to 64 MB of frames a
reconnect builds at once and two downloads of up to 20 MB in flight; voice recordings waiting to be
transcribed 16 MiB, attachments held for a reply 6 MiB and the voice outbox's attachments 24 MiB;
one relay frame of up to 4 MiB per line. With everything full at once that is about 320 MB, so
`compose.snippet.yml` gives the container 512 MB (`mem_limit: 512m`). A hub that never receives
Telegram files stays far below that; a smaller limit only risks the container being restarted when
the queues fill.
