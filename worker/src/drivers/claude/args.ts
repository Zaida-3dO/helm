// Composes the claude CLI flags. One function owns every flag so later features
// add theirs here rather than scattering them.

import { DEFAULT_EFFORT, isEffort, type ClaudeLaunch } from "../../protocol/types.ts";

/** Haiku models take no effort setting. */
export function modelTakesEffort(model: string | undefined): boolean {
  return !model || !/haiku/i.test(model);
}

export function buildClaudeArgs(c: ClaudeLaunch): string[] {
  const args = [
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    // Echoes each user message (with our uuid) back on stdout: that echo is the submission ack.
    "--replay-user-messages",
    // Permission prompts come to the worker over the control channel (can_use_tool).
    "--permission-prompt-tool",
    "stdio",
    "--setting-sources",
    (c.settingSources ?? ["user", "project", "local"]).join(","),
  ];
  if (c.model) args.push("--model", c.model);
  const effort = isEffort(c.effort) ? c.effort : DEFAULT_EFFORT;
  if (modelTakesEffort(c.model)) args.push("--effort", effort);
  if (c.skipPermissions) args.push("--permission-mode", "bypassPermissions", "--allow-dangerously-skip-permissions");
  if (c.resumeSessionId) args.push("--resume", c.resumeSessionId);
  if (c.appendSystemPrompt) args.push("--append-system-prompt", c.appendSystemPrompt);
  if (c.settings && Object.keys(c.settings).length > 0) args.push("--settings", JSON.stringify(c.settings));
  if (c.extraArgs) args.push(...c.extraArgs);
  return args;
}
