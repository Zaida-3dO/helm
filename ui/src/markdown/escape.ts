// HTML escaping for text and attribute values. Everything the renderer emits that originates from
// the source text goes through one of these.

const ESCAPES: Record<string, string> = {
  '&': '&amp;',
  '<': '&lt;',
  '>': '&gt;',
  '"': '&quot;',
  "'": '&#39;',
};

export function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (c) => ESCAPES[c] ?? c);
}

/** Same escaping; a separate name so attribute call sites read as such. */
export const escapeAttr = escapeHtml;
