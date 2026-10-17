// Shared bits for the MCP client tests: fake-server configs and a bridge with a status log.

import path from "node:path";
import { fileURLToPath } from "node:url";
import { createMcpBridge, type BridgeOptions, type McpBridge } from "../src/mcp/index.ts";
import type { Json, ResolvedServer, ServerStatus } from "../src/mcp/types.ts";
import type { FakeConfig } from "../../fakes/mcp-server/server.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
export const FAKE_MCP = path.resolve(here, "../../fakes/mcp-server/server.ts");

/** The fake's tools with object schemas (bad_schema is skipped by the client). */
export const FIXED_TOOLS = 9;

export function stdioServer(id: string, cfg: FakeConfig = {}, extra: Partial<ResolvedServer> = {}): ResolvedServer {
  return {
    id,
    name: `Fake ${id}`,
    transport: "stdio",
    command: process.execPath,
    args: [FAKE_MCP, "--stdio", JSON.stringify(cfg)],
    allowPrivateHttp: false,
    ...extra,
  };
}

export function httpServer(id: string, url: string, extra: Partial<ResolvedServer> = {}): ResolvedServer {
  return { id, name: `Fake ${id}`, transport: "http", url, allowPrivateHttp: false, ...extra };
}

export interface Harness {
  bridge: McpBridge;
  lines: Json[];
  last(serverId: string): ServerStatus | undefined;
}

export function harness(opts: BridgeOptions = {}): Harness {
  const lines: Json[] = [];
  const bridge = createMcpBridge((l) => lines.push(l), opts);
  return {
    bridge,
    lines,
    last: (id) => [...lines].reverse().find((l) => l.serverId === id) as ServerStatus | undefined,
  };
}

export function text(r: { content: Json[] }): string {
  return r.content
    .filter((c) => c.type === "text")
    .map((c) => String(c.text))
    .join("\n");
}
