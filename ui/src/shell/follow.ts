// What a `shell_open_chat` event does. The host always sends it when a chat starts another
// (a roll over, an agent's app_open_chat); when it carries `ifViewing`, the window is only to
// be moved while the user is still looking at that chat — otherwise the user gets a notice with
// an Open button, so a chat the app starts never pulls the window away mid-read. Events without
// `ifViewing` (the tray, the pet, a toast) always follow.

export interface OpenEvent {
  chatId?: unknown;
  ifViewing?: unknown;
  title?: unknown;
  messageId?: unknown;
}

export type OpenDecision =
  | { kind: 'select'; chatId: string; messageId?: string }
  | { kind: 'notice'; chatId: string; title: string | null };

/** Decide what to do with the event, given the chat currently on screen. */
export function decideOpen(event: OpenEvent, currentChatId: string | null): OpenDecision | null {
  const chatId = typeof event.chatId === 'string' && event.chatId !== '' ? event.chatId : null;
  if (!chatId) return null;
  const ifViewing = typeof event.ifViewing === 'string' && event.ifViewing !== '' ? event.ifViewing : null;
  if (ifViewing === null || ifViewing === currentChatId) {
    const messageId =
      typeof event.messageId === 'number' && Number.isFinite(event.messageId)
        ? String(event.messageId)
        : typeof event.messageId === 'string' && event.messageId !== ''
          ? event.messageId
          : undefined;
    return { kind: 'select', chatId, messageId };
  }
  const title = typeof event.title === 'string' && event.title.trim() !== '' ? event.title : null;
  return { kind: 'notice', chatId, title };
}

/**
 * The window to open for a chat id. A session that rolled over is no longer where the chat lives:
 * when the stack names a newer session, that one opens — one window spans the whole stack, and a
 * message to land on is resolved across it. `latestId` is null when the chat never rolled over.
 */
export function openTarget(chatId: string, latestId: string | null | undefined): string {
  return typeof latestId === 'string' && latestId !== '' && latestId !== chatId ? latestId : chatId;
}

/**
 * A roll over hands the window on from the old session to its successor, so the side panel goes
 * with it: the tabs the reader had open, the selected one. The open event names the old chat under
 * `ifViewing`, but an agent starting a chat names its own chat the same way, so the successor must
 * say it came from there (`prevSessionId`) before anything moves. `carry` is told once; nothing
 * moves for any other kind of open, or when the chat cannot be read.
 */
export async function carryOnRollOver(
  event: OpenEvent,
  loadChat: (chatId: string) => Promise<{ prevSessionId: string | null }>,
  carry: (from: string, to: string) => void,
): Promise<boolean> {
  const to = typeof event.chatId === 'string' && event.chatId !== '' ? event.chatId : null;
  const from = typeof event.ifViewing === 'string' && event.ifViewing !== '' ? event.ifViewing : null;
  if (!to || !from || to === from) return false;
  try {
    if ((await loadChat(to)).prevSessionId !== from) return false;
  } catch {
    return false;
  }
  carry(from, to);
  return true;
}

/** The draft of a chat, as far as carrying it between two sessions needs. */
export interface DraftHolder {
  draft: string;
  draftRevision: number;
}

/** What the successor's draft becomes when the old session still holds text of its own. */
export function mergedDraft(old: string, successor: string): string {
  if (successor.trim() === '' || old.includes(successor.trim())) return old;
  if (successor.includes(old.trim())) return successor;
  return `${successor}\n\n${old}`;
}

/**
 * Text typed into the old session's composer after the host moved its draft on (or in the last
 * moments before) is saved to the old session only, which is archived. When the window follows,
 * it goes on to the successor: into an empty draft as it is, over a draft that is the earlier
 * part of the same text, and after a draft that holds something else, never over it. The old
 * session's draft is emptied so a restored old chat does not offer it a second time. Resolves
 * true when something was carried.
 */
export async function carryDraft(
  from: string,
  to: string,
  load: (chatId: string) => Promise<DraftHolder>,
  save: (chatId: string, text: string, revision: number) => Promise<unknown>,
  now: () => number = Date.now,
): Promise<boolean> {
  try {
    const [old, next] = [await load(from), await load(to)];
    if (old.draft.trim() === '') return false;
    const text = mergedDraft(old.draft, next.draft);
    if (text !== next.draft) await save(to, text, Math.max(now(), next.draftRevision + 1));
    await save(from, '', Math.max(now(), old.draftRevision + 1));
    return true;
  } catch {
    return false;
  }
}

/** What the window needs to follow a roll over from the old session to its successor. */
export interface RollOverDeps {
  loadChat: (chatId: string) => Promise<{ prevSessionId: string | null; draft: string; draftRevision: number }>;
  /** Hand the side panel's tabs and selection from one session to the next. */
  carryPanel: (from: string, to: string) => void;
  /** Save every composer's unsaved text, so the old session holds what was typed. */
  flush: () => Promise<unknown>;
  saveDraft: (chatId: string, text: string, revision: number) => Promise<unknown>;
}

/**
 * The window follows a roll over: the side panel goes with it, then any text typed in the old
 * composer is saved and carried to the successor. Nothing moves for an open that is not a roll
 * over. Resolves true when the window's state was carried.
 */
export async function followRollOver(event: OpenEvent, deps: RollOverDeps): Promise<boolean> {
  if (!(await carryOnRollOver(event, deps.loadChat, deps.carryPanel))) return false;
  await deps.flush().catch(() => undefined);
  await carryDraft(event.ifViewing as string, event.chatId as string, deps.loadChat, deps.saveDraft);
  return true;
}
