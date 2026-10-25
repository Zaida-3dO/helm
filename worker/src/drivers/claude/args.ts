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

/** Claude Code's own worktree tools; kept off unless the user lets them through. */
const WORKTREE_TOOLS = ["EnterWorktree", "ExitWorktree"];

/** Claude Code's own schedule tools; offered only when the user lets them be (every call is then redirected). */
const SCHEDULE_TOOLS = ["CronCreate", "CronDelete", "CronList", "ScheduleWakeup", "RemoteTrigger"];

/**
 * The tools to disallow. The worktree tools come off the list only when the launch asks for them
 * and no repository has worktrees switched off (Helm then sets HELM_WORKTREE_OFF_LIST for the chat).
 * The schedule tools come off it when the launch asks for them.
 */
export function disallowedTools(c: ClaudeLaunch, env: Record<string, string | undefined> = process.env): string[] {
  const allowWorktrees = c.worktreeTools === true && !env.HELM_WORKTREE_OFF_LIST;
  const allowSchedules = c.scheduleTools === true;
  return DISALLOWED_TOOLS.filter((t) => !(allowWorktrees && WORKTREE_TOOLS.includes(t)) && !(allowSchedules && SCHEDULE_TOOLS.includes(t)));
}

export interface ArgOptions {
  /** The long-command rule, appended to the main chat's instructions when the rule is on. */
  promptRule?: string;
  /** Register Helm's in-process `helm` MCP server (`--mcp-config` with the SDK entry). */
  mcpConfig?: string;
}

/** Pre-approves every tool of Helm's own server. */
export const HELM_ALLOWED = "mcp__helm";

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
    [...disallowedTools(c), ...(c.disabledMcpServers ?? []).filter((n) => /^[A-Za-z0-9_-]{1,64}$/.test(n) && n !== "helm").map((n) => `mcp__${n}`)].join(","),
  ];
  // Native MCP stays on by default; Helm's server is added beside the user's own.
  if (opts.mcpConfig) args.push("--mcp-config", opts.mcpConfig, "--allowedTools", HELM_ALLOWED);
  if (c.strictMcpConfig === true) args.push("--strict-mcp-config");
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
  // Held repository servers are also switched off for the CLI's .mcp.json loader.
  const held = (c.disabledMcpServers ?? []).filter((n) => /^[A-Za-z0-9_-]{1,64}$/.test(n) && n !== "helm");
  const extra = held.length > 0 ? { disabledMcpjsonServers: held } : {};
  args.push("--settings", JSON.stringify({ ...custom, ...extra, fastMode: c.fastMode === true }));
  if (c.extraArgs) args.push(...c.extraArgs);
  return args;
}
