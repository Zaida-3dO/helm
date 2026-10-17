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
import { refusal, toBase64, type Attached } from './attachments.ts';
import { Recall } from './recall.ts';
import { rankSkills, skillQuery, type Skill } from './skills.ts';
import { accessoriesAt, collectSendContributions, mountComposer, type AccessoryContext } from './slots.ts';

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
  /** This chat's earlier prompts, newest first, for Up/Down recall. */
  recall?: string[];
}

const uid = () => globalThis.crypto.randomUUID();

export function isBusy(status: ChatRecord['status']): boolean {
  return status === 'running' || status === 'starting' || status === 'stopping';
}

export function Composer({ chat, thread, pending, ops, flushers, sendShortcut, onMove, registerSetText, recall = [] }: ComposerProps) {
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
    const insert = (text: string) => {
      const current = editor.current?.getText() ?? '';
      const next = current.trim() ? `${current}\n\n${text}` : text;
      editor.current?.setText(next);
      saver.edit(next);
      setEmpty(!next);
      editor.current?.focus();
    };
    registerSetText(insert);
    // composerApi(chatId).insert(...) reaches this composer (slots.ts).
    return mountComposer(chat.id, { insert, focus: () => editor.current?.focus() });
  }, [registerSetText, saver, chat.id]);

  const newestQueued = () => orderPending(pending).filter((s) => itemState(s) === 'queued').at(-1);

  // Attached images: stored on add, sent by id, thumbnails as blob: URLs.
  const [attached, setAttached] = useState<Attached[]>([]);
  const attachedRef = useRef<Attached[]>([]);
  attachedRef.current = attached;
  useEffect(() => () => attachedRef.current.forEach((a) => URL.revokeObjectURL(a.url)), []);
  const addFiles = useCallback(
    async (files: File[]) => {
      for (const f of files) {
        const why = refusal(attachedRef.current, f);
        if (why) {
          setError(why);
          continue;
        }
        try {
          const r = await ops.attach(chat.id, f.name || 'image', f.type, await toBase64(f));
          const a: Attached = { id: r.attachmentId, name: f.name || 'image', size: f.size, url: URL.createObjectURL(f) };
          attachedRef.current = [...attachedRef.current, a];
          setAttached(attachedRef.current);
        } catch (e) {
          setError(e instanceof ApiError ? e.message : String(e));
        }
      }
    },
    [ops, chat.id],
  );
  const removeAttached = (id: string) =>
    setAttached((list) => {
      list.filter((a) => a.id === id).forEach((a) => URL.revokeObjectURL(a.url));
      return list.filter((a) => a.id !== id);
    });

  const send = useCallback(
    async (action: SendAction) => {
      const text = editor.current?.getText() ?? '';
      const images = attachedRef.current;
      setError(null);
      if (!text.trim() && images.length === 0) {
        // Empty-composer shortcuts: steer or send now the newest queued message.
        const q = newestQueued();
        if (q && action === 'steer') await ops.submissionAction(q.id, 'steerNow').catch(() => undefined);
        if (q && action === 'now') await ops.submissionAction(q.id, 'sendNow').catch(() => undefined);
        return;
      }
      const token = saver.beginSend();
      try {
        const extra = collectSendContributions(chat.id);
        await ops.send(chat.id, uid(), text, action, saver.revision, {
          origin: 'composer',
          context: extra.context,
          attachments: images.map((a) => a.id),
        });
        images.forEach((a) => URL.revokeObjectURL(a.url));
        setAttached((list) => list.filter((a) => !images.includes(a)));
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

  // Prompt recall and skill completion.
  const recaller = useRef(new Recall());
  const [draftText, setDraftText] = useState(chat.draft);
  const [skills, setSkills] = useState<Skill[]>([]);
  const skillPrefix = skillQuery(draftText);
  useEffect(() => {
    if (skillPrefix === null || skills.length > 0) return;
    void ops
      .skills(chat.id)
      .then(setSkills)
      .catch(() => undefined);
  }, [skillPrefix, skills.length, ops, chat.id]);
  const skillMatches = skillPrefix === null ? [] : rankSkills(skills, skillPrefix);
  const showText = (t: string) => {
    editor.current?.setText(t);
    saver.edit(t);
    setEmpty(!t);
    setDraftText(t);
  };

  const onKey = useCallback(
    (e: KeyboardEvent): boolean => {
      if (e.altKey && !e.ctrlKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
        onMove(e.key === 'ArrowUp' ? -1 : 1);
        return true;
      }
      const plain = !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && !e.isComposing;
      if (plain && (e.key === 'ArrowUp' || e.key === 'ArrowDown') && attachedRef.current.length === 0) {
        const current = editor.current?.getText() ?? '';
        const next = e.key === 'ArrowUp' ? recaller.current.up(recall, current) : recaller.current.down(current);
        if (next !== null) {
          showText(next);
          return true;
        }
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [ctx, send, stop, busy, onMove, recall],
  );

  const onChange = useCallback(
    (text: string) => {
      setEmpty(!text);
      saver.edit(text);
      setDraftText(text);
    },
    [saver],
  );

  const actx: AccessoryContext = { chat, thread, busy, draftEmpty: empty };
  const above = accessoriesAt('above');
  const toolbar = accessoriesAt('toolbar');

  return (
    <div
      className="composer"
      onPaste={(e) => {
        const files = [...e.clipboardData.files].filter((f) => f.type.startsWith('image/'));
        if (files.length) {
          e.preventDefault();
          void addFiles(files);
        }
      }}
      onDragOver={(e) => {
        if ([...e.dataTransfer.items].some((i) => i.kind === 'file')) e.preventDefault();
      }}
      onDrop={(e) => {
        const files = [...e.dataTransfer.files];
        if (files.length) {
          e.preventDefault();
          void addFiles(files);
        }
      }}
    >
      {above.length > 0 && (
        <div className="composer-above">
          {above.map((a) => (
            <div key={a.id} className="composer-accessory" data-accessory={a.id}>
              {a.render(actx)}
            </div>
          ))}
        </div>
      )}
      {error && (
        <div className="composer-error error" data-testid="composer-error">
          {error}
        </div>
      )}
      {attached.length > 0 && (
        <div className="composer-attachments" data-testid="composer-attachments">
          {attached.map((a) => (
            <span key={a.id} className="attachment-thumb" title={a.name}>
              <img src={a.url} alt={a.name} />
              <button type="button" className="small link" aria-label={`Remove ${a.name}`} onClick={() => removeAttached(a.id)}>
                ×
              </button>
            </span>
          ))}
        </div>
      )}
      {skillMatches.length > 0 && (
        <div className="skill-menu" role="listbox" aria-label="Skills" data-testid="skill-menu">
          {skillMatches.map((s) => (
            <button key={s.name} type="button" role="option" className="small" title={s.description} onClick={() => showText(`/${s.name} `)}>
              /{s.name}
            </button>
          ))}
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
        {toolbar.map((a) => (
          <span key={a.id} className="composer-accessory" data-accessory={a.id}>
            {a.render(actx)}
          </span>
        ))}
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
        <button type="button" className="primary" data-testid="send" disabled={empty && attached.length === 0} title={sendTooltip(ctx, busy)} onClick={() => void send('default')}>
          {busy ? (behavior.mode === 'asap' ? 'Steer' : behavior.mode === 'queue' ? 'Queue' : 'Interrupt') : 'Send'}
        </button>
      </div>
    </div>
  );
}
