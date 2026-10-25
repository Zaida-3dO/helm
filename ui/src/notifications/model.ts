// In-app completion notices, shown as an in-window stack: the
// post / withdraw decision, kept pure for node:test.
//
// - Posted once per completion id (a memory of the last 1,024 ids), only within 30 s of finishing,
//   never for a completion already read or dismissed, never for the chat on screen.
// - One per chat: a newer completion replaces the older notice.
// - Withdrawn when the chat is no longer unread in the attention summary (read, archived, new work
//   started, muted), or when the user opens the chat.

export const ANNOUNCED_MAX = 1024;
export const FRESH_MS = 30_000;
export const PREVIEW_MAX = 240;
export const STACK_MAX = 4;

export interface CompletionLike {
  id: string;
  chatId: string;
  title: string;
  project: string;
  preview: string;
  completedAt: number;
  read: boolean;
  dismissed: boolean;
}

export interface Notice {
  chatId: string;
  completionId: string;
  title: string;
  project: string;
  preview: string;
  at: number;
}

/** The words of a Markdown reply without its marks: `**bold**` reads as bold, a link as its text. */
export function stripMarkdown(md: string): string {
  return md
    .replace(/^[ \t]*```.*$/gm, ' ')
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/^[ \t]{0,3}(?:#{1,6}[ \t]+|>[ \t]?|[-*+][ \t]+|\d+[.)][ \t]+)/gm, '')
    .replace(/(\*\*|__|~~)(.+?)\1/g, '$2')
    .replace(/(^|[\s(])([*_])([^\s*_](?:[^*_\n]*[^\s*_])?)\2(?=$|[\s).,;:!?])/g, '$1$3')
    .replace(/`([^`\n]+)`/g, '$1');
}

/** One plain line, at most 240 characters with an ellipsis. */
export function flatten(text: string, max = PREVIEW_MAX): string {
  const one = stripMarkdown(text).replace(/\s+/g, ' ').trim();
  return one.length > max ? `${one.slice(0, max - 1)}…` : one;
}

type Listener = () => void;

export class NoticeStack {
  private notices: Notice[] = [];
  private readonly order: string[] = [];
  private readonly seen = new Set<string>();
  private readonly listeners = new Set<Listener>();
  private readonly now: () => number;

  constructor(now: () => number = Date.now) {
    this.now = now;
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  list = (): Notice[] => this.notices;

  private set(next: Notice[]): void {
    this.notices = next;
    for (const l of [...this.listeners]) l();
  }

  private remember(id: string): boolean {
    if (this.seen.has(id)) return false;
    this.seen.add(id);
    this.order.push(id);
    while (this.order.length > ANNOUNCED_MAX) {
      const old = this.order.shift();
      if (old) this.seen.delete(old);
    }
    return true;
  }

  /** A completion event. `onScreen` is the chat the user is looking at (window focused). */
  completion(c: CompletionLike, onScreen: string | null): boolean {
    if (c.read || c.dismissed) return false;
    if (!this.remember(c.id)) return false;
    if (this.now() - c.completedAt > FRESH_MS) return false;
    if (c.chatId === onScreen) return false;
    const n: Notice = {
      chatId: c.chatId,
      completionId: c.id,
      title: c.title.trim() || 'Task finished',
      project: c.project,
      preview: flatten(c.preview),
      at: this.now(),
    };
    const rest = this.notices.filter((x) => x.chatId !== c.chatId);
    this.set([...rest, n].slice(-STACK_MAX));
    return true;
  }

  /** The attention summary changed: withdraw notices whose chat is no longer unread. */
  summary(unreadChatIds: string[]): void {
    const keep = new Set(unreadChatIds);
    const next = this.notices.filter((n) => keep.has(n.chatId));
    if (next.length !== this.notices.length) this.set(next);
  }

  /** The user opened or dismissed a chat's notice. */
  withdraw(chatId: string): void {
    if (this.notices.some((n) => n.chatId === chatId)) this.set(this.notices.filter((n) => n.chatId !== chatId));
  }

  clear(): void {
    if (this.notices.length) this.set([]);
  }
}
