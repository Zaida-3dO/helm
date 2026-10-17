// The composer: editor, send and stop, send keys that follow the
// send mode, draft saving, the model and effort picker and the permission-mode chip.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import { ApiError } from '../api/client.ts';
import type { ChatRecord, SendAction, Submission, ThreadState } from '../api/types.ts';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';
import type { Flushers } from '../shell/quit.ts';
import { DraftSaver } from './drafts.ts';
import { Editor, type EditorHandle } from './Editor.tsx';
import { resolveKey, sendTooltip, stopTooltip, type KeyContext, type SendShortcut } from './keymap.ts';
import { ModelPicker } from './ModelPicker.tsx';
import { PermissionChip } from './PermissionChip.tsx';
import { orderPending, itemState } from './queue.ts';

export interface ComposerProps {
  chat: ChatRecord;
  thread: ThreadState | null;
  pending: Submission[];
  ops: ChatOps;
  flushers: Flushers;
  sendShortcut: SendShortcut;
  /** Alt+Up/Down on the composer reorders the newest queued message. */
  onMove: (delta: -1 | 1) => void;
  /** A recovered or withdrawn text lands here. */
  registerSetText: (f: (text: string) => void) => void;
}

const uid = () => globalThis.crypto.randomUUID();

export function isBusy(status: ChatRecord['status']): boolean {
  return status === 'running' || status === 'starting' || status === 'stopping';
}

export function Composer({ chat, thread, pending, ops, flushers, sendShortcut, onMove, registerSetText }: ComposerProps) {
  const editor = useRef<EditorHandle>(null);
  const [error, setError] = useState<string | null>(null);
  const [empty, setEmpty] = useState(!chat.draft);
  const stopping = useRef(false);
  const behavior = thread?.sendBehavior ?? HOME_SEND_BEHAVIOR;
  const busy = isBusy(chat.status);
  const ctx: KeyContext = useMemo(
    () => ({ sendShortcut, mode: behavior.mode, showChoices: behavior.showChoices }),
    [sendShortcut, behavior.mode, behavior.showChoices],
  );

  const saver = useMemo(
    () => new DraftSaver(chat.draft, chat.draftRevision, (text, rev) => ops.saveDraft(chat.id, text, rev)),
    // One saver per chat; the editor is keyed by chat as well.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [chat.id],
  );
  useEffect(() => {
    const off = flushers.add(() => saver.flush());
    return () => {
      off();
      void saver.flush();
      saver.dispose();
    };
  }, [saver, flushers]);

  // A newer draft from the host (Recover to draft, Withdraw) replaces the editor's text.
  useEffect(() => {
    if (chat.draftRevision > saver.revision) {
      saver.revision = chat.draftRevision;
      editor.current?.setText(chat.draft);
      setEmpty(!chat.draft);
    }
  }, [chat.draft, chat.draftRevision, saver]);

  useEffect(() => {
    registerSetText((text) => {
      const current = editor.current?.getText() ?? '';
      const next = current.trim() ? `${current}\n\n${text}` : text;
      editor.current?.setText(next);
      saver.edit(next);
      setEmpty(!next);
      editor.current?.focus();
    });
  }, [registerSetText, saver]);

  const newestQueued = () => orderPending(pending).filter((s) => itemState(s) === 'queued').at(-1);

  const send = useCallback(
    async (action: SendAction) => {
      const text = editor.current?.getText() ?? '';
      setError(null);
      if (!text.trim()) {
        // Empty-composer shortcuts: steer or send now the newest queued message.
        const q = newestQueued();
        if (q && action === 'steer') await ops.submissionAction(q.id, 'steerNow').catch(() => undefined);
        if (q && action === 'now') await ops.submissionAction(q.id, 'sendNow').catch(() => undefined);
        return;
      }
      const token = saver.beginSend();
      try {
        await ops.send(chat.id, uid(), text, action, saver.revision);
        if (saver.sent(token, (editor.current?.getText() ?? '') === text)) {
          editor.current?.setText('');
          setEmpty(true);
        }
      } catch (e) {
        // Refused: the text stays in the draft.
        setError(e instanceof ApiError ? e.message : String(e));
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [chat.id, ops, saver, pending],
  );

  const stop = useCallback(
    async (continueQueue: boolean) => {
      // Stop requests are de-duplicated.
      if (stopping.current) return;
      stopping.current = true;
      try {
        await ops.stop(chat.id, continueQueue);
      } catch (e) {
        setError(e instanceof ApiError ? e.message : String(e));
      } finally {
        stopping.current = false;
      }
    },
    [chat.id, ops],
  );

  const onKey = useCallback(
    (e: KeyboardEvent): boolean => {
      if (e.altKey && !e.ctrlKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
        onMove(e.key === 'ArrowUp' ? -1 : 1);
        return true;
      }
      const r = resolveKey({ key: e.key, ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, meta: e.metaKey }, ctx);
      if (r.kind === 'send') {
        void send(r.action);
        return true;
      }
      if (r.kind === 'stop-continue') {
        if (!busy) return false;
        void stop(true);
        return true;
      }
      return false;
    },
    [ctx, send, stop, busy, onMove],
  );

  const onChange = useCallback(
    (text: string) => {
      setEmpty(!text);
      saver.edit(text);
    },
    [saver],
  );

  return (
    <div className="composer">
      {error && (
        <div className="composer-error error" data-testid="composer-error">
          {error}
        </div>
      )}
      <Editor
        key={chat.id}
        initialText={chat.draft}
        placeholder={busy ? 'Steer the running task…' : 'Message Claude…'}
        onChange={onChange}
        onKey={onKey}
        handle={editor}
      />
      <div className="composer-bar">
        <ModelPicker chat={chat} ops={ops} />
        <PermissionChip mode={thread?.permissionMode ?? null} />
        <span className="dock-spacer" />
        {busy && (
          <button
            type="button"
            className="danger"
            data-testid="stop"
            title={stopTooltip(behavior.stopSendsWaiting, behavior.showChoices)}
            onClick={() => void stop(false)}
          >
            Stop
          </button>
        )}
        <button type="button" className="primary" data-testid="send" disabled={empty} title={sendTooltip(ctx, busy)} onClick={() => void send('default')}>
          {busy ? (behavior.mode === 'asap' ? 'Steer' : behavior.mode === 'queue' ? 'Queue' : 'Interrupt') : 'Send'}
        </button>
      </div>
    </div>
  );
}
