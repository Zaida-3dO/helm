// Shapes the worker reads and writes. The field choices are recorded in worker/PROTOCOL-NOTES.md.

export type Json = Record<string, unknown>;

/**
 * A chat that runs on a local model server instead of Anthropic's. The key is not here: the host
 * hands it to the worker in HELM_LOCAL_MODEL_KEY, which the worker reads once and removes.
 */
export interface LocalLaunch {
  /** The server's address; must be plain http on this machine (loopback), or the worker refuses to start. */
  baseUrl: string;
  /** The server's own name for the model. Every Claude Code model slot (main, small, sub-agents) uses it. */
  model: string;
  contextTokens?: number;
  maxOutputTokens?: number;
  /** The thinking budget when the launch carries no effort; otherwise the effort sets it. */
  thinkingTokens?: number;
}

export interface ClaudeLaunch {
  /** Explicit executable. When absent, the resolver runs (HELM_CLAUDE_PATH, PATH, ...). */
  command?: string;
  /** Arguments placed before the driver's own flags (used to run the fake: [script, fixture]). */
  commandArgs?: string[];
  model?: string;
  effort?: string;
  skipPermissions?: boolean;
  /** API-key mode: keep ANTHROPIC_* and CLAUDE_CODE_OAUTH_TOKEN in the child env. */
  apiKeyMode?: boolean;
  resumeSessionId?: string;
  appendSystemPrompt?: string;
  settings?: Json;
  /** Claude Code's fast mode, sent on every launch as `--settings {"fastMode":…}`. Only this field
   * sets it: a `fastMode` inside `settings` is dropped. Anything but `true` is off. */
  fastMode?: boolean;
  /** With `resumeSessionId`: resume it as a new session (`--fork-session`), a fork's first launch. */
  forkSession?: boolean;
  settingSources?: string[];
  extraArgs?: string[];
  /** Native MCP servers off: `--strict-mcp-config`, only Helm's server loads. */
  strictMcpConfig?: boolean;
  /** Native MCP servers whose tools are disallowed (held repository servers). */
  disabledMcpServers?: string[];
  /** Claude Code's own worktree tools (EnterWorktree, ExitWorktree) may be offered. Absent: off. */
  worktreeTools?: boolean;
  /** Tool calls a sub-agent may make before it is told to hand off and stop; 0 or absent: no limits. */
  subagentSoftTurns?: number;
  /** Past 30% over the limit, refuse a sub-agent's tool calls (otherwise it is only reminded). */
  subagentHardStop?: boolean;
  /** Cut very long results of Helm's own tools to a head and a tail. */
  toolOutputCap?: boolean;
  /** Claude Code's own schedule tools may be offered; every call is refused with a redirect to Helm's schedules. Absent: off. */
  scheduleTools?: boolean;
  /** Record which files each Bash call changed (compares the checkout before and after). Absent: off. */
  shellChanges?: boolean;
  /** Run on a local model server. Absent: a normal Claude chat. */
  local?: LocalLaunch;
}

export interface LaunchLimits {
  maxRecordBytes?: number;
  promptAckMs?: number;
  stopSettleMs?: number;
  controlTimeoutMs?: number;
  shutdownGraceMs?: number;
  /** How long a tool call waits for the landing guard's answer (default 10 s). */
  guardTimeoutMs?: number;
}

/** Workers receive only these send-behaviour fields at launch. */
export interface SendBehaviorLaunch {
  longCommands?: "off" | "warn" | "redirect";
  optIn?: boolean;
}

export interface Launch {
  chatId: string;
  harness?: "claude";
  cwd?: string;
  claude?: ClaudeLaunch;
  sendBehavior?: SendBehaviorLaunch;
  /** Test-only overrides; production launches omit this. */
  limits?: LaunchLimits;
  /** Host tools to serve through the in-process `helm` MCP server. */
  hostTools?: import("../hosttools/server.ts").HostTool[];
  /** Where the worker makes one private folder per host-tool request. */
  chatToolsDirectory?: string;
}

export const EFFORTS = ["low", "medium", "high", "xhigh", "max"] as const;
export type Effort = (typeof EFFORTS)[number];
export const DEFAULT_EFFORT: Effort = "xhigh";

export function isEffort(v: unknown): v is Effort {
  return typeof v === "string" && (EFFORTS as readonly string[]).includes(v);
}

/**
 * What each effort means on a local model, which has no effort setting: a thinking budget in
 * tokens. Low turns thinking off. Claude Code is given the number as MAX_THINKING_TOKENS (and, to
 * change it mid-chat, set_max_thinking_tokens).
 */
export const LOCAL_THINKING_TOKENS: Readonly<Record<Effort, number>> = { low: 0, medium: 4096, high: 8192, xhigh: 16384, max: 32768 };

/** Interaction request. */
export interface UiRequest {
  type: "extension_ui_request";
  id: string;
  method: "select" | "confirm" | "input" | "editor" | "dismiss";
  attention?: "approval" | "question" | "plan" | "trust";
  title?: string;
  header?: string;
  message?: string;
  context?: { label: string; text: string; markdown?: boolean }[];
  options?: string[];
  optionDescriptions?: string[];
  allowOther?: boolean;
  secret?: boolean;
  step?: number;
  steps?: number;
  answer?: string;
  prefill?: string;
  /** On dismiss: how many interactions are still waiting. */
  remaining?: number;
}

export interface UiResponse {
  id: string;
  value?: string;
  confirmed?: boolean;
  cancelled?: boolean;
  goto?: number;
}

/** Normalised content blocks. */
export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "toolCall"; id: string; name: string; arguments: unknown }
  | { type: "image"; mimeType?: string };

export type SettleOutcome = "complete" | "error" | "stopped" | "interrupted";
export type RetryEndState = "recovered" | "retried" | "cancelled" | "failed";
