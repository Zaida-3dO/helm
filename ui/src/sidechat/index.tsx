// Side chats in the side panel: the `side-chat` panel kind, chat-scoped,
// at most one per source chat, never persisted. Its body is a full chat view of the side chat;
// its header offers Promote and Discard.
//
// Closing the tab re-reads the chat: an empty side chat is discarded silently; one with messages
// is promoted into an ordinary chat (so nothing is left hidden), with a toast offering Archive.

import type { ChatOps } from '../api/ops.ts';
import { registerPanelKind, panelStore } from '../panel/registry.ts';
import { toast } from '../ui/index.ts';
import { SideChatView } from './SideChatView.tsx';

export const SIDE_CHAT_KIND = 'side-chat';

export interface SideChatData {
  chatId: string;
  sourceId: string;
}

let ops: ChatOps | null = null;

/** The chat ops the kind uses (set by the chat view that mounts first). */
export function setSideChatOps(o: ChatOps): void {
  ops = o;
}

const codeOf = (e: unknown) => (e && typeof e === 'object' && 'code' in e ? String((e as { code: unknown }).code) : '');

async function resolveOnClose(chatId: string): Promise<void> {
  if (!ops) return;
  const got = await ops.get(chatId).catch(() => null);
  if (!got || !got.chat.sideSourceId) return;
  // The host knows whether the side chat sent anything: an unconfirmed discard succeeds only
  // for one that never did (its copied history does not count).
  try {
    await ops.resolveSideChat(chatId, 'discard');
    return;
  } catch (e) {
    if (codeOf(e) !== 'confirm') return;
  }
  await ops.resolveSideChat(chatId, 'promote').catch(() => undefined);
  toast.show({
    kind: 'info',
    text: 'The side chat is kept as an ordinary chat.',
    action: { label: 'Archive', run: () => void ops?.archive(chatId).catch(() => undefined) },
  });
}

registerPanelKind({
  kind: SIDE_CHAT_KIND,
  title: 'Side chat',
  scope: 'chat',
  maxTabs: 1,
  render: (tab) => {
    const d = tab.data as SideChatData;
    return <SideChatView chatId={d.chatId} />;
  },
  headerActions: (tab) => {
    const d = tab.data as SideChatData;
    return (
      <>
        <button type="button" className="small" data-testid="side-chat-promote" onClick={() => void ops?.resolveSideChat(d.chatId, 'promote').then(() => panelStore.update((m) => m.close(tab.id, 'user')))}>
          Promote
        </button>
        <button
          type="button"
          className="small"
          data-testid="side-chat-discard"
          onClick={() => {
            const close = () => panelStore.update((m) => m.close(tab.id, 'user'));
            void ops
              ?.resolveSideChat(d.chatId, 'discard')
              .then(close)
              .catch((e: unknown) => {
                if (codeOf(e) !== 'confirm') {
                  toast.show({ kind: 'error', text: String((e as Error).message ?? e) });
                  return;
                }
                // It has sent messages: discarding is for good, so ask once more.
                toast.show({
                  kind: 'info',
                  text: 'This side chat has messages. Discard it for good?',
                  action: { label: 'Discard', run: () => void ops?.resolveSideChat(d.chatId, 'discard', true).then(close) },
                });
              });
          }}
        >
          Discard
        </button>
      </>
    );
  },
  onClose: (tab) => {
    const d = tab.data as SideChatData;
    void resolveOnClose(d.chatId);
  },
});
