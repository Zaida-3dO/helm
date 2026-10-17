// Tool identity on transcript rows: which server a tool row came from, so the transcript can show
// Helm's own tools and MCP tools distinctly.

import type { Json } from "../protocol/types.ts";
import { SERVER_NAME } from "./server.ts";

export interface ToolSource {
  kind: "host" | "mcp" | "native";
  server: string;
  tool: string;
}

/** A discovered MCP tool name as `<server>__<tool>`; anything else keeps the whole name. */
function splitMcp(name: string): { server: string; tool: string } {
  const at = name.indexOf("__");
  return at > 0 ? { server: name.slice(0, at), tool: name.slice(at + 2) } : { server: "MCP", tool: name };
}

export function toolSource(name: string, args: unknown): ToolSource {
  const own = `mcp__${SERVER_NAME}__`;
  if (name.startsWith(own)) {
    const tool = name.slice(own.length);
    if (tool === "app_mcp_call") {
      const target = typeof (args as Json | null)?.name === "string" ? String((args as Json).name) : "";
      if (target.startsWith("app_")) return { kind: "host", server: "Helm", tool: target };
      if (target) return { kind: "mcp", ...splitMcp(target) };
    }
    return { kind: "host", server: "Helm", tool };
  }
  if (name.startsWith("mcp__")) {
    const rest = name.slice("mcp__".length);
    const at = rest.indexOf("__");
    if (at > 0) return { kind: "mcp", server: rest.slice(0, at), tool: rest.slice(at + 2) };
  }
  return { kind: "native", server: "Claude Code", tool: name };
}
