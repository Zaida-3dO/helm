// Composes the claude CLI flags. One function owns every flag so later features
// add theirs here rather than scattering them.

import { DEFAULT_EFFORT, isEffort, type ClaudeLaunch } from "../../protocol/types.ts";

/** Haiku models take no effort setting. */
export function modelTakesEffort(model: string | undefined): boolean {
  return !model || !/haiku/i.test(model);
}

/**
 * Native tools the app replaces: schedules and wakeups belong to the
 * app's schedules, folders to its worktrees. Names read from claude 2.1.289's init tool list.
 */
export const DISALLOWED_TOOLS = ["CronCreate", "CronDelete", "CronList", "ScheduleWakeup", "RemoteTrigger", "EnterWorktree", "ExitWorktree"] as const;

export interface ArgOptions {
  /** The long-command rule, appended to the main chat's instructions when the rule is on. */
  promptRule?: string;
}

export function buildClaudeArgs(c: ClaudeLaunch, opts: ArgOptions = {}): string[] {
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
    "--disallowedTools",
    DISALLOWED_TOOLS.join(","),
  ];
  if (c.model) args.push("--model", c.model);
  const effort = isEffort(c.effort) ? c.effort : DEFAULT_EFFORT;
  if (modelTakesEffort(c.model)) args.push("--effort", effort);
  if (c.skipPermissions) args.push("--permission-mode", "bypassPermissions", "--allow-dangerously-skip-permissions");
  if (c.resumeSessionId) args.push("--resume", c.resumeSessionId);
  if (c.resumeSessionId && c.forkSession === true) args.push("--fork-session");
  const append = [c.appendSystemPrompt, opts.promptRule].filter((x): x is string => typeof x === "string" && x.length > 0).join("\n\n");
  if (append) args.push("--append-system-prompt", append);
  // fastMode is always explicit: the launch's own field is its only owner.
  const { fastMode: _dropped, ...custom } = c.settings ?? {};
  void _dropped;
  args.push("--settings", JSON.stringify({ ...custom, fastMode: c.fastMode === true }));
  if (c.extraArgs) args.push(...c.extraArgs);
  return args;
}
