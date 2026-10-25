// The cap on what one of Helm's own tools may put into the model's context.
//
// A result longer than `maxChars` keeps its head and its tail, with a plain marker between them
// saying how much was cut and where the whole text is. The full text goes to a file in the
// system temp folder, pretty-printed when it is JSON so a line-limited reader can page through it
// (best effort: without one, the marker says to ask for a smaller result).
// This covers the helm server's tools and the MCP tools it proxies. Claude Code's built-in tools
// are out of reach: see PROTOCOL-NOTES.md, "Tool output cap".

import { chmodSync, mkdirSync, readdirSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Json } from "../protocol/types.ts";

export const DEFAULT_MAX_CHARS = 5_000;
const HEAD_SHARE = 0.65;
const KEEP_FILES_MS = 24 * 60 * 60 * 1000;
/** Tool discovery pages are already bounded and are useless when cut. */
const EXEMPT = new Set(["app_list_tools", "app_mcp_tools"]);

export interface OutputCap {
  maxChars: number;
  /** Stores the full text and returns where it went, or undefined when it could not. */
  save?: (full: string) => string | undefined;
}

/** Cuts `text` to a head and a tail around a marker. Returns `text` itself when it is short. */
export function capText(text: string, maxChars: number, savedTo?: string): string {
  if (text.length <= maxChars) return text;
  let head = Math.floor(maxChars * HEAD_SHARE);
  let tail = maxChars - head;
  // Never split a surrogate pair.
  const isLow = (i: number) => text.charCodeAt(i) >= 0xdc00 && text.charCodeAt(i) <= 0xdfff;
  if (head > 0 && isLow(head)) head -= 1;
  if (tail > 0 && isLow(text.length - tail)) tail -= 1;
  const cut = text.length - head - tail;
  const where = savedTo
    ? `The whole output is saved at ${savedTo}; read that file to see the rest.`
    : "Ask again for less (a filter, a smaller page or one id) to see the rest.";
  return `${text.slice(0, head)}\n\n[Helm cut ${cut.toLocaleString("en-US")} characters from the middle of this output (${text.length.toLocaleString("en-US")} in all). ${where}]\n\n${text.slice(text.length - tail)}`;
}

/** A saver that writes into `<temp>/helm-tool-output`, and clears files older than a day. */
/** JSON on one very long line is hard to read in pieces; indent it. Anything else stays as is. */
export function readable(full: string): string {
  try {
    const v: unknown = JSON.parse(full);
    return v !== null && typeof v === "object" ? JSON.stringify(v, null, 2) : full;
  } catch {
    return full;
  }
}

export function fileSaver(prefix: string, dir = join(tmpdir(), "helm-tool-output")): (full: string) => string | undefined {
  let n = 0;
  let swept = false;
  const safe = prefix.replace(/[^A-Za-z0-9_-]/g, "").slice(0, 40) || "chat";
  return (full) => {
    try {
      const posix = process.platform !== "win32";
      mkdirSync(dir, { recursive: true, ...(posix ? { mode: 0o700 } : {}) });
      if (posix) chmodSync(dir, 0o700);
      if (!swept) {
        swept = true;
        for (const f of readdirSync(dir)) {
          try {
            const p = join(dir, f);
            if (Date.now() - statSync(p).mtimeMs > KEEP_FILES_MS) unlinkSync(p);
          } catch {
            // Someone else's file or already gone.
          }
        }
      }
      const path = join(dir, `${safe}-${Date.now()}-${++n}.txt`);
      writeFileSync(path, readable(full), { encoding: "utf8", ...(posix ? { mode: 0o600 } : {}) });
      return path;
    } catch {
      return undefined;
    }
  };
}

/** Applies the cap to an MCP CallToolResult built from text. Other results pass unchanged. */
export function capToolResult(result: Json, toolName: string, cap: OutputCap): Json {
  if (EXEMPT.has(toolName)) return result;
  const content = result.content;
  if (!Array.isArray(content) || content.length !== 1) return result;
  const block = content[0] as Json;
  if (block?.type !== "text" || typeof block.text !== "string" || block.text.length <= cap.maxChars) return result;
  const text = capText(block.text, cap.maxChars, cap.save?.(block.text));
  // The structured copy would carry the whole output past the cut.
  const { structuredContent: _dropped, ...rest } = result;
  void _dropped;
  return { ...rest, content: [{ type: "text", text }] };
}
