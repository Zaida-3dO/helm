// Glue between the MCP bridge and the rest of the worker:
// - `asHostToolBridge`: the shape the in-process "helm" server asks for
//   (`McpBridgeLike` in worker/src/hosttools/server.ts);
// - `applyConfigLine`: the host's `mcp_config` stdin line;
// - `nativeFromInit`: the servers the CLI reports loading natively in its `system/init` message;
// - `duplicateMcpRefusal`: the PreToolUse answer for Claude Code's copy of a server Helm also has.

import { normName, type McpBridge } from "./bridge.ts";
import { McpError, type DiscoveryResult, type Json, type NativeServer, type McpConfig, type ResolvedServer } from "./types.ts";

/** The hosttools channel's call outcome (worker/src/hosttools/channel.ts `CallResult`). */
export type BridgeCallResult = { ok: true; result: unknown } | { ok: false; error: string; changed: "no" | "maybe" };

export interface HostToolBridge {
  enabled(): boolean;
  tools(args: Json): Promise<unknown>;
  has(name: string): boolean;
  call(name: string, args: Json, signal?: AbortSignal): Promise<BridgeCallResult>;
}

const NOTHING = " Nothing changed.";
const MAYBE = " The change may have happened: check before retrying.";

function failure(e: unknown): BridgeCallResult {
  const m = e instanceof McpError ? e : new McpError(String((e as Error)?.message ?? e), { changed: "maybe" });
  return { ok: false, error: m.message + (m.changed === "maybe" ? MAYBE : NOTHING), changed: m.changed };
}

/** `app_mcp_tools` without `server` searches every server, still loading at most 20 tools. */
async function searchAll(bridge: McpBridge, query: string | undefined): Promise<Json> {
  const results: (DiscoveryResult | { server: string; error: string })[] = [];
  let loaded = 0;
  for (const id of bridge.serverIds()) {
    if (loaded >= 20) {
      results.push({ server: id, error: "Not searched: 20 tools were already loaded. Name this server to search it." });
      continue;
    }
    try {
      const r = await bridge.tools({ server: id, query });
      loaded += r.loaded.length;
      results.push(r);
    } catch (e) {
      results.push({ server: id, error: (e as Error).message });
    }
  }
  return { servers: results };
}

export function asHostToolBridge(bridge: McpBridge): HostToolBridge {
  return {
    enabled: () => bridge.serverIds().length > 0,
    async tools(args: Json) {
      const query = typeof args.query === "string" ? args.query : undefined;
      const server = typeof args.server === "string" && args.server !== "" ? args.server : undefined;
      if (server) return bridge.tools({ server, query });
      const ids = bridge.serverIds();
      if (ids.length === 1) return bridge.tools({ server: ids[0]!, query });
      return searchAll(bridge, query);
    },
    has: (name) => bridge.isLoaded(name),
    async call(name, args, signal) {
      try {
        const r = await bridge.call(name, args, signal);
        const text = r.content
          .filter((c) => c.type === "text")
          .map((c) => String(c.text))
          .join("\n");
        if (r.isError) return { ok: false, error: text || `${r.source.tool} reported an error.`, changed: "maybe" };
        const images = r.content.filter((c) => c.type === "image");
        return { ok: true, result: images.length ? { text, images, source: r.source } : text };
      } catch (e) {
        return failure(e);
      }
    },
  };
}

/** Applies an `mcp_config` line from the host. Returns false (and changes nothing) when it is
 * not one. */
export function applyConfigLine(bridge: McpBridge, cmd: Json): boolean {
  if (cmd.type !== "mcp_config" || typeof cmd.revision !== "string" || !Array.isArray(cmd.servers)) return false;
  const servers = (cmd.servers as unknown[]).filter(
    (s): s is ResolvedServer => !!s && typeof s === "object" && typeof (s as Json).id === "string" && typeof (s as Json).name === "string",
  );
  const cfg: McpConfig = { revision: cmd.revision, servers };
  if (Array.isArray(cmd.native)) cfg.native = (cmd.native as NativeServer[]).filter((n) => n && typeof n.name === "string");
  bridge.configure(cfg);
  return true;
}

/** The natively loaded servers from the CLI's `system/init` message (`mcp_servers: [{name,
 * status}]`), leaving out Helm's own and any that failed to connect. */
export function nativeFromInit(init: Json): NativeServer[] {
  if (init.type !== "system" || init.subtype !== "init" || !Array.isArray(init.mcp_servers)) return [];
  return (init.mcp_servers as Json[])
    .filter((s) => s && typeof s.name === "string" && s.name !== "helm" && s.status !== "failed")
    .map((s) => ({ name: s.name as string }));
}

/** Why a call to Claude Code's own MCP tool `toolName` (`mcp__<server>__<tool>`) is refused: the
 * same server is configured in Helm, and Helm's copy takes precedence. Undefined allows it.
 * A sub-agent's call is always allowed: the redirect names Helm's tools, which a sub-agent is not
 * given, so refusing it would leave the sub-agent with no way to reach the server. */
export function duplicateMcpRefusal(toolName: string, bridge: Pick<McpBridge, "shadowing"> | undefined, fromSubagent = false): string | undefined {
  const m = /^mcp__(.+?)__./.exec(toolName);
  if (fromSubagent || !m || !bridge || normName(m[1]!) === "helm") return undefined;
  const s = bridge.shadowing(m[1]!);
  if (!s) return undefined;
  return `${s.name} is also set up in Helm, and Helm's copy is the one to use: find this tool with app_mcp_tools (server "${s.id}") and call it with app_mcp_call.`;
}
