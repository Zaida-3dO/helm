# Helm roadmap

Helm is numbered 0.x until it is the everyday place to run an orchestration session: one chat that
plans work, hands it to other agents and sees it through review to merged pull requests. That point
is **v1.0**.

## Where it stands (0.2.1)

- Chats with Claude Code, one worker per active chat, that survive a quit, a crash or a relaunch.
- A composer with model and effort, permission mode, attachments, chat references and a `/` menu.
- Streaming replies, a work timeline, subagent cards that can be stopped one at a time, a Needs
  attention inbox and find in chat.
- The side panel (files, Changes with diffs and review comments, vault notes, quick open), the
  terminal dock, action buttons and quick replies, and Mod+K search.
- Host tools with consent, the MCP client and the plan-usage meter.
- Schedules, away mode, the usage governor and the run-slot queue.
- The orchestrator's workflow panel and its per-chat switch.
- The hub: shared memory, the relay line and a Telegram bot (its own, or a bot another service
  already runs), connected in Settings → Hub. Messages from Telegram carry a badge in the chat.
- A per-user Windows installer.

## Next (0.3): finish what is half-built

- **The waiting bar** for a send waiting on a run slot, and the remaining schedules work.
- **An updater or a dev channel**, so trying a new build does not mean running a setup file.
- **Loose ends** in the new-chat page and the composer, and the contrast of muted labels and the
  composer's focus state.
- **AI triage:** a quiet pass over idle chats that says which ones need the user.

## Later

- **Browser:** hosted browsers in a tab.
- **Board:** the project board inside Helm, with sidebar project edits written back to it.
- **Voice and Codex:** dictation behind the composer mic; Codex as a second agent.
- **Pull requests:** GitHub pull requests reviewed and merged inside Helm.
- **Delight:** pets, themes and sounds.
- **Ops:** server monitors.

## v1.0

A proof of concept has already orchestrated three agents through to merged pull requests from a Helm
chat. v1.0 makes that the everyday path rather than an experiment.
