// Settings → General → Changed by chats: what the host's change list returns, and how a change reads.

export interface SettingChange {
  changeId: number;
  chatId: string;
  key: string;
  before: unknown;
  after: unknown;
  at: number;
  undone: boolean;
}

/** The host's list (newest first); anything malformed is dropped. */
export function parseChanges(v: unknown): SettingChange[] {
  const list = (v as { changes?: unknown } | null)?.changes;
  if (!Array.isArray(list)) return [];
  return list.flatMap((c): SettingChange[] => {
    const r = c as Partial<SettingChange> | null;
    if (!r || typeof r.changeId !== 'number' || typeof r.key !== 'string' || typeof r.at !== 'number') return [];
    return [{ changeId: r.changeId, chatId: String(r.chatId ?? ''), key: r.key, before: r.before ?? null, after: r.after ?? null, at: r.at, undone: r.undone === true }];
  });
}

/** One value in a sentence: `null` is the default in use, an object is cut short. */
export function describeValue(v: unknown): string {
  if (v === null || v === undefined) return 'the default';
  if (typeof v === 'boolean') return v ? 'on' : 'off';
  if (typeof v === 'string') return `"${v.length > 40 ? `${v.slice(0, 39)}…` : v}"`;
  if (typeof v === 'number') return String(v);
  const text = JSON.stringify(v) ?? '';
  return text.length > 60 ? `${text.slice(0, 59)}…` : text;
}

/** "was off, now on" */
export function changeText(c: SettingChange): string {
  return `was ${describeValue(c.before)}, now ${describeValue(c.after)}`;
}

/** A setting's key as a readable name: `compactSidebar` → "Compact sidebar". */
export function keyTitle(key: string): string {
  const words = key.replace(/([a-z0-9])([A-Z])/g, '$1 $2').toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/**
 * Who made a change, for the list: the chat's title with control and direction characters removed
 * and cut at 64 characters, so a title cannot pose as anything else. A chat that is no longer
 * listed (archived or deleted) reads as "a chat that is gone".
 */
export function changedBy(chatId: string, chats: { id: string; title: string }[]): { text: string; known: boolean } {
  const chat = chats.find((c) => c.id === chatId);
  if (!chat) return { text: 'a chat that is gone', known: false };
  // eslint-disable-next-line no-control-regex
  const clean = chat.title.replace(/[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, '').trim();
  const cut = clean.length > 64 ? `${clean.slice(0, 63)}\u2026` : clean;
  return { text: cut === '' ? 'an untitled chat' : cut, known: true };
}
