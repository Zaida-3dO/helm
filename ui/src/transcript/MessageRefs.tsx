// The files, folders and chats the user attached by reference, as chips in their bubble. The row
// carries each one's kind, name and target (details.refs). A chat chip opens that chat.

import { useSyncExternalStore } from 'react';
import type { MessageRef } from '../api/generated/MessageRef.ts';
import { useShell } from '../shell/context.ts';
import { IconChat, IconFileText, IconFolder } from '../ui/icons.tsx';
import { chatRefOpens } from './rowMeta.ts';

export function MessageRefs({ refs }: { refs: MessageRef[] | undefined }) {
  const shell = useShell();
  const chats = useSyncExternalStore(shell.store.subscribe, () => shell.store.chatList());
  if (!refs || refs.length === 0) return null;
  return (
    <div className="msg-refs" data-testid="message-refs">
      {refs.map((r, i) => {
        const icon = r.kind === 'chat' ? <IconChat size={13} /> : r.kind === 'folder' ? <IconFolder size={13} /> : <IconFileText size={13} />;
        const body = (
          <>
            {icon}
            <span className="msg-ref-name">{r.name}</span>
          </>
        );
        // A chat that is archived or gone is shown quietly and does not open.
        if (r.kind === 'chat' && !chatRefOpens(chats, r.target)) {
          return (
            <span key={`${r.target}-${i}`} className="msg-ref msg-ref-chat msg-ref-gone" data-kind="chat" data-gone="true" title="This chat is archived or no longer available">
              {body}
              <span className="msg-ref-note">archived</span>
            </span>
          );
        }
        return r.kind === 'chat' ? (
          <button key={`${r.target}-${i}`} type="button" className="msg-ref msg-ref-chat" data-kind="chat" title={`Open ${r.name}`} onClick={() => shell.select(r.target)}>
            {body}
          </button>
        ) : (
          <span key={`${r.target}-${i}`} className={`msg-ref msg-ref-${r.kind}`} data-kind={r.kind} title={r.target}>
            {body}
          </span>
        );
      })}
    </div>
  );
}
