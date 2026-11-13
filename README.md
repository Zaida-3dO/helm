# Helm

**One window for all your AI coding agents.**

Helm is a desktop app for running, watching and steering many Claude Code agents at once. Every chat
is a real Claude Code session working in a real folder. Helm keeps them organised and visible, keeps
them going when you look away, and puts everything they need around them: files, diffs, a terminal,
pull requests, notes, search and a browser for pages they make.

Start a chat, give it a job, and move on to the next one. Helm tells you when an agent needs you,
shows you what it did, and keeps the session alive through quits, crashes, restarts and sleep.

> Windows-first. Linux is a secondary target; macOS is not supported.

## Why Helm

- **Run many agents without losing track.** Chats sit on project boards with titles, icons, unread
  marks and a Needs attention inbox, so you always know which agent is waiting on you and why.
- **See the work, not only the words.** Live checklists while an agent works, a recap card when it
  finishes, every tool call and subagent on a timeline, and diffs one click away.
- **Your own sign-in, your own machine.** Helm drives the unmodified `claude` binary under your own
  Claude Code login. Nothing is proxied; your chats live in a profile folder on your PC.
- **Agents that can run the show.** Agents can start chats, hand work to each other, wait on builds
  and pull requests, and report back, with a workflow that takes a plan through build, review and
  merge.
- **Local models when you want them.** Run a chat on a model server on your own machine (Ollama,
  Strata) and switch between it and Claude mid-conversation.
- **Away from your desk, still in charge.** Reply from Telegram, talk to your agents by voice, and
  let Helm keep work moving while you are away.

## Features

### Chats
- Every chat is a Claude Code session in a project folder, with its own worker process. It
  survives a quit, a crash or a relaunch and picks up where it left off.
- Model and effort picker, permission modes (including plan mode), and a model switch at any
  point in a chat, between Claude models or to and from a local model.
- Fork from any message, **Go back** to before a message, roll a long chat over into a fresh one with
  a hand-off, and suggested titles.
- Messages you send while an agent is working are delivered at its next safe point: nothing waits
  in a queue.
- Chat cost: tokens, estimated dollars and share of your weekly plan, per chat and per project.

### The composer
- Attach files, folders and screenshots (paste or drop), mention other chats with `@`, paste long
  text as an attachment, and use a `/` menu for skills and commands.
- Dictation: speak into the composer through your own speech-to-text server.
- Autocomplete as you type, and Send or Steer while an agent is busy.

### Seeing the work
- Streaming replies, a work timeline, and subagents as cards you can stop one at a time.
- A live checklist during a turn and a recap card at its end.
- Agents can show you images as swipeable galleries and offer **option boards** you answer with a
  tap.
- Approval, question and plan cards, a Needs attention inbox, find in chat, and a plan-usage meter
  with your live 5-hour and weekly limits.

### Projects and boards
- Project boards group chats by what they are for; each project can have notes every chat reads.
- Projects can be closed and reopened, chats can be muted, nested, pinned, shared and archived.
- Git worktrees per chat when a repository wants them, and a guard that keeps agents out of the
  main checkout when it does not.
- Search everything with Mod+K, and browse, open and fork your Claude Code and Claude Desktop
  chats from inside Helm.

### The side panel
- Files with a live tree, Changes with diffs, staging and review comments, and quick open
  (Ctrl+P, or tap Shift twice).
- Pull requests: read, comment, react and review, with split diffs.
- Notes from your Obsidian vaults, with links that open the right note.
- **Pages:** HTML pages your agents build run in a sealed-off frame, with their network off unless
  you allow it, and can be pinned to come back to.
- A terminal dock under the transcript.

### Agents that coordinate
- An orchestrator workflow: plan, build, review and land, with review rounds capped and lessons
  recorded when a run goes badly.
- Ready-made agents (planner, coder, reviewer, plan reviewer, PR clean-up, smoke tester) and skills,
  editable in Settings.
- Agents can start and message other chats, declare waits (a build, a pull request, another chat)
  and let Helm do the waiting, flag you for attention, schedule prompts and change settings, each
  behind a consent step you control.
- Skills, rules and memories in layers (yours, a project's, shared), kept in sync across your
  machines.

### Tools and models
- An MCP client for your own servers, with per-server switches and a Google accounts block for
  Gmail.
- Web search as a Helm tool, with your own Brave key.
- Local model servers on your machine (Ollama, Strata): started on send, unloaded when idle, shown
  in the model picker, and usable for background jobs such as titles.
- Your editor's current selection, through a small VS Code extension.

### Running while you are away
- Schedules and reminders that send a prompt into a chat on time.
- Away mode: a digest of what happened, triage of idle chats, and retries for chats that stalled.
- A usage governor that slows down before you hit your plan limits.
- Recovery from sleep, network outages and renderer crashes, with automatic continue.

### The hub (optional)
A small always-on service for a home server:
- A shared memory store over MCP, private per person or shared.
- A Telegram bot that talks to your chats, including pictures, files, voice notes and long
  messages.
- Voice from your phone, with replies spoken back.
- A Remote page to answer waiting prompts from a browser.
- Several machines and people: pair each PC, run chats on another of your machines, and give
  each person in a household their own private space.

### Comfort
- Themes, your own colours with contrast checked for you, motion and sounds you can turn down.
- Native notifications with Reply and Continue, a tray icon, an optional desktop pet, and small
  celebrations when work finishes.
- Backup, staged restore and export of your whole profile, and updates installed from a folder.
- A feedback store: tell Helm (or let your agents tell it) what got in the way.

## Getting started

1. Install Claude Code and sign in.
2. Build and install Helm (see [`docs/setup.md`](docs/setup.md)); the installer is per user and
   needs no admin rights.
3. Open Helm, pick a project folder, and start a chat.

From there: press **Mod+K** to find anything, open the side panel for files and diffs, and watch
the sidebar for chats that need you. Settings has a page for every part above.

## Documentation

- [`docs/setup.md`](docs/setup.md): requirements, building, testing and the repository layout.
- [`docs/roadmap.md`](docs/roadmap.md): where Helm is going next.
- [`hub/README.md`](hub/README.md): setting up the hub on a home server.
