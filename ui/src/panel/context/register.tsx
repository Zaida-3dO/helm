// The composer side of the side panel: the eye toggle with its
// "Looking at: …" chip in the composer toolbar (Ctrl+Alt+E), the send contributor that attaches the
// item while the toggle is on, and the review tray above the editor whose "Add to message" puts the
// pending comments into the draft as a numbered list. Nothing here ever sends.

import { useEffect, useMemo, useSyncExternalStore } from 'react';
import { panelOps } from '../../api/panel.ts';
import { composerApi, registerComposerAccessory, registerSendContributor } from '../../composer/slots.ts';
import { Button, useBinding } from '../../ui/index.ts';
import { panelStore } from '../registry.ts';
import { numbered } from './numbered.ts';
import { contextOn, currentItem, itemLabel, pendingComments, setComments } from './store.ts';

function usePanelVersion() {
  return useSyncExternalStore(panelStore.subscribe, panelStore.getVersion);
}

function ops() {
  return panelStore.api ? panelOps(panelStore.api) : null;
}

function EyeToggle({ chatId }: { chatId: string }) {
  usePanelVersion();
  const o = useMemo(() => ops(), []);
  useEffect(() => {
    if (!o || contextOn.has(chatId)) return;
    o.contextGet(chatId)
      .then((r) => {
        contextOn.set(chatId, r.on);
        panelStore.changed();
      })
      .catch(() => undefined);
  }, [o, chatId]);
  const on = contextOn.get(chatId) ?? false;
  const toggle = () => {
    if (!o) return;
    o.contextSet(chatId, !on)
      .then((r) => {
        contextOn.set(chatId, r.on);
        panelStore.changed();
      })
      .catch((e: unknown) => panelStore.showNotice(e instanceof Error ? e.message : String(e)));
  };
  useBinding({ id: 'panel.context', title: 'Send what the side panel shows', defaultKeys: 'Ctrl+Alt+E', run: toggle });
  const item = on ? currentItem(chatId) : null;
  return (
    <span className="pc-wrap">
      <Button
        variant="icon"
        label={on ? 'Stop sending the side panel item' : 'Send the side panel item with each message'}
        pressed={on}
        data-testid="panel-context-toggle"
        onClick={toggle}
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden>
          <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" />
          <circle cx="12" cy="12" r="3" />
          {!on && <path d="M3 3l18 18" />}
        </svg>
      </Button>
      {item && (
        <span className="pc-chip" data-testid="panel-context-chip" title={JSON.stringify(item).slice(0, 400)}>
          Looking at: {itemLabel(item)}
        </span>
      )}
    </span>
  );
}

function ReviewTray({ chatId }: { chatId: string }) {
  usePanelVersion();
  const o = useMemo(() => ops(), []);
  useEffect(() => {
    if (!o || pendingComments.has(chatId)) return;
    o.commentList(chatId)
      .then((l) => setComments(chatId, l))
      .catch(() => undefined);
  }, [o, chatId]);
  const list = pendingComments.get(chatId) ?? [];
  if (list.length === 0) return null;
  return (
    <div className="pc-tray" data-testid="review-tray">
      <span>
        {list.length} review comment{list.length === 1 ? '' : 's'} pending
      </span>
      <Button
        variant="text"
        data-testid="review-add-to-message"
        onClick={() => {
          if (!o) return;
          // Add to message never sends: it only puts the list into the draft.
          if (!composerApi(chatId).insert(numbered(list))) {
            panelStore.showNotice('Open the chat to add the comments to its message.');
            return;
          }
          o.commentClear(chatId)
            .then(() => setComments(chatId, []))
            .catch((e: unknown) => panelStore.showNotice(e instanceof Error ? e.message : String(e)));
        }}
      >
        Add to message
      </Button>
    </div>
  );
}

registerComposerAccessory({ id: 'panel-context', placement: 'toolbar', render: (ctx) => <EyeToggle chatId={ctx.chat.id} /> });
registerComposerAccessory({ id: 'review-tray', placement: 'above', render: (ctx) => <ReviewTray chatId={ctx.chat.id} /> });

/** While the toggle is on, each composer send carries what the panel shows next to that chat. */
registerSendContributor((chatId) => {
  if (!contextOn.get(chatId)) return undefined;
  const item = currentItem(chatId);
  return item ? { context: item } : undefined;
});

