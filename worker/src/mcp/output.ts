// What the model gets from an MCP tool result: text up to 50 KB and
// 2,000 lines with a truncation note; up to 4 images (png/jpeg/webp, each ≤ 2 MiB, ≤ 4 MiB in
// all); embedded text resources and resource links as text; other content as a placeholder;
// structured content appended as JSON unless a text block already carries the same JSON.
// Everything passes through the server's redactor.

import type { Redactor } from "./redact.ts";
import type { Json, McpLimits } from "./types.ts";

const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/webp"]);

function sameJson(text: string, v: unknown): boolean {
  try {
    return JSON.stringify(JSON.parse(text)) === JSON.stringify(v);
  } catch {
    return false;
  }
}

/** Cuts `text` to the byte and line limits, on a character boundary. */
export function capText(text: string, maxBytes: number, maxLines: number): { text: string; cut: boolean } {
  let cut = false;
  let out = text;
  const lines = out.split("\n");
  if (lines.length > maxLines) {
    out = lines.slice(0, maxLines).join("\n");
    cut = true;
  }
  if (Buffer.byteLength(out) > maxBytes) {
    const buf = Buffer.from(out).subarray(0, maxBytes);
    out = buf.toString("utf8").replace(/�$/, "");
    cut = true;
  }
  return { text: out, cut };
}

export function formatResult(raw: Json, red: Redactor, limits: McpLimits): { content: Json[]; isError: boolean } {
  const blocks = Array.isArray(raw.content) ? (raw.content as Json[]) : [];
  const texts: string[] = [];
  const images: Json[] = [];
  let imageBytes = 0;
  let droppedImages = 0;
  for (const b of blocks) {
    if (!b || typeof b !== "object") continue;
    switch (b.type) {
      case "text":
        if (typeof b.text === "string") texts.push(b.text);
        break;
      case "image": {
        const data = typeof b.data === "string" ? b.data : "";
        const mime = typeof b.mimeType === "string" ? b.mimeType.toLowerCase() : "";
        const size = Math.floor((data.length * 3) / 4);
        if (
          IMAGE_TYPES.has(mime) &&
          images.length < limits.maxImages &&
          size <= limits.maxImageBytes &&
          imageBytes + size <= limits.maxImageTotal
        ) {
          images.push({ type: "image", data, mimeType: mime });
          imageBytes += size;
        } else {
          droppedImages++;
        }
        break;
      }
      case "resource": {
        const r = (b.resource ?? {}) as Json;
        if (typeof r.text === "string") texts.push(r.text);
        else texts.push(`[binary resource ${String(r.uri ?? "")} omitted]`);
        break;
      }
      case "resource_link":
        texts.push(`Resource: ${String(b.name ?? b.uri ?? "")} ${String(b.uri ?? "")}`.trim());
        break;
      default:
        texts.push(`[${String(b.type ?? "unknown")} content omitted]`);
    }
  }
  if (raw.structuredContent !== undefined && !texts.some((t) => sameJson(t, raw.structuredContent))) {
    texts.push(JSON.stringify(raw.structuredContent));
  }
  if (droppedImages > 0) texts.push(`[${droppedImages} image(s) omitted: only png, jpeg or webp, at most ${limits.maxImages}, 2 MiB each and 4 MiB in all]`);
  const joined = red.text(texts.join("\n"));
  const capped = capText(joined, limits.maxTextBytes, limits.maxLines);
  let text = capped.text;
  if (capped.cut) text += `\n[output truncated to ${limits.maxTextBytes} bytes and ${limits.maxLines} lines]`;
  const content: Json[] = [];
  if (text !== "" || images.length === 0) content.push({ type: "text", text });
  content.push(...images);
  return { content, isError: raw.isError === true };
}
