// Agent text on an Allow card is inert: plain text, one element per line,
// and every invisible or direction-changing character shown as a visible ⟨U+XXXX⟩ token, so a
// reason cannot hide or reorder what the user reads.

/** True for characters that must be shown as a token rather than drawn. */
export function isHidden(cp: number): boolean {
  if (cp === 0x0a || cp === 0x09) return false; // newline and tab are layout
  if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return true; // controls
  if (cp === 0x034f || cp === 0x115f || cp === 0x1160 || cp === 0x3164 || cp === 0xffa0 || cp === 0x2800) return true;
  if ((cp >= 0xfe00 && cp <= 0xfe0f) || (cp >= 0xe0100 && cp <= 0xe01ef)) return true; // variation selectors
  const ch = String.fromCodePoint(cp);
  if (/\p{Cf}/u.test(ch)) return true; // format: bidi marks, zero-width, tags
  if (/[\p{Zl}\p{Zp}]/u.test(ch)) return true;
  if (/\p{Zs}/u.test(ch) && cp !== 0x20) return true; // spaces other than the plain one
  return false;
}

export function token(cp: number): string {
  return `⟨U+${cp.toString(16).toUpperCase().padStart(4, '0')}⟩`;
}

/** The text as display lines with hidden characters made visible. */
export function inertLines(text: string): string[] {
  let out = '';
  for (const ch of text) {
    const cp = ch.codePointAt(0) ?? 0;
    out += isHidden(cp) ? token(cp) : ch;
  }
  return out.split('\n');
}
