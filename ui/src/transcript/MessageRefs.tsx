// The files, folders and chats the user attached by reference, as chips in their bubble. The row
// carries each one's kind, name and target (details.refs). A chat chip opens that chat. When the
// referenced session rolled over — its archived row leaves the sidebar — the chip follows it: it
// opens where that chat lives now, and only reads as archived when there is nowhere to follow.

import { useEffect, useSyncExternalStore } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { MessageRef } from '../api/generated/MessageRef.ts';
import { useShell } from '../shell/context.ts';
import { IconChat, IconFileText, IconFolder } from '../ui/icons.tsx';
import { chatChipTarget, chatRefOpens } from './rowMeta.ts';

// The successor of each rolled-over reference we have asked about. An entry lands only when the
// answer does: key present means resolved (the value may be null), absent means still asking.
const successors = new Map<string, string | null>();
const asked = new Set<string>();
let rev = 0;
const listeners = new Set<() => void>();
function settle(): void {
  rev += 1;
  for (const l of [...listeners]) l();
}

/** Ask once per chat id where its reference should open: the newest live session of its stack. */
function followWhereOpsCanAsk(ids: readonly string[], ops: ChatOps): void {
  for (const id of ids) {
    if (successors.has(id) || asked.has(id)) continue;
    asked.add(id);
    ops
      .stack(id)
      .then((st) => successors.set(id, chatChipTarget(st.sessions, id)), () => successors.set(id, null))
      .finally(settle);
  }
}

export function MessageRefs({ refs }: { refs: MessageRef[] | undefined }) {
  const shell = useShell();
  const chats = useSyncExternalStore(shell.store.subscribe, () => shell.store.chatList());
  // A settled follow-up re-renders the chips that waited on it.
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
    () => rev,
  );
  const missing = (refs ?? []).filter((r) => r.kind === 'chat' && !chatRefOpens(chats, r.target)).map((r) => r.target);
  useEffect(() => {
    followWhereOpsCanAsk(missing, shell.ops);
    // The list of names is the subject; the store and ops are stable per shell.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [missing.join('|'), shell]);
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
        if (r.kind !== 'chat') {
          return (
            <span key={`${r.target}-${i}`} className={`msg-ref msg-ref-${r.kind}`} data-kind={r.kind} title={r.target}>
              {body}
            </span>
          );
        }
        if (chatRefOpens(chats, r.target)) {
          return (
            <button key={`${r.target}-${i}`} type="button" className="msg-ref msg-ref-chat" data-kind="chat" title={`Open ${r.name}`} onClick={() => shell.select(r.target)}>
              {body}
            </button>
          );
        }
        // A chat the list no longer holds: ask where its stack lives now (asked once per id above).
        const live = successors.has(r.target) ? successors.get(r.target) : undefined;
        if (live) {
          return (
            <button key={`${r.target}-${i}`} type="button" className="msg-ref msg-ref-chat" data-kind="chat" data-follow={live} title={`Open ${r.name}, which carries on in the chat's current session`} onClick={() => shell.select(live)}>
              {body}
              <span className="msg-ref-note">rolled over</span>
            </button>
          );
        }
        if (live === null) {
          return (
            <span key={`${r.target}-${i}`} className="msg-ref msg-ref-chat msg-ref-gone" data-kind="chat" data-gone="true" title="This chat is archived or no longer available">
              {body}
              <span className="msg-ref-note">archived</span>
            </span>
          );
        }
        // Still asking: show the chip quietly, without yet claiming it is archived.
        return (
          <span key={`${r.target}-${i}`} className="msg-ref msg-ref-chat" data-kind="chat" title={r.name}>
            {body}
          </span>
        );
      })}
    </div>
  );
}
