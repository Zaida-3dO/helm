// Generated names and discovery search for MCP tools.
//
// A discovered tool is called by `mcp_<serverId>_<sanitised name, ≤ 20>_<12 hex of the name's
// SHA-256>`, labelled "<server name> · <tool name>". Search folds case, dashes, underscores and
// whitespace; a tool matches when one of its name, description or presented name contains every
// term; a leading presented `mcp_<server>_` in a term is ignored; a query of separators only
// matches nothing.

import { createHash } from "node:crypto";

export function generatedName(serverId: string, tool: string): string {
  const hash = createHash("sha256").update(tool).digest("hex").slice(0, 12);
  const clean = tool
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "")
    .slice(0, 20)
    .replace(/_$/, "");
  return `mcp_${serverId}_${clean || "tool"}_${hash}`;
}

export function label(serverName: string, tool: string): string {
  return `${serverName} · ${tool}`;
}

const fold = (s: string) => s.toLowerCase().replace(/[-_\s]+/g, "");

/** The search terms, or `null` for "match everything", or `[]` for "match nothing". */
export function terms(query: string | undefined, serverId: string): string[] | null {
  if (query === undefined || query.trim() === "") return null;
  const prefix = `mcp_${serverId}_`;
  const out = query
    .split(/\s+/)
    .map((t) => (t.toLowerCase().startsWith(prefix) ? t.slice(prefix.length) : t))
    .map(fold)
    .filter((t) => t !== "");
  return out;
}

export function matches(t: string[] | null, fields: (string | undefined)[]): boolean {
  if (t === null) return true;
  if (t.length === 0) return false;
  return fields.some((f) => {
    if (!f) return false;
    const ff = fold(f);
    return t.every((term) => ff.includes(term));
  });
}
