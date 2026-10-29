// The claude child's environment. Credentials and nesting markers are stripped unless
// API-key mode is on. Non-essential traffic and auto-update are switched off.
//
// One exception: a chat on a local model server (`claude.local`). It gets that server's address as
// the base URL and the server's own key as the auth token, and never the user's sign-in or an
// Anthropic key.

import { DEFAULT_EFFORT, isEffort, LOCAL_THINKING_TOKENS, type LocalLaunch } from "../../protocol/types.ts";

export const STRIPPED_UNLESS_API_KEY_MODE = [
  "ANTHROPIC_API_KEY",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_BASE_URL",
  "CLAUDE_CODE_OAUTH_TOKEN",
  "CLAUDECODE",
] as const;

/** Where the host hands the worker a local server's key. The worker reads it once (`takeLocalKey`),
 * removes it from its own environment, and never passes it on under this name. */
export const LOCAL_KEY_VAR = "HELM_LOCAL_MODEL_KEY";

/** Always removed: variables that would make the child look like part of another agent's session,
 * Helm's own Node compile cache (set for the worker and helpers only), and the local key's hand-over name. */
export const ALWAYS_STRIPPED = ["CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT", "NODE_COMPILE_CACHE", LOCAL_KEY_VAR] as const;

/** Removed from a local chat on top of the credential variables: another provider, or headers that
 * could carry a credential to the local server. */
const STRIPPED_FOR_LOCAL = ["ANTHROPIC_CUSTOM_HEADERS"];
const STRIPPED_PREFIX_FOR_LOCAL = ["CLAUDE_CODE_USE_"];

/** Sent when the server needs no key: Claude Code must always have a token of its own to use, or it
 * would fall back to the user's sign-in. */
const LOCAL_KEY_PLACEHOLDER = "local";

export const FORCED: Readonly<Record<string, string>> = {
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  DISABLE_AUTOUPDATER: "1",
};

/**
 * Reads the local server key the host passed in `env` and removes it there, so nothing the worker
 * starts later (claude, MCP servers, hooks) can see it. Undefined when the launch brought none.
 */
export function takeLocalKey(env: Record<string, string | undefined>): string | undefined {
  let key: string | undefined;
  // Windows env names are case-insensitive: compare upper-cased.
  for (const k of Object.keys(env)) {
    if (k.toUpperCase() !== LOCAL_KEY_VAR) continue;
    key = env[k];
    delete env[k];
  }
  return key === undefined || key === "" ? undefined : key;
}

/**
 * The base URL a local chat may use: plain http to this machine. Returns it without a trailing
 * slash. Anything else throws, so a launch can never send the local key (or a prompt) to another host.
 */
export function assertLoopbackHttp(baseUrl: string): string {
  let u: URL;
  try {
    u = new URL(baseUrl);
  } catch {
    throw new Error("the local model server's address is not a valid URL");
  }
  const loopback = u.hostname === "127.0.0.1" || u.hostname === "localhost" || u.hostname === "[::1]";
  if (u.protocol !== "http:" || !loopback || u.username !== "" || u.password !== "") {
    throw new Error("a local model server must be on this machine and use plain http (127.0.0.1, localhost or ::1)");
  }
  return u.origin + (u.pathname === "/" ? "" : u.pathname.replace(/\/+$/, ""));
}

/** The thinking budget for a local chat: the effort's, or the launch's own figure when it has no effort. */
export function localThinkingTokens(local: Pick<LocalLaunch, "thinkingTokens">, effort: string | undefined): number {
  if (isEffort(effort)) return LOCAL_THINKING_TOKENS[effort];
  if (typeof local.thinkingTokens === "number" && local.thinkingTokens >= 0) return Math.floor(local.thinkingTokens);
  return LOCAL_THINKING_TOKENS[DEFAULT_EFFORT];
}

export function buildClaudeEnv(
  base: Record<string, string | undefined>,
  opts: { apiKeyMode?: boolean; chatId?: string; local?: LocalLaunch; localKey?: string; effort?: string },
): Record<string, string> {
  const local = opts.local;
  const strip = new Set<string>(ALWAYS_STRIPPED.map((k) => k.toUpperCase()));
  // A local chat never carries Anthropic credentials, whatever API-key mode says.
  if (!opts.apiKeyMode || local) for (const k of STRIPPED_UNLESS_API_KEY_MODE) strip.add(k);
  if (local) for (const k of STRIPPED_FOR_LOCAL) strip.add(k);
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(base)) {
    if (v === undefined) continue;
    // Windows env names are case-insensitive: compare upper-cased.
    const upper = k.toUpperCase();
    if (strip.has(upper)) continue;
    if (local && STRIPPED_PREFIX_FOR_LOCAL.some((p) => upper.startsWith(p))) continue;
    out[k] = v;
  }
  for (const [k, v] of Object.entries(FORCED)) out[k] = v;
  if (opts.chatId) out.HELM_CHAT_ID = opts.chatId;
  if (local) applyLocal(out, local, opts.localKey, opts.effort);
  return out;
}

function applyLocal(out: Record<string, string>, local: LocalLaunch, key: string | undefined, effort: string | undefined): void {
  const model = typeof local.model === "string" ? local.model.trim() : "";
  if (!model) throw new Error("a local chat needs the server's model name");
  out.ANTHROPIC_BASE_URL = assertLoopbackHttp(local.baseUrl);
  out.ANTHROPIC_AUTH_TOKEN = key || LOCAL_KEY_PLACEHOLDER;
  // Every model slot is the local model, so the small-model jobs and sub-agents stay on it too.
  out.ANTHROPIC_MODEL = model;
  out.ANTHROPIC_SMALL_FAST_MODEL = model;
  out.ANTHROPIC_DEFAULT_OPUS_MODEL = model;
  out.ANTHROPIC_DEFAULT_SONNET_MODEL = model;
  out.ANTHROPIC_DEFAULT_HAIKU_MODEL = model;
  if (typeof local.contextTokens === "number" && local.contextTokens > 0) out.CLAUDE_CODE_MAX_CONTEXT_TOKENS = String(Math.floor(local.contextTokens));
  if (typeof local.maxOutputTokens === "number" && local.maxOutputTokens > 0) out.CLAUDE_CODE_MAX_OUTPUT_TOKENS = String(Math.floor(local.maxOutputTokens));
  out.MAX_THINKING_TOKENS = String(localThinkingTokens(local, effort));
  // A proxy variable in the user's environment must not carry loopback traffic (and the key) to a proxy.
  const loopback = ["127.0.0.1", "localhost", "::1"];
  const existing = Object.keys(out).find((k) => k.toUpperCase() === "NO_PROXY");
  const name = existing ?? "NO_PROXY";
  const have = (out[name] ?? "").split(",").map((s) => s.trim()).filter(Boolean);
  out[name] = [...have, ...loopback.filter((h) => !have.includes(h))].join(",");
}
