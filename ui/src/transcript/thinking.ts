// Thinking preview: built only from the last PREVIEW_SOURCE chars, with Markdown
// stripped, showing the latest sentence or two. The view refreshes it at most every PREVIEW_MS.

export const PREVIEW_SOURCE = 2400;
export const PREVIEW_MS = 300;
const PREVIEW_MAX = 240;

export function stripMarkdown(s: string): string {
  return s
    .replace(/```[\s\S]*?(```|$)/g, ' ')
    .replace(/`([^`]*)`/g, '$1')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/^\s{0,3}(#{1,6}|>|[-*+]|\d+\.)\s+/gm, '')
    .replace(/(\*\*|__|\*|_|~~)/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}

export function thinkingPreview(thinking: string): string {
  const plain = stripMarkdown(thinking.slice(-PREVIEW_SOURCE));
  if (!plain) return '';
  // Split into sentences, keep whole sentences from the end while they fit.
  const sentences = plain.match(/[^.!?]+[.!?]*\s*/g) ?? [plain];
  let out = '';
  for (let i = sentences.length - 1; i >= 0; i--) {
    const next = sentences[i] + out;
    if (next.trim().length > PREVIEW_MAX && out) break;
    out = next;
  }
  out = out.trim();
  return out.length > PREVIEW_MAX ? '…' + out.slice(-PREVIEW_MAX) : out;
}
