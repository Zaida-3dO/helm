// The part of a note an embed or a heading link names (Obsidian's `Note#Heading`, `Note#^block`).

/** Normalises heading text the way links name it: trimmed, case-insensitive, spaces collapsed. */
export function headingKey(s: string): string {
  return s.replace(/[#*_`[\]]/g, '').replace(/\s+/g, ' ').trim().toLowerCase();
}

/** From the named heading to the next heading of the same or a higher level. Null when absent. */
export function headingSection(text: string, heading: string): string | null {
  const lines = text.replace(/\r\n?/g, '\n').split('\n');
  const want = headingKey(heading);
  let start = -1;
  let level = 0;
  let inFence = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] as string;
    if (/^ {0,3}(```|~~~)/.test(line)) inFence = !inFence;
    if (inFence) continue;
    const m = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (!m) continue;
    const lv = (m[1] as string).length;
    if (start < 0) {
      if (headingKey(m[2] as string) === want) {
        start = i;
        level = lv;
      }
    } else if (lv <= level) {
      return lines.slice(start, i).join('\n').trimEnd();
    }
  }
  return start < 0 ? null : lines.slice(start).join('\n').trimEnd();
}

/** The paragraph (or list item) carrying `^id`, without the marker. Null when absent. */
export function blockSection(text: string, id: string): string | null {
  const paras = text.replace(/\r\n?/g, '\n').split(/\n\s*\n/);
  const marker = new RegExp(`\\s\\^${id.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*$`, 'm');
  const hit = paras.find((p) => marker.test(p));
  return hit ? hit.replace(marker, '').trim() : null;
}

/** What an embed shows: the section it names, else the whole note. */
export function embedText(text: string, heading?: string, block?: string): string {
  if (block) return blockSection(text, block) ?? text;
  if (heading) return headingSection(text, heading) ?? text;
  return text;
}
