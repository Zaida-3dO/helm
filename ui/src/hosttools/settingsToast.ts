// The settings-change toast's model: a chat changed settings,
// the user sees what changed for 8 s with Undo and Open chat. It has its own slot, beside the
// main toast slot, so it never hides an error.

export const SETTINGS_TOAST_MS = 8000;

export interface SettingsChange {
  changeId: number;
  key: string;
}

export interface SettingsNotice {
  chatId: string;
  changeIds: number[];
  text: string;
  expiresAt: number;
}

/** Read an `external_settings` event; null when it is malformed. */
export function noticeFrom(event: unknown, now: number): SettingsNotice | null {
  if (!event || typeof event !== 'object') return null;
  const e = event as { chatId?: unknown; changes?: unknown };
  if (typeof e.chatId !== 'string' || !Array.isArray(e.changes)) return null;
  const changes = e.changes.filter(
    (c): c is SettingsChange => !!c && typeof (c as SettingsChange).changeId === 'number' && typeof (c as SettingsChange).key === 'string',
  );
  if (changes.length === 0) return null;
  const keys = changes.map((c) => c.key);
  const list = keys.length <= 3 ? keys.join(', ') : `${keys.slice(0, 3).join(', ')} and ${keys.length - 3} more`;
  return {
    chatId: e.chatId,
    changeIds: changes.map((c) => c.changeId),
    text: `A chat changed ${keys.length === 1 ? 'a setting' : `${keys.length} settings`}: ${list}`,
    expiresAt: now + SETTINGS_TOAST_MS,
  };
}

type Listener = () => void;

/** The second toast slot: one settings notice at a time, the newest wins. */
export class SettingsToastSlot {
  private notice: SettingsNotice | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private readonly listeners = new Set<Listener>();
  private readonly now: () => number;

  constructor(now: () => number = Date.now) {
    this.now = now;
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => void this.listeners.delete(l);
  };

  current = (): SettingsNotice | null => this.notice;

  show(event: unknown): void {
    const n = noticeFrom(event, this.now());
    if (!n) return;
    this.set(n);
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.tick(), SETTINGS_TOAST_MS + 5);
  }

  dismiss(): void {
    clearTimeout(this.timer);
    this.set(null);
  }

  tick(): void {
    if (this.notice && this.notice.expiresAt <= this.now()) this.set(null);
  }

  private set(n: SettingsNotice | null): void {
    this.notice = n;
    for (const l of [...this.listeners]) l();
  }
}

export const settingsToast = new SettingsToastSlot();
