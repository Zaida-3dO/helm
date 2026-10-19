// A fake host for checking the UI in a plain browser. It implements the host's ops and
// events with a scripted turn: streamed text, a tool card, and an approval, question or plan
// card when the prompt asks for one ("approve", "question", "plan"). "long" in a prompt adds a
// background task. A steer sent mid-turn shows as Pending and is read about 1.5 s later.
//
// Dev builds only: main.tsx imports it behind `import.meta.env.DEV`, so production drops it.

import type { Invoke } from '../api/client.ts';
import type {
  ChatRecord,
  Interaction,
  MessageRow,
  SendBehavior,
  Submission,
  ThreadState,
  TranscriptPage,
} from '../api/types.ts';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';
import { leafFakeOps } from './fake-leaf.ts';
import { projectsFakeOps } from './fake-projects.ts';
import { panelFakeOps } from './fake-panel.ts';
import { shellFakeOps } from './fake-shell.ts';
import { mcpFakeOps } from './fake-mcp.ts';
import { hosttoolsFakeOps } from './fake-hosttools.ts';
import { usageFakeOps } from './fake-usage.ts';
import { itemLinksFakeOps } from './fake-itemlinks.ts';
import { orchestratorFakeOps } from './fake-orchestrator.ts';
import { newChatFakeOps } from '../newchat/fake.ts';

type Emit = (payload: unknown) => void;

/** What another area's fake ops get from the fake host. */
export interface FakeOpContext {
  emit: Emit;
  /** The fake host's own ops, to call through (for example `chat_list`). */
  call: (op: string, args?: Record<string, unknown>) => unknown;
}

export type FakeOpHandler = (op: string, args: Record<string, unknown>, ctx: FakeOpContext) => unknown;

const extraOps: { prefixes: string[]; handler: FakeOpHandler }[] = [];

/**
 * Each area's `ui/src/dev/fake-<area>.ts` implements its own ops and registers
 * them here with one line, e.g. `registerFakeOps(['project_', 'sidebar_'], handler)`. An op is
 * routed to the first registration with a matching prefix (an exact name counts as a prefix);
 * the fake host's own ops win over registrations.
 */
export function registerFakeOps(prefixes: string[], handler: FakeOpHandler): void {
  extraOps.push({ prefixes, handler });
}

{ const leaf = leafFakeOps(); registerFakeOps(leaf.prefixes, leaf.handler); }
{ const sb = projectsFakeOps(); registerFakeOps(sb.prefixes, sb.handler); }
{ const panel = panelFakeOps(); registerFakeOps(panel.prefixes, panel.handler); }
{ const sh = shellFakeOps(); registerFakeOps(sh.prefixes, sh.handler); }
{ const mcp = mcpFakeOps(); registerFakeOps(mcp.prefixes, mcp.handler); }
{ const ht = hosttoolsFakeOps(); registerFakeOps(ht.prefixes, ht.handler); }
{ const us = usageFakeOps(); registerFakeOps(us.prefixes, us.handler); }
{ const il = itemLinksFakeOps(); registerFakeOps(il.prefixes, il.handler); }
{ const oc = orchestratorFakeOps(); registerFakeOps(oc.prefixes, oc.handler); }
{ const nc = newChatFakeOps(); registerFakeOps(nc.prefixes, nc.handler); }

export function fakeOpFor(op: string): FakeOpHandler | undefined {
  return extraOps.find((r) => r.prefixes.some((p) => op.startsWith(p)))?.handler;
}

interface FakeChat {
  chat: ChatRecord;
  thread: ThreadState;
  rows: MessageRow[];
  pending: Submission[];
  turn: { cancel: () => void } | null;
}

const PAGE = 100;

/** The last part of a folder path, either slash. */
const folderName = (p: string) => p.split(/[\\/]/).pop() ?? '';

export function createFakeHost(emit: Emit, opts: { seedRows?: number; stepMs?: number } = {}): Invoke {
  const step = opts.stepMs ?? 80;
  let revision = 1;
  let rowId = 1;
  const chats = new Map<string, FakeChat>();
  let sendBehavior: SendBehavior = { ...HOME_SEND_BEHAVIOR };
  const prefs = new Map<string, unknown>();
  const answers = new Map<string, (a: Record<string, unknown>) => void>();

  const rev = () => ++revision;
  const now = () => Date.now();
  const uid = () => (globalThis.crypto?.randomUUID?.() ?? `id-${Math.random().toString(16).slice(2)}`);

  function emitChat(c: FakeChat) {
    c.chat = { ...c.chat, revision: rev(), updatedAt: now() };
    emit({ kind: 'chat', chat: c.chat });
  }
  function emitThread(c: FakeChat) {
    c.thread = { ...c.thread, revision: rev() };
    emit({ kind: 'thread', chatId: c.chat.id, thread: c.thread });
  }
  function emitRows(c: FakeChat, rows: MessageRow[]) {
    emit({ kind: 'message', chatId: c.chat.id, revision, rows });
  }
  function emitQueue(c: FakeChat) {
    emit({ kind: 'queue', chatId: c.chat.id, revision: rev(), pending: c.pending });
  }
  function setStatus(c: FakeChat, status: ChatRecord['status']) {
    const dropCompletion = status !== 'idle' && status !== 'interrupted';
    c.chat = { ...c.chat, status, unread: dropCompletion ? { ...c.chat.unread, completionId: null } : c.chat.unread };
    c.thread = { ...c.thread, status };
    emitChat(c);
    emitThread(c);
    emitAttention();
  }

  function emitAttention() {
    emit({ kind: 'attention', summary: { ...attention(), revision: rev() } });
  }

  /** A clean finish leaves one unread completion. */
  function complete(c: FakeChat) {
    const last = [...c.rows].reverse().find((r) => r.role === 'assistant');
    const id = uid();
    c.chat = { ...c.chat, unread: { ...c.chat.unread, completionId: id } };
    emit({
      kind: 'completion',
      completion: { id, chatId: c.chat.id, title: c.chat.title, project: folderName(c.chat.cwd), preview: (last?.text ?? '').slice(0, 240), completedAt: now(), read: false, dismissed: false },
    });
    emitChat(c);
    emitAttention();
  }

  function addRow(c: FakeChat, p: Partial<MessageRow> & Pick<MessageRow, 'role'>): MessageRow {
    const id = rowId++;
    const r: MessageRow = {
      id,
      chatId: c.chat.id,
      sourceKey: p.sourceKey ?? `row-${id}`,
      text: '',
      thinking: '',
      thinkingTail: null,
      toolName: null,
      toolCallId: null,
      status: 'complete',
      timestamp: now(),
      sequence: id,
      revision: rev(),
      artifact: null,
      details: {},
      ...p,
    };
    c.rows.push(r);
    emitRows(c, [r]);
    return r;
  }
  function updateRow(c: FakeChat, r: MessageRow, p: Partial<MessageRow>): MessageRow {
    const next = { ...r, ...p, revision: rev() };
    c.rows = c.rows.map((x) => (x.id === r.id ? next : x));
    emitRows(c, [next]);
    return next;
  }

  function newChat(cwd: string): FakeChat {
    const id = uid();
    const t = now();
    const c: FakeChat = {
      chat: {
        id,
        projectId: `p:${cwd}`,
        cwd,
        title: 'New chat',
        status: 'idle',
        error: null,
        harness: 'claude',
        model: 'claude-opus-5-5',
        effort: 'xhigh',
        draft: '',
        draftRevision: 0,
        createdAt: t,
        updatedAt: t,
        revision: rev(),
        pinned: false,
        archived: false,
        parentChatId: null,
        sideSourceId: null,
        titleState: 'fallback',
        unread: { markToken: null, completionId: null },
        muted: false,
        lastActivityAt: t,
        stalled: false,
      },
      thread: {
        chatId: id,
        status: 'idle',
        permissionMode: 'bypassPermissions',
        interactions: [],
        liveTools: [],
        backgroundTasks: [],
        compacting: null,
        capabilities: { withdraw: true, deliverNow: true, stopTool: true, compact: true },
        sendBehavior,
        queueHeld: false,
        revision: rev(),
      },
      rows: [],
      pending: [],
      turn: null,
    };
    chats.set(id, c);
    return c;
  }

  function seed(c: FakeChat, n: number) {
    const base = now() - n * 60_000;
    for (let i = 0; i < n; i++) {
      const id = rowId++;
      const user = i % 3 === 0;
      c.rows.push({
        id,
        chatId: c.chat.id,
        sourceKey: `seed-${id}`,
        role: user ? 'user' : i % 3 === 1 ? 'toolResult' : 'assistant',
        text: user ? `Seeded question ${i}` : i % 3 === 1 ? `output line ${i}\n`.repeat(1 + (i % 5)) : `Seeded answer ${i}. `.repeat(1 + (i % 7)),
        thinking: '',
        thinkingTail: null,
        toolName: i % 3 === 1 ? 'Bash' : null,
        toolCallId: i % 3 === 1 ? `seed-tool-${id}` : null,
        status: 'complete',
        timestamp: base + i * 60_000,
        sequence: id,
        revision: rev(),
        artifact: null,
        details: i % 3 === 1 ? { toolArgs: JSON.stringify({ command: `echo ${i}` }) } : {},
      });
    }
  }

  function sleep(ms: number, cancelled: () => boolean) {
    return new Promise<void>((resolve, reject) =>
      setTimeout(() => (cancelled() ? reject(new Error('stopped')) : resolve()), ms),
    );
  }

  async function ask(c: FakeChat, req: Omit<Interaction, 'chatId' | 'id'>): Promise<Record<string, unknown>> {
    const id = uid();
    const interaction: Interaction = { chatId: c.chat.id, id, ...req };
    c.thread = { ...c.thread, interactions: [...c.thread.interactions, interaction] };
    emitThread(c);
    const a = await new Promise<Record<string, unknown>>((resolve) => answers.set(id, resolve));
    c.thread = { ...c.thread, interactions: c.thread.interactions.filter((i) => i.id !== id) };
    emitThread(c);
    return a;
  }

  async function streamText(c: FakeChat, text: string, cancelled: () => boolean, thinking = '') {
    let r = addRow(c, { role: 'assistant', status: 'streaming', thinking: '' });
    const words = text.split(/(?<=\s)/);
    if (thinking) {
      for (let i = 0; i < thinking.length; i += 40) {
        await sleep(step, cancelled);
        r = updateRow(c, r, { thinking: thinking.slice(0, i + 40) });
      }
    }
    let shown = '';
    for (let i = 0; i < words.length; i += 3) {
      await sleep(step, cancelled);
      shown += words.slice(i, i + 3).join('');
      r = updateRow(c, r, { text: shown });
    }
    updateRow(c, r, { status: 'complete', details: { usage: { inputTokens: 1200, outputTokens: 300, cacheReadTokens: 0, cacheWriteTokens: 0, costUsd: 0.01 } } });
  }

  async function tool(c: FakeChat, name: string, args: Record<string, unknown>, output: string, cancelled: () => boolean, ms = 900) {
    const toolCallId = uid();
    let r = addRow(c, { role: 'toolResult', toolName: name, toolCallId, status: 'streaming', details: { toolArgs: JSON.stringify(args), stoppable: true } });
    c.thread = { ...c.thread, liveTools: [...c.thread.liveTools, { toolCallId, name, startedAt: now(), stoppable: name === 'Bash' }] };
    emitThread(c);
    try {
      await sleep(ms, cancelled);
      r = updateRow(c, r, { status: 'complete', text: output });
    } finally {
      c.thread = { ...c.thread, liveTools: c.thread.liveTools.filter((t) => t.toolCallId !== toolCallId) };
      emitThread(c);
    }
    // A steer is read at the tool boundary.
    deliverSteers(c);
  }

  function deliverSteers(c: FakeChat) {
    const steers = c.pending.filter((s) => s.mode === 'steer' && s.state === 'accepted');
    if (!steers.length) return;
    for (const s of steers) addRow(c, { role: 'user', text: s.text, sourceKey: s.id, details: { submissionId: s.id, mode: 'steer' } });
    c.pending = c.pending.filter((s) => !steers.includes(s));
    emitQueue(c);
  }

  async function runTurn(c: FakeChat, text: string) {
    let stopped = false;
    const cancelled = () => stopped;
    c.turn = { cancel: () => (stopped = true) };
    setStatus(c, 'running');
    try {
      await sleep(step * 2, cancelled);
      await streamText(c, `Looking into "${text.slice(0, 40)}". I'll check the folder first. `, cancelled, 'The user wants something. **First** I should look at the files, then decide.');
      if (/long/i.test(text)) {
        c.thread = { ...c.thread, backgroundTasks: [{ id: uid(), title: 'npm test', command: 'npm test', kind: 'shell', startedAt: now() }] };
        emitThread(c);
      }
      if (/approve/i.test(text)) {
        const a = await ask(c, {
          method: 'select',
          attention: 'approval',
          title: 'Allow Bash?',
          message: 'Claude wants to run a command.',
          context: [{ label: 'Command', text: 'npm run build' }],
          options: ['Allow once', 'Allow for this session', 'Deny', 'Stop turn'],
        });
        if (a.value === 'Stop turn') throw new Error('stopped');
        if (a.value === 'Deny' || a.cancelled) await streamText(c, 'Understood, I will not run it. ', cancelled);
        else await tool(c, 'Bash', { command: 'npm run build' }, 'built in 2.1s\n', cancelled);
      } else {
        await tool(c, 'Bash', { command: 'ls -la' }, 'README.md\npackage.json\nsrc\n', cancelled, 1500);
      }
      await tool(c, 'Read', { file_path: 'src/main.ts' }, 'export {}\n', cancelled);
      await tool(c, 'Edit', { file_path: 'src/main.ts', old_string: 'export {}', new_string: 'export const x = 1;\nexport const y = 2;' }, 'ok', cancelled);
      if (/question/i.test(text)) {
        const questions = ['Which colour?', 'Which size?'];
        const out: string[] = [];
        for (let s = 0; s < questions.length; ) {
          const a = await ask(c, { method: 'select', attention: 'question', title: questions[s], options: s === 0 ? ['Red', 'Blue'] : ['Small', 'Large'], allowOther: true, step: s, steps: questions.length, prefill: out[s] });
          if (typeof a.goto === 'number') {
            s = a.goto;
            continue;
          }
          if (a.cancelled) break;
          out[s] = String(a.value);
          s++;
        }
        await streamText(c, `You picked ${out.join(' and ') || 'nothing'}. `, cancelled);
      }
      if (/plan/i.test(text)) {
        const a = await ask(c, { method: 'confirm', attention: 'plan', title: 'Approve plan?', message: '1. Read the code\n2. Change it\n3. Test it', options: ['Approve plan', 'Keep planning'] });
        await streamText(c, a.confirmed || a.value === 'Approve plan' ? 'Plan approved, starting. ' : 'Keeping on planning. ', cancelled);
      }
      await streamText(c, 'All done: the folder has a README, a package file and a src folder, and **main.ts** now exports two constants.', cancelled);
      setStatus(c, 'idle');
      complete(c);
      startNext(c);
    } catch {
      setStatus(c, 'stopped');
    } finally {
      c.turn = null;
      if (c.thread.interactions.length) {
        c.thread = { ...c.thread, interactions: [], liveTools: [] };
        emitThread(c);
      }
    }
  }

  function startNext(c: FakeChat) {
    if (c.thread.queueHeld) return;
    const next = c.pending.find((s) => s.state === 'queued');
    if (!next) return;
    c.pending = c.pending.filter((s) => s !== next);
    emitQueue(c);
    addRow(c, { role: 'user', text: next.text, sourceKey: next.id, details: { submissionId: next.id } });
    void runTurn(c, next.text);
  }

  /** A first cut of the host's attention summary: running chats and pending interactions. */
  function attention() {
    const all = [...chats.values()].filter((c) => !c.chat.archived && !c.chat.sideSourceId);
    const list = all.filter((c) => !c.chat.muted);
    const row = (c: FakeChat, kind: string) => ({ chatId: c.chat.id, title: c.chat.title || 'Untitled', project: folderName(c.chat.cwd), kind, reason: kind });
    const busy = list.filter((c) => ['starting', 'running', 'stopping'].includes(c.chat.status));
    const needs = list.filter((c) => c.thread.interactions.some((i) => i.attention));
    const unread = list.filter((c) => c.chat.unread.markToken || (c.chat.unread.completionId && c.chat.status === 'idle' && !c.chat.error));
    return {
      badge: new Set([...needs, ...unread]).size,
      busy: all.filter((c) => ['starting', 'running', 'stopping'].includes(c.chat.status)).length,
      needsYou: needs.slice(0, 8).map((c) => {
        const a = c.thread.interactions.find((i) => i.attention)?.attention;
        return row(c, a === 'trust' ? 'approval' : String(a));
      }),
      running: busy.slice(0, 8).map((c) => row(c, 'running')),
      unread: unread.slice(0, 8).map((c) => row(c, 'unread')),
      stalled: [],
      revision,
    };
  }

  function find(chatId: unknown): FakeChat {
    const c = chats.get(String(chatId));
    if (!c) throw { code: 'not_found', message: 'No such chat.' };
    return c;
  }

  function page(c: FakeChat, a: Record<string, unknown>): TranscriptPage {
    let end = c.rows.length;
    if (typeof a.before === 'string') end = Math.max(0, c.rows.findIndex((r) => String(r.id) === a.before));
    const start = Math.max(0, end - PAGE);
    return {
      chatId: c.chat.id,
      messages: c.rows.slice(start, end),
      hasOlder: start > 0,
      hasNewer: end < c.rows.length,
      olderCursor: start > 0 ? String(c.rows[start]!.id) : null,
      newerCursor: null,
      runs: [],
      pending: c.pending,
      revision,
    };
  }

  if (opts.seedRows) {
    const c = newChat('C:/Users/me/Documents/Coding/demo');
    c.chat.title = 'Seeded chat';
    seed(c, opts.seedRows);
  }

  const ops: Record<string, (a: Record<string, unknown>) => unknown> = {
    app_info: () => ({ name: 'Helm', edition: 'desktop', version: 'dev', schemaVersion: 1, profileDir: '(fake host)' }),
    quit_request: () => null,
    chat_create: (a) => newChat(String(a.cwd)).chat,
    chat_list: () => [...chats.values()].map((c) => c.chat).filter((c) => !c.archived && !c.sideSourceId).sort((x, y) => y.createdAt - x.createdAt),
    chat_get: (a) => {
      const c = find(a.chatId);
      return { chat: c.chat, thread: c.thread };
    },
    transcript_page: (a) => page(find(a.chatId), a),
    chat_send: (a) => {
      const c = find(a.chatId);
      const text = String(a.text ?? '');
      if (!text.trim()) throw { code: 'refused', message: 'Nothing to send.' };
      const busy = c.chat.status === 'running' || c.chat.status === 'starting';
      const action = String(a.action);
      const mode = action === 'default' ? sendBehavior.mode : action;
      const s: Submission = {
        id: String(a.submissionId),
        chatId: c.chat.id,
        text,
        mode: 'prompt',
        state: 'accepted',
        origin: a.origin === 'action' || a.origin === 'notification' ? a.origin : 'typed',
        position: null,
        createdAt: now(),
      };
      if (c.chat.title === 'New chat') c.chat = { ...c.chat, title: text.slice(0, 40) };
      if (!busy) {
        addRow(c, { role: 'user', text, sourceKey: s.id, details: { submissionId: s.id } });
        void runTurn(c, text);
      } else if (mode === 'asap' || mode === 'steer') {
        s.mode = 'steer';
        c.pending = [...c.pending, s];
        emitQueue(c);
      } else if (mode === 'queue') {
        s.mode = 'followUp';
        s.state = 'queued';
        s.position = c.pending.length;
        c.pending = [...c.pending, s];
        emitQueue(c);
      } else {
        s.state = 'queued';
        s.mode = 'followUp';
        s.position = -1;
        c.pending = [s, ...c.pending];
        emitQueue(c);
        c.turn?.cancel();
        setTimeout(() => startNext(c), step * 3);
      }
      // Only the composer's sends clear the draft.
      if ((a.origin ?? 'composer') === 'composer') c.chat = { ...c.chat, draft: '' };
      emitChat(c);
      return { submission: s, chat: c.chat };
    },
    chat_stop: (a) => {
      const c = find(a.chatId);
      if (!c.turn) return { status: c.chat.status };
      setStatus(c, 'stopping');
      c.turn.cancel();
      if (a.continueQueue) {
        setTimeout(() => startNext(c), step * 3);
      } else {
        c.pending = c.pending.map((s) => ({ ...s, state: 'uncertain' as const }));
        emitQueue(c);
      }
      return { status: 'stopping' };
    },
    queue_resume: (a) => {
      const c = find(a.chatId);
      c.thread = { ...c.thread, queueHeld: false };
      emitThread(c);
      if (c.chat.status !== 'running') startNext(c);
      return null;
    },
    submission_action: (a) => {
      for (const c of chats.values()) {
        const s = c.pending.find((x) => x.id === a.submissionId);
        if (!s) continue;
        let out: Submission = s;
        if (a.action === 'withdraw' || a.action === 'recover') {
          out = { ...s, state: a.action === 'withdraw' ? 'withdrawn' : 'superseded' };
          c.pending = c.pending.filter((x) => x.id !== s.id);
          c.chat = { ...c.chat, draft: s.text, draftRevision: Math.max(now(), c.chat.draftRevision + 1) };
          emitChat(c);
        } else if (a.action === 'steerNow') {
          out = { ...s, mode: 'steer', state: 'accepted' };
          c.pending = c.pending.map((x) => (x.id === s.id ? out : x));
        } else if (a.action === 'sendNow') {
          c.pending = [{ ...s, position: -1 }, ...c.pending.filter((x) => x.id !== s.id)];
          c.turn?.cancel();
          setTimeout(() => startNext(c), step * 3);
        } else if (a.action === 'move') {
          const pos = Number(a.position);
          c.pending = c.pending.map((x) =>
            x.id === s.id ? { ...x, position: pos } : x.position === pos ? { ...x, position: s.position } : x,
          );
        }
        emitQueue(c);
        return { submission: out };
      }
      throw { code: 'not_found', message: 'No such submission.' };
    },
    interaction_answer: (a) => {
      answers.get(String(a.requestId))?.(a);
      answers.delete(String(a.requestId));
      return null;
    },
    draft_save: (a) => {
      const c = find(a.chatId);
      const r = Number(a.revision);
      if (r <= c.chat.draftRevision) throw { code: 'stale', message: 'A newer draft is saved.' };
      c.chat = { ...c.chat, draft: String(a.text), draftRevision: r, revision: rev() };
      return { revision: r };
    },
    chat_set_model: (a) => {
      const c = find(a.chatId);
      c.chat = { ...c.chat, model: (a.model as string) ?? c.chat.model, effort: (a.effort as string) ?? c.chat.effort };
      emitChat(c);
      addRow(c, { role: 'context', text: `Model set to ${c.chat.model} · ${c.chat.effort}`, details: { contextKind: 'modelChange' } });
      return c.chat;
    },
    model_catalogue: () => ({
      harness: 'claude',
      models: [
        { id: 'claude-opus-5-5', name: 'Opus 5.5', description: 'Most capable' },
        { id: 'claude-sonnet-5', name: 'Sonnet 5', description: 'Balanced' },
        { id: 'claude-haiku-5', name: 'Haiku 5', description: 'Fastest' },
      ],
      efforts: ['low', 'medium', 'high', 'xhigh', 'max'],
      defaultEffort: 'xhigh',
      source: 'fallback',
    }),
    tool_stop: () => ({ stopped: true }),
    attention_get: () => attention(),
    chat_archive: (a) => {
      const c = find(a.chatId);
      if (['starting', 'running', 'stopping'].includes(c.chat.status)) throw { code: 'refused', message: 'This chat is working; stop the task first, then archive it.' };
      c.chat = { ...c.chat, archived: true };
      emitChat(c);
      emitAttention();
      return c.chat;
    },
    chat_restore: (a) => {
      const c = find(a.chatId);
      c.chat = { ...c.chat, archived: false };
      emitChat(c);
      emitAttention();
      return c.chat;
    },
    chat_delete: (a) => {
      const c = find(a.chatId);
      if (!c.chat.archived) throw { code: 'refused', message: 'Only archived chats can be deleted; archive it first.' };
      chats.delete(c.chat.id);
      emit({ kind: 'chat_removed', chatId: c.chat.id });
      emitAttention();
      return null;
    },
    chat_mute: (a) => {
      const c = find(a.chatId);
      c.chat = { ...c.chat, muted: Boolean(a.muted) };
      emitChat(c);
      emitAttention();
      return c.chat;
    },
    attention_address: () => null,
    autocomplete_suggest: (a) => {
      const words = ['component', 'composer', 'completion', 'transcript', 'thinking'];
      const last = String(a.draft ?? '').split(/\s/).pop() ?? '';
      const hit = last.length >= 2 ? words.find((w) => w.startsWith(last) && w !== last) : undefined;
      return { completion: hit ? hit.slice(last.length) : '', retryAfterMs: null };
    },
    side_chat_resolve: (a) => {
      const c = find(a.chatId);
      if (a.action === 'discard') {
        chats.delete(c.chat.id);
        emit({ kind: 'chat_removed', chatId: c.chat.id });
        return null;
      }
      c.chat = { ...c.chat, sideSourceId: null };
      emitChat(c);
      return c.chat;
    },
    attachment_put: () => ({ attachmentId: uid() }),
    skills_list: () => [
      { name: 'review', description: 'Review the current changes' },
      { name: 'compact', description: 'Compact the conversation' },
    ],
    attention_snooze: () => null,
    unread_mark: (a) => {
      const c = find(a.chatId);
      c.chat = { ...c.chat, unread: { ...c.chat.unread, markToken: uid() } };
      emitChat(c);
      emitAttention();
      return c.chat;
    },
    unread_clear: (a) => {
      const c = find(a.chatId);
      if (c.chat.unread.markToken === a.token) c.chat = { ...c.chat, unread: { ...c.chat.unread, markToken: null } };
      emitChat(c);
      emitAttention();
      return c.chat;
    },
    completion_ack: (a) => {
      const c = find(a.chatId);
      if (c.chat.unread.completionId === a.completionId) c.chat = { ...c.chat, unread: { ...c.chat.unread, completionId: null } };
      emitChat(c);
      emitAttention();
      return c.chat;
    },
    chat_compact: () => null,
    preference_get: (a) => (a.key === 'sendBehavior' ? sendBehavior : (prefs.get(String(a.key)) ?? null)),
    preference: (a) => {
      if (a.key === 'sendBehavior') sendBehavior = { ...HOME_SEND_BEHAVIOR, ...(a.value as object) };
      else prefs.set(String(a.key), a.value);
      for (const c of chats.values()) {
        c.thread = { ...c.thread, sendBehavior };
        emitThread(c);
      }
      return null;
    },
  };

  return async (command, payload) => {
    if (command !== 'api') throw { code: 'refused', message: `Unknown command ${command}` };
    const op = String(payload.op);
    const args = (payload.args as Record<string, unknown>) ?? {};
    const f = ops[op];
    await new Promise((r) => setTimeout(r, 5));
    if (f) return f(args);
    const extra = fakeOpFor(op);
    if (extra) return extra(op, args, { emit, call: (o, a = {}) => ops[o]?.(a) });
    throw { code: 'not_implemented', message: `The fake host has no op ${op}.` };
  };
}
