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
import { resolveKey, sendLabel, sendTooltip, stopTooltip, type KeyContext, type SendShortcut } from './keymap.ts';
import { Button } from '../ui/controls.tsx';
import { IconAttach, IconChat, IconFileText as IconFile, IconFolder, IconFolderPlus, IconX } from '../ui/icons.tsx';
import { SetupBanner, useRuntimeProblems } from './SetupBanner.tsx';
import { ContextStatus } from './ContextStatus.tsx';
import { useShell } from '../shell/context.ts';
import { addRef, dropLandsIn, parseFileDrop, roomLeft, type AttachedRef } from './refs.ts';
import { ModelPicker } from './ModelPicker.tsx';
import { DefaultStar } from './DefaultStar.tsx';
import { PermissionChip } from './PermissionChip.tsx';
import { orderPending, itemState } from './queue.ts';
import { bytesToBase64, isAnimated, refusal, stashSent, type Attached, type SentAttachments } from './attachments.ts';
import { Recall } from './recall.ts';
import { commandWithArgs, moveActive, SCOPE_LABEL, skillMenu, skillQuery, type Skill } from './skills.ts';
import { useHiddenSkills } from '../settings/applied.ts';
import { accessoriesAt, accessorySide, collectSendContributions, mountComposer, type AccessoryContext } from './slots.ts';
import './composer.css';
// The dock items and banners above the box share the dock sheet (ChatView mounts this composer).
import './dock.css';

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
  /** The user turned autocomplete on (off by default). */
  autocomplete?: boolean;
}

const uid = () => globalThis.crypto.randomUUID();

export function isBusy(status: ChatRecord['status']): boolean {
  return status === 'running' || status === 'starting' || status === 'stopping';
}

export function Composer({ chat, thread, pending, ops, flushers, sendShortcut, onMove, registerSetText, recall = [], autocomplete = false }: ComposerProps) {
  const editor = useRef<EditorHandle>(null);
  const [error, setError] = useState<string | null>(null);
  const [empty, setEmpty] = useState(!chat.draft);
  const stopping = useRef(false);
  // True while a send is on its way to the host: a second press does not send the message twice.
  const sendingRef = useRef(false);
  const [sending, setSending] = useState(false);
  const behavior = thread?.sendBehavior ?? HOME_SEND_BEHAVIOR;
  const busy = isBusy(chat.status);
  // Send waits while Claude Code cannot be found, and while the agent starts with nothing in flight.
  const problems = useRuntimeProblems();
  const starting = chat.status === 'starting' && pending.length === 0;
  const blocked = problems.length > 0 || starting;
  const blockedRef = useRef(blocked);
  blockedRef.current = blocked;
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
    return mountComposer(chat.id, { insert, focus: () => editor.current?.focus(), restore: (s) => restoreRef.current(s) });
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
        if (roomLeft(attachedRef.current.length, refsRef.current.length) === 0) {
          setError('A message can carry at most 20 attachments.');
          return;
        }
        const why = refusal(attachedRef.current, f);
        if (why) {
          setError(why);
          continue;
        }
        try {
          const bytes = new Uint8Array(await f.arrayBuffer());
          if (isAnimated(f.type, bytes)) {
            setError(`${f.name || 'This image'} is animated. Only still images can be pasted; attach an animation as a file with the paperclip.`);
            continue;
          }
          const r = await ops.attach(chat.id, f.name || 'image', f.type, bytesToBase64(bytes));
          const a: Attached = { id: r.attachmentId, name: f.name || 'image', size: f.size, url: URL.createObjectURL(f), file: f };
          attachedRef.current = [...attachedRef.current, a];
          setAttached(attachedRef.current);
        } catch (e) {
          setError(e instanceof ApiError ? e.message : String(e));
        }
      }
    },
    [ops, chat.id],
  );
  // Files, folders and chats by reference.
  const [refs, setRefs] = useState<AttachedRef[]>([]);
  const refsRef = useRef<AttachedRef[]>([]);
  refsRef.current = refs;
  const addRefs = useCallback(
    async (kind: 'path' | 'chat', targets: string[]) => {
      for (const t of targets) {
        if (roomLeft(attachedRef.current.length, refsRef.current.length) === 0) {
          setError('A message can carry at most 20 attachments.');
          return;
        }
        try {
          const r = await ops.attachRef(chat.id, kind, t);
          refsRef.current = addRef(refsRef.current, { id: r.attachmentId, kind: r.kind, name: r.name, target: t });
          setRefs(refsRef.current);
        } catch (e) {
          setError(e instanceof ApiError ? e.message : String(e));
        }
      }
    },
    [ops, chat.id],
  );
  const pickFiles = async () => {
    setError(null);
    try {
      const r = await ops.pickFiles();
      await addRefs('path', r.paths);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
    }
  };
  const pickFolder = async () => {
    setError(null);
    try {
      const r = await ops.pickFolder();
      if (r.path) await addRefs('path', [r.path]);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
    }
  };
  const full = roomLeft(attached.length, refs.length) === 0;
  // A taken-back message (withdrawn, recovered or edited) gives its images and files back.
  const restore = useCallback(
    (sent: SentAttachments) => {
      void addFiles(sent.files as File[]);
      for (const kind of ['path', 'chat'] as const) {
        const targets = sent.refs.filter((r) => r.kind === kind).map((r) => r.target);
        if (targets.length) void addRefs(kind, targets);
      }
    },
    [addFiles, addRefs],
  );
  const restoreRef = useRef(restore);
  restoreRef.current = restore;

  // Drops: files from Explorer arrive as the host's `file_drop` event
  // with paths; a chat row dragged from the sidebar is read from the sidebar's drag markers.
  const box = useRef<HTMLDivElement>(null);
  const [dropHint, setDropHint] = useState<{ kind: 'files' } | { kind: 'chat'; title: string } | null>(null);
  const shell = useShell();
  const boxRect = () => box.current?.getBoundingClientRect() ?? null;
  useEffect(
    () =>
      shell.bus.on('file_drop', (e) => {
        const d = parseFileDrop(e as Record<string, unknown>);
        if (!d) return;
        const inside = dropLandsIn(boxRect(), d.x, d.y, window.devicePixelRatio || 1);
        if (d.phase === 'leave') setDropHint(null);
        else if (d.phase === 'drop') {
          setDropHint(null);
          if (inside && d.paths.length) {
            setError(null);
            void addRefs('path', d.paths);
          }
        } else setDropHint((h) => (inside ? (h?.kind === 'files' ? h : { kind: 'files' }) : h?.kind === 'files' ? null : h));
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [shell, addRefs],
  );
  useEffect(() => {
    const dragged = () => document.querySelector<HTMLElement>('.sb-root[data-dragging="true"] .sb-chat-wrap[data-drag-source="true"]');
    const over = (e: PointerEvent) => dropLandsIn(boxRect(), e.clientX, e.clientY, 1);
    const move = (e: PointerEvent) => {
      const row = dragged();
      const title = row && over(e) ? document.querySelector('.sb-ghost')?.textContent?.trim() || 'this chat' : null;
      // Same hint, same object: React skips the render, so a drag does not re-render per move.
      setDropHint((h) => (title ? (h?.kind === 'chat' && h.title === title ? h : { kind: 'chat', title }) : h?.kind === 'chat' ? null : h));
    };
    // Capture: this runs before the sidebar's own pointerup clears its drag markers.
    const up = (e: PointerEvent) => {
      const row = dragged();
      const id = row?.dataset.chatId;
      setDropHint((h) => (h?.kind === 'chat' ? null : h));
      if (id && over(e) && id !== chat.id) {
        setError(null);
        void addRefs('chat', [id]);
      }
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up, true);
    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up, true);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [addRefs, chat.id]);

  const removeAttached = (id: string) =>
    setAttached((list) => {
      list.filter((a) => a.id === id).forEach((a) => URL.revokeObjectURL(a.url));
      return list.filter((a) => a.id !== id);
    });

  const send = useCallback(
    async (action: SendAction) => {
      const text = editor.current?.getText() ?? '';
      const images = attachedRef.current;
      const sentRefs = refsRef.current;
      if (blockedRef.current) return;
      setError(null);
      if (!text.trim() && images.length === 0 && sentRefs.length === 0) {
        // Empty-composer shortcuts: steer or send now the newest queued message.
        const q = newestQueued();
        if (q && action === 'steer') await ops.submissionAction(q.id, 'steerNow').catch(() => undefined);
        if (q && action === 'now') await ops.submissionAction(q.id, 'sendNow').catch(() => undefined);
        return;
      }
      if (sendingRef.current) return;
      sendingRef.current = true;
      setSending(true);
      const token = saver.beginSend();
      try {
        const extra = collectSendContributions(chat.id);
        const submissionId = uid();
        await ops.send(chat.id, submissionId, text, action, saver.revision, {
          origin: 'composer',
          context: extra.context,
          attachments: [...images.map((a) => a.id), ...sentRefs.map((r) => r.id)],
        });
        // Kept for a while, so taking the message back (withdraw, recover) restores its attachments.
        stashSent(submissionId, {
          files: images.flatMap((a) => (a.file ? [a.file] : [])),
          refs: sentRefs.map((r) => ({ kind: r.kind === 'chat' ? ('chat' as const) : ('path' as const), target: r.target })),
        });
        images.forEach((a) => URL.revokeObjectURL(a.url));
        setAttached((list) => list.filter((a) => !images.includes(a)));
        refsRef.current = refsRef.current.filter((r) => !sentRefs.includes(r));
        setRefs(refsRef.current);
        if (saver.sent(token, (editor.current?.getText() ?? '') === text)) {
          editor.current?.setText('');
          setEmpty(true);
        }
      } catch (e) {
        // Refused: the text stays in the draft.
        setError(e instanceof ApiError ? e.message : String(e));
      } finally {
        sendingRef.current = false;
        setSending(false);
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
  const commands = useMemo(() => new Set(skills.map((s) => s.name.toLowerCase())), [skills]);
  const skillPrefix = skillQuery(draftText);
  const wantsSkills = skillPrefix !== null || commandWithArgs(draftText) !== null;
  useEffect(() => {
    if (!wantsSkills || skills.length > 0) return;
    void ops
      .skills(chat.id)
      .then(setSkills)
      .catch(() => undefined);
  }, [wantsSkills, skills.length, ops, chat.id]);
  // The `/` menu: pick a command while its name is typed, then it stays shown,
  // with its description, while the arguments are typed. Escape closes it until the draft changes.
  const [menuShut, setMenuShut] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const hiddenSkills = useHiddenSkills();
  const shownSkills = useMemo(() => skills.filter((k) => !hiddenSkills.some((h) => h.toLowerCase() === k.name.toLowerCase())), [skills, hiddenSkills]);
  const menu = menuShut === draftText ? null : skillMenu(draftText, shownSkills);
  const menuKey = menu ? `${menu.mode}:${menu.items.map((x) => x.name).join(',')}` : '';
  useEffect(() => setActive(0), [menuKey]);
  const pickSkill = (name: string) => {
    showText(`/${name} `);
    editor.current?.focus();
  };

  // Autocomplete: 350 ms after typing pauses, only with the window in front and no
  // skill menu open; the host owns the 10-per-minute budget. Tab accepts while the draft is
  // unchanged; Escape or any edit drops the suggestion.
  const [ghost, setGhost] = useState<{ base: string; completion: string } | null>(null);
  const asked = useRef<string>('');
  useEffect(() => {
    setGhost((g) => (g && g.base === draftText ? g : null));
    if (!autocomplete || skillPrefix !== null || draftText.trim().length < 4 || asked.current === draftText) return;
    const t = setTimeout(() => {
      if (document.visibilityState !== 'visible' || !document.hasFocus()) return;
      asked.current = draftText;
      const base = draftText;
      void ops
        .autocomplete(chat.id, base)
        .then((r) => {
          if (r.completion && (editor.current?.getText() ?? '') === base && editor.current?.caretAtEnd()) setGhost({ base, completion: r.completion });
        })
        .catch(() => undefined);
    }, 350);
    return () => clearTimeout(t);
  }, [autocomplete, draftText, skillPrefix, ops, chat.id]);
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
      if (menu && e.key === 'Escape') {
        setMenuShut(editor.current?.getText() ?? '');
        return true;
      }
      if (menu?.mode === 'pick' && plain) {
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
          setActive((a) => moveActive(a, menu.items.length, e.key === 'ArrowDown' ? 1 : -1));
          return true;
        }
        if (e.key === 'Tab' || e.key === 'Enter') {
          const s = menu.items[active] ?? menu.items[0];
          if (s) pickSkill(s.name);
          return true;
        }
      }
      if (ghost && plain && e.key === 'Tab') {
        const current = editor.current?.getText() ?? '';
        if (current === ghost.base && editor.current?.caretAtEnd()) {
          showText(ghost.base + ghost.completion);
          setGhost(null);
          return true;
        }
      }
      if (ghost && e.key === 'Escape') {
        setGhost(null);
        return true;
      }
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
    [ctx, send, stop, busy, onMove, recall, ghost, menu, active],
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
  const toolbarStart = toolbar.filter((a) => accessorySide(a) === 'start');
  const toolbarEnd = toolbar.filter((a) => accessorySide(a) === 'end');

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
      <SetupBanner problems={problems} />
      {error && (
        <div className="composer-error error" data-testid="composer-error">
          {error}
        </div>
      )}
      {menu && (
        <div className="skill-menu" data-testid="skill-menu" data-mode={menu.mode}>
          <div className="skill-menu-head">
            <span>{menu.mode === 'pick' ? 'Commands and skills' : 'Command'}</span>
            <span className="skill-menu-keys">{menu.mode === 'pick' ? '↑ ↓ choose · ↵ insert · esc close' : 'Type its arguments, then send · esc close'}</span>
          </div>
          <div className="skill-menu-list" role="listbox" aria-label="Commands and skills">
            {menu.items.map((s, n) => (
              <button
                key={s.name}
                type="button"
                role="option"
                aria-selected={menu.mode === 'pick' && n === active}
                className={`skill-row${menu.mode === 'pick' && n === active ? ' active' : ''}`}
                data-skill={s.name}
                onMouseEnter={() => menu.mode === 'pick' && setActive(n)}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => (menu.mode === 'pick' ? pickSkill(s.name) : editor.current?.focus())}
              >
                <span className="skill-gem" aria-hidden="true" />
                <span className="skill-text">
                  <span className="skill-name">/{s.name}</span>
                  {s.description && <span className="skill-desc">{s.description}</span>}
                </span>
                {s.scope && <span className="skill-scope">{SCOPE_LABEL[s.scope] ?? s.scope}</span>}
              </button>
            ))}
          </div>
        </div>
      )}
      <div className="composer-box" data-testid="composer-box" ref={box} data-drop={dropHint ? dropHint.kind : undefined}>
        {dropHint && (
          <div className={`composer-drop composer-drop-${dropHint.kind}`} data-testid="composer-drop" aria-hidden="true">
            {dropHint.kind === 'files' ? (
              'Drop files to attach'
            ) : (
              <span className="composer-drop-card">
                <IconChat size={13} /> Drop to reference <strong>{dropHint.title}</strong>
              </span>
            )}
          </div>
        )}
        {attached.length > 0 && (
          <div className="composer-attachments" data-testid="composer-attachments">
            {attached.map((a) => (
              <span key={a.id} className="attachment-thumb" title={a.name}>
                <img src={a.url} alt={a.name} />
                <span className="attachment-thumb-name">{a.name}</span>
                <button type="button" className="attachment-thumb-x" aria-label={`Remove ${a.name}`} onClick={() => removeAttached(a.id)}>
                  <IconX size={11} strokeWidth={2.4} />
                </button>
              </span>
            ))}
          </div>
        )}
        {refs.length > 0 && (
          <div className="composer-refs" data-testid="composer-refs">
            {refs.map((r) => (
              <span key={r.id} className={`composer-ref composer-ref-${r.kind}`} title={r.kind === 'chat' ? r.name : r.target} data-kind={r.kind}>
                <span className="composer-ref-icon">{r.kind === 'chat' ? <IconChat size={14} /> : r.kind === 'folder' ? <IconFolder size={14} /> : <IconFile size={14} />}</span>
                <span className="composer-ref-text">
                  <span className="composer-ref-name">{r.name}</span>
                  <span className="composer-ref-kind">{r.kind === 'chat' ? 'Chat reference' : r.kind === 'folder' ? 'Folder reference' : 'File reference'}</span>
                </span>
                <button
                  type="button"
                  className="composer-ref-x"
                  aria-label={`Remove ${r.name}`}
                  onClick={() => {
                    refsRef.current = refsRef.current.filter((x) => x.id !== r.id);
                    setRefs(refsRef.current);
                  }}
                >
                  <IconX size={14} />
                </button>
              </span>
            ))}
          </div>
        )}
        <ContextStatus chatId={chat.id} />
        <Editor
          key={chat.id}
          initialText={chat.draft}
          placeholder={busy ? 'Steer the running task…' : 'Message Claude…'}
          onChange={onChange}
          onKey={onKey}
          handle={editor}
          ghost={ghost}
          commands={commands}
        />
        <div className="composer-bar">
          <div className="composer-selects">
            <ModelPicker chat={chat} ops={ops} />
            <DefaultStar chat={chat} />
            <PermissionChip chatId={chat.id} mode={thread?.permissionMode ?? null} busy={busy} ops={ops} />
            {toolbarStart.map((a) => (
              <span key={a.id} className="composer-accessory" data-accessory={a.id}>
                {a.render(actx)}
              </span>
            ))}
          </div>
          <div className="composer-actions">
            {toolbarEnd.map((a) => (
              <span key={a.id} className="composer-accessory" data-accessory={a.id}>
                {a.render(actx)}
              </span>
            ))}
            <Button variant="icon" className="composer-attach" label="Attach files" title={full ? 'A message can carry at most 20 attachments' : 'Attach files · up to 20, including images'} disabled={full} data-testid="attach-files" onClick={() => void pickFiles()}>
              <IconAttach size={16} />
            </Button>
            <Button variant="icon" className="composer-attach" label="Attach a folder" title={full ? 'A message can carry at most 20 attachments' : 'Attach a folder'} disabled={full} data-testid="attach-folder" onClick={() => void pickFolder()}>
              <IconFolderPlus size={16} />
            </Button>
            {busy && (
              <Button
                variant="stop"
                label="Stop"
                data-testid="stop"
                title={stopTooltip(behavior.stopSendsWaiting, behavior.showChoices)}
                onClick={() => void stop(false)}
              />
            )}
            <Button
              variant="send"
              label={sendLabel(busy, behavior.mode)}
              data-testid="send"
              disabled={blocked || sending || (empty && attached.length === 0 && refs.length === 0)}
              aria-busy={sending || undefined}
              title={problems.length > 0 ? 'Connect Claude Code to send' : starting ? 'Claude Code is starting' : sendTooltip(ctx, busy)}
              onClick={() => void send('default')}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
