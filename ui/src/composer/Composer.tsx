// The composer: editor, send and stop, send keys that follow the
// send mode, draft saving, the model and effort picker and the permission-mode chip.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import { ApiError } from '../api/client.ts';
import type { ChatRecord, SendAction, Submission, ThreadState } from '../api/types.ts';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';
import type { Flushers } from '../shell/quit.ts';
import { playCue } from '../shell/sounds.ts';
import { DraftSaver } from './drafts.ts';
import { Editor, type EditorHandle } from './Editor.tsx';
import { resolveKey, sendLabel, sendTooltip, stopTooltip, type KeyContext, type SendShortcut } from './keymap.ts';
import { Button } from '../ui/controls.tsx';
import { IconAttach, IconBolt, IconChat, IconFileText as IconFile, IconFolder, IconFolderPlus, IconSparkles, IconX } from '../ui/icons.tsx';
import { remoteChatOps } from '../api/devices.ts';
import { fastModeShown, fastModeTitle, fastModeVisible } from './fastMode.ts';
import { SetupBanner, useRuntimeCheck } from './SetupBanner.tsx';
import { ContextStatus, ContextStatusRemote } from './ContextStatus.tsx';
import { useShell } from '../shell/context.ts';
import { addRef, dropLandsIn, parseFileDrop, pathName, roomLeft, type AttachedRef } from './refs.ts';
import { ModelPicker } from './ModelPicker.tsx';
import { isLocalModel } from './modelEffort.ts';
import { PermissionChip } from './PermissionChip.tsx';
import { bytesToBase64, hasSendContent, isAnimated, refusal, unstagedOnly, type Attached } from './attachments.ts';
import { deviceOps } from '../devices/api.ts';
import { bytesFromBase64 } from '../transcript/messageImages.ts';
import { Recall } from './recall.ts';
import { ImageViewer } from '../transcript/ImageViewer.tsx';
import { applySlash, commandWithArgs, helmCommand, matchesHidden, moveActive, rankSkills, SCOPE_LABEL, skillMenu, skillQuery, slashAt, withHelmCommands, type Skill, type SlashWord } from './skills.ts';
import { SendHold, holdStep } from './hold.ts';
import { rollOverChat, SidebarActions } from '../sidebar/actions.ts';
import { SidebarData } from '../sidebar/data.ts';
import { sidebarUndo } from '../sidebar/undo.ts';
import { projectOps } from '../api/projects.ts';
import type { RuntimeSettings } from '../settings/runtime/model.ts';
import { ContextMenu, toast } from '../ui/index.ts';
import { panelOps, type QuickItem } from '../api/panel.ts';
import { isLongPaste, linkedPaste, pastedFileName } from './paste.ts';
import { applyMention, MENTION_ROWS, mentionAt, mentionText, type Mention } from './mentions.ts';
import { useScrollOnSend } from './scrollOnSend.ts';
import { MicButton } from './MicButton.tsx';
import { unconfirmedText, type RemoteComposer } from '../devices/state.ts';
import { useHiddenSkills } from '../settings/applied.ts';
import { accessoriesAt, accessorySide, announceSent, collectSendContributions, mountComposer, type AccessoryContext } from './slots.ts';
import './composer.css';
// The dock items and banners above the box share the dock sheet (ChatView mounts this composer).
import './dock.css';

// The composer's fast-mode bolt: this chat's own pick for Claude Code's fast mode. It reads the
// chat's pick off its record and falls back to the new-chat default of the PC that runs the chat
// — for a chat that runs on another PC that default is read there, the way the model picker reads
// its defaults, and the `chat_set_fast_mode` change is routed to that PC by the same path the
// model picker's picks take. It is not shown for a local-model chat (fast mode is a Claude Code
// switch) nor in the new-chat draft, where there is no chat yet to pick for.
function FastModeBolt({ chat, ops, deviceUid, draft }: { chat: ChatRecord; ops: ChatOps; deviceUid?: string; draft?: boolean }) {
  const shell = useShell();
  const [defaultOn, setDefaultOn] = useState<boolean | null>(null);
  useEffect(() => {
    if (draft) return;
    let live = true;
    const read = deviceUid
      ? remoteChatOps(shell.api)
          .composerDefaults(chat.id)
          .then((d) => (typeof d.fastMode === 'boolean' ? d.fastMode : null))
      : shell.api<RuntimeSettings>('runtime_settings_get', {}).then((s) => !!s.fastMode);
    read.then((on) => live && setDefaultOn(on)).catch(() => undefined);
    return () => {
      live = false;
    };
  }, [shell, chat.id, deviceUid, draft]);
  if (draft || !fastModeVisible(chat.model)) return null;
  const on = fastModeShown(chat.fastMode ?? null, defaultOn);
  if (on === null) return null;
  const toggle = () => {
    ops.setFastMode(chat.id, !on).catch((e: unknown) => toast.show({ kind: 'error', text: String((e as Error).message ?? e) }));
  };
  return (
    <Button
      variant="icon"
      className="composer-fast-toggle"
      label="Fast mode"
      data-testid="composer-fast-mode"
      pressed={on}
      title={fastModeTitle(on)}
      onClick={toggle}
    >
      <IconBolt size={16} />
    </Button>
  );
}

export interface ComposerProps {
  chat: ChatRecord;
  thread: ThreadState | null;
  pending: Submission[];
  ops: ChatOps;
  flushers: Flushers;
  sendShortcut: SendShortcut;
  /** A recovered or withdrawn text lands here. */
  registerSetText: (f: (text: string) => void) => void;
  /** This chat's earlier prompts, newest first, for Up/Down recall. */
  recall?: string[];
  /** The user turned autocomplete on (off by default). */
  autocomplete?: boolean;
  /**
   * The chat runs on another device (or a new chat will). Only text can be sent, nothing is read
   * from or written to this PC for it. The editor is never locked by device state: `blocked` is
   * the sentence for Send's tooltip, and `holds` makes Send wait for the device with the message.
   */
  remote?: RemoteComposer | null;
  /** The new-chat page's draft composer: the chat does not exist yet, so the fast-mode bolt is
   *  left out — the chat starts at the new-chat default and can pick its own once it exists. */
  newChatDraft?: boolean;
}


const uid = () => globalThis.crypto.randomUUID();

/** The Attach menu opens above its button: about two rows tall. */
const ATTACH_MENU_HEIGHT = 84;
/** The autocomplete marker stays at least this long after a failure. */
const AUTOCOMPLETE_MARKER_MS = 30_000;
/** When the host asks for a pause this long or longer the helper failed (a short pause is only the rate limit or a busy helper). */
const AUTOCOMPLETE_FAILED_MS = 10_000;

export function isBusy(status: ChatRecord['status']): boolean {
  return status === 'running' || status === 'starting' || status === 'stopping';
}

export function Composer({ chat, thread, pending, ops, flushers, sendShortcut, registerSetText, recall = [], autocomplete = false, remote = null, newChatDraft = false }: ComposerProps) {
  const shell = useShell();
  // For a chat that runs on another PC, attachments are staged with the device op; the send
  // carries them across by itself.
  const devOps = useMemo(() => deviceOps(shell.api), [shell.api]);
  const editor = useRef<EditorHandle>(null);
  const [error, setError] = useState<string | null>(null);
  // A send the device never confirmed: it may have arrived, so the text stays and nothing is resent.
  const [unconfirmed, setUnconfirmed] = useState<string | null>(null);
  const offBy = remote?.blocked ?? null;
  // The device the chat runs on is only away: Send holds the message rather than refusing it.
  const away = remote !== null && remote.holds;
  // One send at most waits for the device, under the id made when Send was pressed.
  const hold = useMemo(() => new SendHold(), [chat.id]);
  const [waiting, setWaiting] = useState<string | null>(null);
  const [empty, setEmpty] = useState(!chat.draft);
  const stopping = useRef(false);
  // True while a send is on its way to the host: a second press does not send the message twice.
  const sendingRef = useRef(false);
  // The draft as the composer last saw it: the empty→non-empty step is the first keystroke, which
  // warms a local chat's model so the first send is not held up loading it.
  const prevText = useRef(chat.draft ?? '');
  const [sending, setSending] = useState(false);
  const behavior = thread?.sendBehavior ?? HOME_SEND_BEHAVIOR;
  const busy = isBusy(chat.status);
  // Send waits while Claude Code cannot be found, while the app is still checking for it at
  // start-up, and while the agent starts with nothing in flight.
  const runtime = useRuntimeCheck();
  const checking = !runtime.checked;
  const starting = chat.status === 'starting' && pending.length === 0;
  // An archived chat cannot be sent to; restoring it (the banner above the box) is the way back.
  // A chat that runs on another PC is never blocked by this PC's Claude Code state. While its
  // device is only away, Send holds the message (see send()); it stays off only where the device
  // reads its chats to other devices, a refusal that does not fix itself.
  const blocked = chat.archived || starting || (remote ? !away && offBy !== null : runtime.blocked || checking);
  const blockedRef = useRef(blocked);
  blockedRef.current = blocked;
  const scrollOnSendRef = useRef(true);
  scrollOnSendRef.current = useScrollOnSend();
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
    return mountComposer(chat.id, { insert, focus: () => editor.current?.focus(), restore: () => restoreRef.current() });
  }, [registerSetText, saver, chat.id]);

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
          const id = remote
            ? (await devOps.attach({ chatId: chat.id, kind: 'image', name: f.name || 'image', mime: f.type, data: bytesToBase64(bytes) })).attachmentId
            : (await ops.attach(chat.id, f.name || 'image', f.type, bytesToBase64(bytes))).attachmentId;
          const a: Attached = { id, name: f.name || 'image', size: f.size, url: URL.createObjectURL(f) };
          attachedRef.current = [...attachedRef.current, a];
          setAttached(attachedRef.current);
        } catch (e) {
          setError(e instanceof ApiError ? e.message : String(e));
        }
      }
    },
    [ops, devOps, chat.id, remote],
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
        // A chat on another PC cannot point at a chat that runs on this one yet; files can.
        if (remote && kind === 'chat') {
          setError('A chat on another device cannot reference a chat on this PC yet.');
          continue;
        }
        try {
          if (remote) {
            const name = pathName(t);
            const r = await devOps.attach({ chatId: chat.id, kind: 'file', name, path: t });
            refsRef.current = addRef(refsRef.current, { id: r.attachmentId, kind: 'file', name, target: t });
          } else {
            const r = await ops.attachRef(chat.id, kind, t);
            refsRef.current = addRef(refsRef.current, { id: r.attachmentId, kind: r.kind, name: r.name, target: t });
          }
          setRefs(refsRef.current);
        } catch (e) {
          setError(e instanceof ApiError ? e.message : String(e));
        }
      }
    },
    [ops, devOps, chat.id, remote],
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
  const [attachMenu, setAttachMenu] = useState<{ x: number; y: number } | null>(null);
  // A very long paste is kept as a text file and attached, not poured into the draft. If the host
  // cannot keep it, the text goes into the draft after all so nothing the reader pasted is lost.
  const attachPasted = useCallback(
    async (text: string) => {
      try {
        const name = pastedFileName(text);
        if (remote) {
          // The words are staged on this PC and cross with the send as a plain-text file.
          const r = await devOps.attach({ chatId: chat.id, kind: 'text', name, text });
          refsRef.current = addRef(refsRef.current, { id: r.attachmentId, kind: 'file', name, target: name });
        } else {
          const r = await ops.attachText(chat.id, name, text);
          refsRef.current = addRef(refsRef.current, { id: r.attachmentId, kind: r.kind, name: r.name, target: r.target });
        }
        setRefs(refsRef.current);
      } catch (e) {
        setError(`${e instanceof ApiError ? e.message : String(e)} The text was pasted into the message instead.`);
        editor.current?.insert(text);
      }
    },
    [ops, devOps, chat.id, remote],
  );
  const onPasteData = useCallback(
    (data: DataTransfer): string | true | null => {
      // Images go through the attachment pipeline below.
      if ([...data.files].some((f) => f.type.startsWith('image/'))) return null;
      const text = data.getData('text/plain');
      if (!text) return null;
      if (isLongPaste(text) && roomLeft(attachedRef.current.length, refsRef.current.length) > 0) {
        setError(null);
        void attachPasted(text);
        return true;
      }
      const html = data.getData('text/html');
      return html ? linkedPaste(text, html) : null;
    },
    [attachPasted, remote],
  );
  const full = roomLeft(attached.length, refs.length) === 0;
  // Stage what the host holds unstaged for this chat: images and files staged before this window
  // opened (a restart), and the ones a taken-back send (withdrawn, recovered, refused, or never
  // sent at all) gave back. Ids already shown are left alone, so this never doubles an attachment.
  const restage = useCallback(async () => {
    if (remote) return;
    try {
      const list = await ops.stagedAttachments(chat.id);
      for (const img of unstagedOnly(list.images, attachedRef.current)) {
        if (roomLeft(attachedRef.current.length, refsRef.current.length) === 0) break;
        const t = await ops.thumb(chat.id, img.id);
        const url = URL.createObjectURL(new Blob([bytesFromBase64(t.bytes)], { type: t.mime }));
        attachedRef.current = [...attachedRef.current, { id: img.id, name: img.name, size: img.size, url }];
      }
      setAttached(attachedRef.current);
      for (const r of unstagedOnly(list.refs, refsRef.current)) {
        if (roomLeft(attachedRef.current.length, refsRef.current.length) === 0) break;
        refsRef.current = addRef(refsRef.current, { id: r.id, kind: r.kind, name: r.name, target: r.target });
      }
      setRefs(refsRef.current);
    } catch {
      // The host is not reachable: the composer keeps whatever it already shows.
    }
  }, [ops, chat.id, remote]);
  const restoreRef = useRef<() => void>(() => undefined);
  restoreRef.current = () => void restage();
  // A reopened chat stages the attachments it is holding again.
  useEffect(() => {
    void restage();
  }, [restage]);

  // Drops: files from Explorer arrive as the host's `file_drop` event
  // with paths; a chat row dragged from the sidebar is read from the sidebar's drag markers.
  const box = useRef<HTMLDivElement>(null);
  const [dropHint, setDropHint] = useState<{ kind: 'files' } | { kind: 'chat'; title: string } | null>(null);
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

  // The attached image open in the viewer, by id, so a removal or a send closes it cleanly.
  const [viewing, setViewing] = useState<string | null>(null);
  const viewingAt = viewing === null ? -1 : attached.findIndex((a) => a.id === viewing);
  const viewerImages = useMemo(() => attached.map((a) => ({ key: a.id, name: a.name, size: a.size })), [attached]);

  // Taking one out also drops it on the host, or it would be staged again the next time this chat
  // opens and go out with the next send.
  const discard = (id: string) => {
    (remote ? devOps.discard(chat.id, id) : ops.discardAttachment(chat.id, id)).catch(() => {
      // Not reachable: it only stays staged on the host, as it was before.
    });
  };
  const removeAttached = (id: string) => {
    discard(id);
    setAttached((list) => {
      list.filter((a) => a.id === id).forEach((a) => URL.revokeObjectURL(a.url));
      return list.filter((a) => a.id !== id);
    });
  };

  // The send itself: the message goes under the submission id it was given, and the host claims
  // each id once, so one id can never run twice.
  const deliver = useCallback(
    async (text: string, action: SendAction, submissionId: string) => {
      if (sendingRef.current) return;
      const images = attachedRef.current;
      const sentRefs = refsRef.current;
      setUnconfirmed(null);
      sendingRef.current = true;
      setSending(true);
      const token = saver.beginSend();
      try {
        const extra = collectSendContributions(chat.id);
        await ops.send(chat.id, submissionId, text, action, saver.revision, {
          origin: 'composer',
          context: extra.context,
          attachments: [...images.map((a) => a.id), ...sentRefs.map((r) => r.id)],
        });
        if (scrollOnSendRef.current) announceSent(chat.id);
        // The host now owns these attachments (linked to the send); if the send ever comes
        // back unread, it returns them to the chat's unstaged set and `restage` picks them up.
        images.forEach((a) => URL.revokeObjectURL(a.url));
        setAttached((list) => list.filter((a) => !images.includes(a)));
        refsRef.current = refsRef.current.filter((r) => !sentRefs.includes(r));
        setRefs(refsRef.current);
        if (saver.sent(token, (editor.current?.getText() ?? '') === text)) {
          editor.current?.setText('');
          setEmpty(true);
          prevText.current = '';
        }
        // The send cue, for the button and Enter alike (off unless the interface sounds are on).
        // A sound that cannot play never undoes a send that went through.
        try {
          playCue('send');
        } catch {
          // No audio here: the send stands.
        }
      } catch (e) {
        // Refused: the text stays in the draft. A send nobody confirmed is told apart: it may have arrived.
        if (e instanceof ApiError && e.code === 'unconfirmed') setUnconfirmed(e.message || unconfirmedText(remote?.deviceName ?? 'The device'));
        else setError(e instanceof ApiError ? e.message : String(e));
      } finally {
        sendingRef.current = false;
        setSending(false);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [chat.id, ops, saver, pending, remote],
  );

  // Archive run from here behaves as it does in the sidebar: the same undo entry and Undo toast
  // (SidebarActions is the sidebar's own, imported not edited).
  const side = useMemo(() => {
    const pops = projectOps(shell.api);
    return new SidebarActions({ api: shell.api, pops, ops, data: new SidebarData(pops), undo: sidebarUndo, onError: (m) => toast.show({ kind: 'error', text: m }) });
  }, [shell.api, shell.ops, ops]);

  const send = useCallback(
    async (action: SendAction) => {
      const text = editor.current?.getText() ?? '';
      const images = attachedRef.current;
      const sentRefs = refsRef.current;
      if (blockedRef.current) return;
      setError(null);
      if (!hasSendContent(text, images.length, sentRefs.length)) return;
      if (sendingRef.current) return;
      // A Helm command is run here, not sent to Claude; the draft clears only once it has started.
      // `/rollover` works for a chat that runs elsewhere too: the op is routed to that PC.
      const cmd = helmCommand(text)?.name;
      if (cmd === 'rollover') {
        sendingRef.current = true;
        setSending(true);
        try {
          if (await rollOverChat(ops, chat.id, (m) => toast.show({ kind: 'error', text: m }))) {
            editor.current?.setText('');
            setEmpty(true);
            prevText.current = '';
          }
        } finally {
          sendingRef.current = false;
          setSending(false);
        }
        return;
      }
      // Fork, Pin and Archive left the top bar and live in the `/` menu. Pin and Archive toggle:
      // a pinned chat unpins, an archived one restores. Archive goes through the sidebar's action,
      // so it pushes the same undo entry and Undo toast as the row menu's does.
      if (cmd === 'fork' || cmd === 'pin' || cmd === 'archive') {
        sendingRef.current = true;
        setSending(true);
        try {
          if (cmd === 'fork') shell.select((await ops.fork(chat.id)).id);
          else if (cmd === 'pin') await shell.api('chat_organize', { chatId: chat.id, pinned: !chat.pinned });
          else if (chat.archived) await ops.restore(chat.id);
          else await side.archive({ chatId: chat.id, title: chat.title });
          editor.current?.setText('');
          setEmpty(true);
          prevText.current = '';
          setSlash(null);
        } catch (e) {
          toast.show({ kind: 'error', text: String((e as Error).message ?? e) });
        } finally {
          sendingRef.current = false;
          setSending(false);
        }
        return;
      }
      if (away && remote) {
        // The device is only away: the message waits here (its draft is already saved) and goes
        // once, under this id, when the device is back. Pressing Send again replaces the wait.
        hold.hold({ submissionId: uid(), action, text, deviceUid: remote.deviceUid });
        setWaiting(remote.deviceName);
        return;
      }
      await deliver(text, action, uid());
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [chat.id, ops, saver, pending, remote, away, hold, deliver, side, shell],
  );

  // The device is live again: the waiting message goes, exactly once — however many live updates
  // follow. A refusal or an unconfirmed answer leaves it at that one; it is never resent by itself.
  // It only goes to the device it waited for: another device, or none, takes it back. While another
  // send is still starting it keeps waiting, and goes when that one has finished.
  useEffect(() => {
    const step = holdStep(hold.peek(), remote, sendingRef.current);
    if (step === 'keep') {
      if (!hold.peek()) setWaiting(null);
      return;
    }
    if (step === 'cancel') {
      hold.cancel();
      setWaiting(null);
      return;
    }
    const held = hold.release();
    setWaiting(null);
    if (held) void deliver(held.text, held.action, held.submissionId);
  }, [remote, deliver, hold, sending]);

  const stop = useCallback(
    async () => {
      // Stop requests are de-duplicated.
      if (stopping.current) return;
      stopping.current = true;
      try {
        await ops.stop(chat.id);
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
  const commands = useMemo(() => new Set(withHelmCommands(skills).map((s) => s.name.toLowerCase())), [skills]);
  const skillPrefix = skillQuery(draftText);
  // The word at the caret when a `/` is being typed, anywhere in the draft: it opens the menu
  // mid-sentence too, and a pick only replaces that word.
  const [slash, setSlash] = useState<SlashWord | null>(null);
  const wantsSkills = skillPrefix !== null || commandWithArgs(draftText) !== null || slash !== null;
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
  const shownSkills = useMemo(
    () => withHelmCommands(skills.filter((k) => !matchesHidden(k, hiddenSkills))),
    [skills, hiddenSkills],
  );
  // A `/` word at the caret opens the pick menu for its query (Helm's own commands lead it); the
  // whole-draft menu also carries the args mode, where the chosen command stays named while its
  // arguments are typed.
  const menu =
    menuShut === draftText
      ? null
      : slash
        ? (() => {
            const items = rankSkills(shownSkills, slash.query, 12);
            return items.length ? { mode: 'pick' as const, items } : null;
          })()
        : skillMenu(draftText, shownSkills);
  const menuKey = menu ? `${menu.mode}:${menu.items.map((x) => x.name).join(',')}` : '';
  useEffect(() => setActive(0), [menuKey]);
  const pickSkill = (name: string) => {
    const text = editor.current?.getText() ?? '';
    const at = slashAt(text, editor.current?.caretOffset() ?? -1);
    if (at) {
      const next = applySlash(text, at, name);
      editor.current?.setTextAt(next.text, next.caret);
      saver.edit(next.text);
      setEmpty(!next.text);
      setDraftText(next.text);
      prevText.current = next.text;
      setSlash(null);
    } else {
      showText(`/${name} `);
    }
    editor.current?.focus();
  };

  // Autocomplete: 350 ms after typing pauses, only with the window in front, no skill menu open,
  // and not for a chat on a local model (the completion is written by a Claude helper, not by the
  // chat's model); the host owns the 10-per-minute budget. Tab accepts while the draft is
  // unchanged; Escape or any edit drops the suggestion.
  const [ghost, setGhost] = useState<{ base: string; completion: string } | null>(null);
  const asked = useRef<string>('');
  useEffect(() => {
    setGhost((g) => (g && g.base === draftText ? g : null));
    if (!autocomplete || remote || isLocalModel(chat.model) || skillPrefix !== null || draftText.trim().length < 4 || asked.current === draftText) return;
    const t = setTimeout(() => {
      if (document.visibilityState !== 'visible' || !document.hasFocus()) return;
      asked.current = draftText;
      const base = draftText;
      void ops
        .autocomplete(chat.id, base)
        .then((r) => {
          if (r.completion) markAutocomplete(null);
          else if (r.retryAfterMs !== null && r.retryAfterMs >= AUTOCOMPLETE_FAILED_MS) {
            markAutocomplete(`The suggestion model did not answer, or the request limit was reached. Trying again in ${Math.ceil(r.retryAfterMs / 1000)} s.`);
          }
          if (r.completion && (editor.current?.getText() ?? '') === base && editor.current?.caretAtEnd()) setGhost({ base, completion: r.completion });
        })
        .catch((e: unknown) => markAutocomplete(e instanceof ApiError ? e.message : 'The request could not be sent.'));
    }, 350);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autocomplete, remote, chat.model, draftText, skillPrefix, ops, chat.id]);
  // A dim marker says suggestions are not coming, with the reason on hover. It stays until the
  // next suggestion that works, and at least AUTOCOMPLETE_MARKER_MS.
  const [acFail, setAcFail] = useState<{ reason: string; at: number } | null>(null);
  const acClear = useRef<ReturnType<typeof setTimeout> | null>(null);
  const markAutocomplete = (reason: string | null) => {
    if (acClear.current) clearTimeout(acClear.current);
    acClear.current = null;
    if (reason !== null) {
      setAcFail((f) => (f && f.reason === reason ? f : { reason, at: Date.now() }));
      return;
    }
    setAcFail((f) => {
      if (!f) return f;
      const left = f.at + AUTOCOMPLETE_MARKER_MS - Date.now();
      if (left > 0) acClear.current = setTimeout(() => setAcFail(null), left);
      return left > 0 ? f : null;
    });
  };
  useEffect(() => () => void (acClear.current && clearTimeout(acClear.current)), []);

  // `@` file mentions: the word at the caret, and the project's files that fit it.
  const panel = useMemo(() => panelOps(shell.api), [shell]);
  const [mention, setMention] = useState<Mention | null>(null);
  const [mentionHits, setMentionHits] = useState<{ q: string; mode: 'path' | 'files'; items: QuickItem[] } | null>(null);
  const [mentionActive, setMentionActive] = useState(0);
  // Escape closes the menu until the draft changes.
  const [mentionShut, setMentionShut] = useState<string | null>(null);
  const mentionQuery = mention?.query ?? '';
  useEffect(() => {
    // For a chat that runs on another PC the same lookup runs there: the `quick_open` op is
    // answered by the device that runs the chat, so the hits are its project's files.
    if (!mentionQuery) {
      setMentionHits(null);
      return;
    }
    let live = true;
    const t = setTimeout(() => {
      panel
        .quickOpen(mentionQuery, chat.id)
        .then((r) => {
          if (!live) return;
          setMentionHits({ q: mentionQuery, mode: r.mode, items: r.items.slice(0, MENTION_ROWS) });
          setMentionActive(0);
        })
        .catch(() => live && setMentionHits(null));
    }, 120);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [mentionQuery, panel, chat.id]);
  const mentionOpen = mention !== null && mentionHits !== null && mentionHits.q === mention.query && mentionHits.items.length > 0 && mentionShut !== draftText;
  const pickMention = (item: QuickItem) => {
    const text = editor.current?.getText() ?? '';
    const at = mentionAt(text, editor.current?.caretOffset() ?? text.length);
    if (!at || !mentionHits) return;
    const next = applyMention(text, at, mentionText(item, mentionHits.mode === 'path'), item.kind === 'dir');
    editor.current?.setTextAt(next.text, next.caret);
    saver.edit(next.text);
    setEmpty(!next.text);
    setDraftText(next.text);
    prevText.current = next.text;
    setSlash(null);
    editor.current?.focus();
  };
  const showText = (t: string) => {
    editor.current?.setText(t);
    saver.edit(t);
    setEmpty(!t);
    setDraftText(t);
  };

  const onKey = useCallback(
    (e: KeyboardEvent): boolean => {
      const plain = !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && !e.isComposing;
      if (mentionOpen && mentionHits) {
        if (e.key === 'Escape') {
          setMentionShut(editor.current?.getText() ?? '');
          return true;
        }
        if (plain && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
          setMentionActive((a) => moveActive(a, mentionHits.items.length, e.key === 'ArrowDown' ? 1 : -1));
          return true;
        }
        if (plain && (e.key === 'Tab' || e.key === 'Enter')) {
          const item = mentionHits.items[mentionActive] ?? mentionHits.items[0];
          if (item) pickMention(item);
          return true;
        }
      }
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
      if (r.kind === 'stop') {
        if (!busy) return false;
        void stop();
        return true;
      }
      return false;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [ctx, send, stop, busy, recall, ghost, menu, active, mentionOpen, mentionHits, mentionActive],
  );

  const onChange = useCallback(
    (text: string) => {
      // The first keystroke in a local chat starts loading its model now (host-side rules keep
      // this to once between turns, at most once every five minutes); a later send joins it. A
      // chat that runs on another PC warms the model there — the loading note shows from its
      // own state, and a refusal (switch off, device down) is not worth showing while typing.
      if (!prevText.current.trim() && text.trim() && isLocalModel(chat.model)) {
        void ops.warm(chat.id).catch(() => undefined);
      }
      prevText.current = text;
      setEmpty(!text);
      saver.edit(text);
      setDraftText(text);
      const caret = editor.current?.caretOffset();
      setMention(caret == null ? null : mentionAt(text, caret));
      setSlash(caret == null ? null : slashAt(text, caret));
    },
    [saver, chat.id, chat.model, ops, remote],
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
      <SetupBanner check={runtime} onRetry={runtime.retry} />
      {error && (
        <div className="composer-error error" data-testid="composer-error">
          {error}
        </div>
      )}
      {unconfirmed && (
        <div className="composer-unconfirmed" role="status" data-testid="composer-unconfirmed">
          {unconfirmed}
        </div>
      )}
      {waiting && (
        <div className="composer-hold" role="status" data-testid="composer-hold">
          <span>{`Waiting for ${waiting}: sends when it's back`}</span>
          <button
            type="button"
            className="composer-hold-cancel"
            data-testid="composer-hold-cancel"
            onClick={() => {
              hold.cancel();
              setWaiting(null);
            }}
          >
            Cancel
          </button>
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
      {mentionOpen && mentionHits && (
        <div className="skill-menu mention-menu" data-testid="mention-menu" data-mode={mentionHits.mode}>
          <div className="skill-menu-head">
            <span>Files</span>
            <span className="skill-menu-keys">↑ ↓ choose · ↵ insert · esc close</span>
          </div>
          <div className="skill-menu-list" role="listbox" aria-label="Files">
            {mentionHits.items.map((it, n) => {
              const shown = it.label.replace(/\\/g, '/');
              const cut = shown.replace(/\/$/, '').lastIndexOf('/');
              return (
                <button
                  key={it.path}
                  type="button"
                  role="option"
                  aria-selected={n === mentionActive}
                  className={`skill-row${n === mentionActive ? ' active' : ''}`}
                  data-path={it.path}
                  onMouseEnter={() => setMentionActive(n)}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => pickMention(it)}
                >
                  <span className="composer-ref-icon" aria-hidden="true">
                    {it.kind === 'dir' ? <IconFolder size={14} /> : <IconFile size={14} />}
                  </span>
                  <span className="skill-text">
                    <span className="skill-name">{cut >= 0 && mentionHits.mode === 'files' ? shown.slice(cut + 1) : shown}</span>
                    {cut >= 0 && mentionHits.mode === 'files' && <span className="skill-desc">{shown.slice(0, cut)}</span>}
                  </span>
                </button>
              );
            })}
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
                <button type="button" className="attachment-thumb-open" aria-label={`Preview ${a.name}`} data-testid="attachment-thumb" onClick={() => setViewing(a.id)}>
                  <img src={a.url} alt="" draggable={false} />
                  <span className="attachment-thumb-name">{a.name}</span>
                </button>
                <button type="button" className="attachment-thumb-x" aria-label={`Remove ${a.name}`} onClick={() => removeAttached(a.id)}>
                  <IconX size={11} strokeWidth={2.4} />
                </button>
              </span>
            ))}
          </div>
        )}
        {viewingAt >= 0 && (
          <ImageViewer
            images={viewerImages}
            index={viewingAt}
            onIndex={(i) => setViewing(attached[i]?.id ?? null)}
            onClose={() => setViewing(null)}
            src={attached[viewingAt]!.url}
            testId="attachment-viewer"
          />
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
                    discard(r.id);
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
        {remote ? <ContextStatusRemote api={shell.api} chatId={chat.id} /> : <ContextStatus chatId={chat.id} />}
        {offBy && (
          <span className="visually-hidden" data-testid="composer-remote-blocked">
            {offBy}
          </span>
        )}
        <Editor
          key={chat.id}
          initialText={chat.draft}
          placeholder={away ? `${remote?.deviceName ?? 'That PC'} is away — your message waits here and sends when it's back` : offBy ?? (busy ? 'Steer the running task…' : 'Message Claude…')}
          editable={!chat.archived}
          onChange={onChange}
          onKey={onKey}
          handle={editor}
          ghost={ghost}
          commands={commands}
          onPaste={onPasteData}
          trailing={
            <>
              {acFail && (
                <span className="editor-unavailable" title={acFail.reason} data-testid="autocomplete-unavailable">
                  <IconSparkles size={12} className="editor-unavailable-icon" /> Autocomplete unavailable
                </span>
              )}
              <span className="visually-hidden" role="status" aria-live="polite">
                {acFail ? `Autocomplete unavailable. ${acFail.reason}` : ''}
              </span>
            </>
          }
        />
        <div className="composer-bar">
          <div className="composer-selects">
            <ModelPicker chat={chat} ops={ops} loading={checking} deviceUid={remote?.deviceUid} />
            <PermissionChip chatId={chat.id} mode={thread?.permissionMode ?? null} busy={busy} ops={ops} />
            <FastModeBolt chat={chat} ops={ops} deviceUid={remote?.deviceUid} draft={newChatDraft} />
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
            {offBy === null && <MicButton editor={editor} />}
            <Button
              variant="icon"
              className="composer-attach"
              label="Attach files or folders"
              title={full ? 'A message can carry at most 20 attachments' : 'Attach files or folders · up to 20, including images'}
              disabled={full}
              aria-haspopup="menu"
              aria-expanded={attachMenu !== null}
              data-testid="attach-files"
              onClick={(e) => {
                const r = e.currentTarget.getBoundingClientRect();
                setAttachMenu({ x: r.left, y: r.top - ATTACH_MENU_HEIGHT });
              }}
            >
              <IconAttach size={16} />
            </Button>
            {attachMenu && (
              <ContextMenu
                x={attachMenu.x}
                y={attachMenu.y}
                label="Attach"
                testId="attach-menu"
                onClose={() => setAttachMenu(null)}
                items={[
                  { id: 'files', label: 'Files…', icon: <IconFile size={14} />, onSelect: () => void pickFiles() },
                  { id: 'folder', label: 'Folder…', icon: <IconFolderPlus size={14} />, onSelect: () => void pickFolder() },
                ]}
              />
            )}
            {busy && (
              <Button
                variant="stop"
                label="Stop"
                data-testid="stop"
                title={stopTooltip(behavior.showChoices)}
                onClick={() => void stop()}
              />
            )}
            <Button
              variant="send"
              label={sendLabel(busy, behavior.mode)}
              data-testid="send"
              disabled={blocked || sending || (empty && attached.length === 0 && refs.length === 0)}
              aria-busy={sending || undefined}
              title={away ? `Sends as soon as ${remote?.deviceName ?? 'that PC'} is back` : offBy ?? (chat.archived ? 'Restore this chat to send' : runtime.blocked ? 'Connect Claude Code to send' : checking ? 'Starting Claude Code…' : starting ? 'Claude Code is starting' : sendTooltip(ctx, busy))}
              onClick={() => void send('default')}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
