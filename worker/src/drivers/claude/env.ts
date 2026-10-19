// The claude child's environment. Credentials and nesting markers are stripped unless
// API-key mode is on. Non-essential traffic and auto-update are switched off.

export const STRIPPED_UNLESS_API_KEY_MODE = [
  "ANTHROPIC_API_KEY",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_BASE_URL",
  "CLAUDE_CODE_OAUTH_TOKEN",
  "CLAUDECODE",
] as const;

/** Always removed: variables that would make the child look like part of another agent's session,
 * and Helm's own Node compile cache (set for the worker and helpers only). */
export const ALWAYS_STRIPPED = ["CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT", "NODE_COMPILE_CACHE"] as const;

export const FORCED: Readonly<Record<string, string>> = {
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  DISABLE_AUTOUPDATER: "1",
};

export function buildClaudeEnv(
  base: Record<string, string | undefined>,
  opts: { apiKeyMode?: boolean; chatId?: string },
): Record<string, string> {
  const strip = new Set<string>(ALWAYS_STRIPPED.map((k) => k.toUpperCase()));
  if (!opts.apiKeyMode) for (const k of STRIPPED_UNLESS_API_KEY_MODE) strip.add(k);
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(base)) {
    if (v === undefined) continue;
    // Windows env names are case-insensitive: compare upper-cased.
    if (strip.has(k.toUpperCase())) continue;
    out[k] = v;
  }
  for (const [k, v] of Object.entries(FORCED)) out[k] = v;
  if (opts.chatId) out.HELM_CHAT_ID = opts.chatId;
  return out;
}
