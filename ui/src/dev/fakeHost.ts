// A fake host for checking the UI in a plain browser. It implements the host's ops and
// events with a scripted turn: streamed text, a tool card, and an approval, question or plan
// card when the prompt asks for one ("approve", "question", "plan"). "todo" adds a TodoWrite checklist and "code" streams a fenced code block. "options" ends the reply with a numbered list of choices. "long" in a prompt adds a
// background task. A steer sent mid-turn shows as Pending and is read about 1.5 s later.
//
// Dev builds only: main.tsx imports it behind `import.meta.env.DEV`, so production drops it.

import type { Invoke } from '../api/client.ts';
import type {
  ChatRecord,
  Interaction,
  MessageDetails,
  MessageRow,
  SendBehavior,
  Submission,
  ThreadState,
  TranscriptPage,
} from '../api/types.ts';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';
import { fakeThumb } from './fakeThumb.ts';
import { leafFakeOps } from './fake-leaf.ts';
import { projectsFakeOps } from './fake-projects.ts';
import { boardFakeOps } from './fake-board.ts';
import { panelFakeOps } from './fake-panel.ts';
import { shellFakeOps } from './fake-shell.ts';
import { mcpFakeOps } from './fake-mcp.ts';
import { hosttoolsFakeOps } from './fake-hosttools.ts';
import { usageFakeOps } from './fake-usage.ts';
import { updatesFakeOps } from './fake-updates.ts';
import { itemLinksFakeOps } from './fake-itemlinks.ts';
import { hubFakeOps } from './fake-hub.ts';
import { githubFakeOps } from './fake-github.ts';
import { holdForSlot, isSlotWaitPrompt, slotsFakeOps } from './fake-slots.ts';
import { orchestratorFakeOps } from './fake-orchestrator.ts';
import { newChatFakeOps } from '../newchat/fake.ts';
import { runFakeSubagents } from '../transcript/subagents/fake.ts';

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
{ const bd = boardFakeOps(); registerFakeOps(bd.prefixes, bd.handler); } // before the projects fake, whose board_ prefix would catch these
{ const sb = projectsFakeOps(); registerFakeOps(sb.prefixes, sb.handler); }
{ const panel = panelFakeOps(); registerFakeOps(panel.prefixes, panel.handler); }
{ const sh = shellFakeOps(); registerFakeOps(sh.prefixes, sh.handler); }
{ const mcp = mcpFakeOps(); registerFakeOps(mcp.prefixes, mcp.handler); }
{ const ht = hosttoolsFakeOps(); registerFakeOps(ht.prefixes, ht.handler); }
{ const us = usageFakeOps(); registerFakeOps(us.prefixes, us.handler); }
{ const up = updatesFakeOps(); registerFakeOps(up.prefixes, up.handler); }
{ const il = itemLinksFakeOps(); registerFakeOps(il.prefixes, il.handler); }
{ const oc = orchestratorFakeOps(); registerFakeOps(oc.prefixes, oc.handler); }
{ const nc = newChatFakeOps(); registerFakeOps(nc.prefixes, nc.handler); }
{ const hub = hubFakeOps(); registerFakeOps(hub.prefixes, hub.handler); }
{ const sl = slotsFakeOps(); registerFakeOps(sl.prefixes, sl.handler); }
{ const gh = githubFakeOps(); registerFakeOps(gh.prefixes, gh.handler); }

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
  /** Chats the idle check has flagged, with its reason. A prompt containing "flag" leaves one after its turn; new activity, snooze or mark complete clears it. */
  const flags = new Map<string, string>();
  let sendBehavior: SendBehavior = { ...HOME_SEND_BEHAVIOR };
  const prefs = new Map<string, unknown>();
  const answers = new Map<string, (a: Record<string, unknown>) => void>();

  const rev = () => ++revision;
  const now = () => Date.now();
  const uid = () => (globalThis.crypto?.randomUUID?.() ?? `id-${Math.random().toString(16).slice(2)}`);
  /** Images put by the composer, kept so a sent message's thumbnail can be read back. */
  const attachments = new Map<string, { name: string; mime: string; bytes: string }>();
  /** References made by the composer, kept so the sent message's bubble can show them. */
  /** The images and references of a send, for the row written when its turn starts. */
  const sentExtras = new Map<string, Pick<MessageDetails, 'images' | 'refs'>>();
  const sentRefList = new Map<string, { kind: string; name: string; target: string }>();

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
        subagents: [],
        subagentsRunning: 0,
        compacting: null,
        capabilities: { withdraw: true, deliverNow: true, stopTool: true, compact: true, stopSubagent: true },
        sendBehavior,
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

  // "[stream]" in a prompt adds a long Markdown reply (about 9 KB) streamed the way the
  // host sends it, one snapshot every step, for checking streaming cost (checks/perf).
  async function streamLong(c: FakeChat, cancelled: () => boolean, chunks = 400) {
    let r = addRow(c, { role: 'assistant', status: 'streaming', thinking: '' });
    let text = '';
    for (let i = 0; i < chunks; ) {
      for (let k = 0; k < 3 && i < chunks; k++, i++) text += i % 12 === 11 ? '\n\n' : i % 40 === 20 ? '\n\n```js\nconst x = 1;\n```\n\n' : `word${i} **bold** and \`c${i}\` `;
      await sleep(step, cancelled);
      r = updateRow(c, r, { text });
    }
    updateRow(c, r, { status: 'complete' });
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
    flags.delete(c.chat.id);
    if (/flag/i.test(text)) flags.set(c.chat.id, `You asked for "${text.slice(0, 40)}" and the last reply stopped before the tests were run. It looks unfinished: nobody has confirmed the change works.`);
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
      if (/todo/i.test(text)) {
        const todos = [
          { content: 'Read the code', status: 'completed', activeForm: 'Reading the code' },
          { content: 'Change it', status: 'in_progress', activeForm: 'Changing it' },
          { content: 'Test it', status: 'pending', activeForm: 'Testing it' },
        ];
        await tool(c, 'TodoWrite', { todos }, 'Todos updated', cancelled);
      }
      if (/code/i.test(text)) await streamText(c, 'Here is the change:\n\n```ts\nexport const x = 1;\nexport const y = 2;\n```\n\n', cancelled);
      if (/subagent/i.test(text)) runFakeSubagents(c, { addRow, updateRow, emitThread, uid });
      if (/question/i.test(text)) {
        const questions = ['Which colour?', 'Which size?'];
        const out: string[] = [];
        for (let s = 0; s < questions.length; ) {
          const a = await ask(c, { method: 'select', attention: 'question', title: questions[s], options: s === 0 ? ['Red', 'Blue (Recommended)'] : ['Small', 'Large'], optionDescriptions: s === 0 ? ['Warm and loud', 'Calm, and what the rest of the app uses'] : undefined, allowOther: true, step: s, steps: questions.length, prefill: out[s] });
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
      if (/\[stream\]/i.test(text)) await streamLong(c, cancelled); // the long reply is the final one
      else if (/options/i.test(text)) {
        await streamText(
          c,
          'The folder has a README, a package file and a src folder. There are three ways to carry on:\n\n1. **Split the stylesheet by area**: smaller files, easier to review, but a wider diff now (Recommended)\n2. Move the shared colours and spacing into one tokens file\n3. Leave the styles as they are and only fix the two broken screens\n\nSay if you want me to start with the first item.',
          cancelled,
        );
      } else await streamText(c, 'All done: the folder has a README, a package file and a src folder, and **main.ts** now exports two constants.', cancelled);
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
    const next = c.pending.find((s) => s.state === 'queued');
    if (!next) return;
    c.pending = c.pending.filter((s) => s !== next);
    emitQueue(c);
    addRow(c, { role: 'user', text: next.text, sourceKey: next.id, details: { submissionId: next.id, ...sentExtras.get(next.id) } });
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
      flagged: list
        .filter((c) => flags.has(c.chat.id) && !['starting', 'running', 'stopping'].includes(c.chat.status) && !needs.includes(c) && !unread.includes(c))
        .slice(0, 8)
        .map((c) => ({ ...row(c, 'triage'), reason: flags.get(c.chat.id) ?? '' })),
      revision,
    };
  }

  // Settings → Runtime's stored settings.
  const runtimeSettings: Record<string, unknown> = { executable: '', skipPermissions: true, fastMode: true, worktreeTools: false, scheduleTools: false, shellChanges: true, subagentLimits: 'orchestrator', subagentHardStop: false, subagentSoftTurns: 100, toolOutputCap: true, model: null, effort: 'xhigh', responseVerbosity: 'low', problem: null };

  // Settings → Runtime's repositories.
  const repos: { repoId: string; remoteUrl?: string; worktreesAllowed: boolean; locations: { id: string; path: string }[] }[] = [
    { repoId: 'github.com/me/demo', remoteUrl: 'https://github.com/me/demo', worktreesAllowed: true, locations: [{ id: 'l1', path: 'C:/Users/me/Documents/Coding/demo' }] },
  ];

  function find(chatId: unknown): FakeChat {
    const c = chats.get(String(chatId));
    if (!c) throw { code: 'not_found', message: 'No such chat.' };
    return c;
  }

  function page(c: FakeChat, a: Record<string, unknown>): TranscriptPage {
    let end = c.rows.length;
    if (typeof a.before === 'string') end = Math.max(0, c.rows.findIndex((r) => String(r.id) === a.before));
    if (typeof a.around === 'number') {
      const at = c.rows.findIndex((r) => r.id === a.around);
      if (at >= 0) end = Math.min(c.rows.length, at + 1 + Math.floor(PAGE / 2));
    }
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
    chat_find: (a) => {
      const q = String(a.query ?? '').toLowerCase();
      const rows = q.trim()
        ? find(a.chatId).rows.flatMap((r) => {
            const text = (r.role === 'user' || r.role === 'assistant' || a.includeActivity ? r.text : '').toLowerCase();
            let count = 0;
            for (let i = text.indexOf(q); i >= 0; i = text.indexOf(q, i + q.length)) count++;
            return count ? [{ id: r.id, count }] : [];
          })
        : [];
      return { rows, nextCursor: null };
    },
    chat_send: (a) => {
      const c = find(a.chatId);
      const text = String(a.text ?? '');
      const sentImages = ((a.attachments as string[] | undefined) ?? []).flatMap((id) => {
        const img = attachments.get(id);
        return img ? [{ id, name: img.name, mime: img.mime, width: 160, height: 120 }] : [];
      });
      const sentRefs = ((a.attachments as string[] | undefined) ?? []).flatMap((id) => sentRefList.get(id) ?? []);
      if (!text.trim() && sentImages.length === 0 && sentRefs.length === 0) throw { code: 'refused', message: 'Nothing to send.' };
      const busy = c.chat.status === 'running' || c.chat.status === 'starting';
      sentExtras.set(String(a.submissionId), { ...(sentImages.length ? { images: sentImages } : {}), ...(sentRefs.length ? { refs: sentRefs } : {}) });
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
      if (!busy && isSlotWaitPrompt(text)) {
        // A send held for a free run slot: saved, not yet delivered.
        s.state = 'saved';
        c.pending = [...c.pending, s];
        emitQueue(c);
        holdForSlot(c.chat.id, s.id);
      } else if (!busy) {
        addRow(c, { role: 'user', text, sourceKey: s.id, details: { submissionId: s.id, ...(sentImages.length ? { images: sentImages } : {}), ...(sentRefs.length ? { refs: sentRefs } : {}) } });
        void runTurn(c, text);
      } else if (mode === 'asap' || mode === 'steer') {
        s.mode = 'steer';
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
    // Dev-only: the slot queue's Start now (run the held send) and Cancel (mark it not delivered).
    dev_release_wait: (a) => {
      const c = find(a.chatId);
      const s = c.pending.find((x) => x.id === a.submissionId);
      if (!s) return null;
      c.pending = c.pending.filter((x) => x.id !== s.id);
      if (a.start) {
        emitQueue(c);
        addRow(c, { role: 'user', text: s.text, sourceKey: s.id, details: { submissionId: s.id } });
        void runTurn(c, s.text);
      } else {
        c.pending = [{ ...s, state: 'rejected' as const }, ...c.pending];
        emitQueue(c);
      }
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
    // the permission-mode pick lands as the live mode, as a worker would report it.
    chat_set_permission_mode: (a) => {
      const c = find(a.chatId);
      c.thread = { ...c.thread, permissionMode: String(a.mode) };
      emitThread(c);
      return { mode: String(a.mode) };
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
    // Settings → Runtime.
    runtime_status: () => ({
      node: { path: 'C:/Program Files/nodejs/node.exe', source: 'PATH', version: 'v24.14.0', problem: null },
      claude: { path: 'C:/Users/me/.local/bin/claude.exe', source: 'local-bin', version: '2.1.301', problem: null },
      problems: [],
    }),
    runtime_settings_get: () => ({ ...runtimeSettings }),
    runtime_settings_save: (a) => {
      Object.assign(runtimeSettings, a, 'model' in a ? { model: a.model ? String(a.model) : null } : {});
      return { ...runtimeSettings };
    },
    repo_list: () => ({ entries: repos }),
    repo_set_worktrees: (a) => {
      const r = repos.find((x) => x.repoId === a.repoId);
      if (!r) throw { code: 'not_found', message: 'There is no repository with that id.' };
      r.worktreesAllowed = a.allowed !== false;
      return { ...r };
    },
    attention_get: () => attention(),
    chat_archive: (a) => {
      const c = find(a.chatId);
      // A working chat is archived at once; the host stops its turn behind the scenes.
      c.chat = { ...c.chat, archived: true, ...(['starting', 'running', 'stopping'].includes(c.chat.status) ? { status: 'stopped' as const } : {}) };
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
    attention_address: (a) => {
      flags.delete(String(a.chatId));
      emitAttention();
      return null;
    },
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
    // Save full output: the dev host pretends to write the file.
    message_output_save: () => ({ path: 'C:/Users/me/Downloads/output.txt' }),
    // A picture on disk: the dev host answers a small PNG for any image path.
    local_image_get: () => ({
      mime: 'image/png',
      bytes: 'iVBORw0KGgoAAAANSUhEUgAAAGQAAAAyCAIAAAAlV+npAAAnSklEQVR4nA3Q8Uc7DQDA4YmJiYmJIyMjI0ecODFxYmLkyMiRIyZOTJw4MTIyMjJivkycODFxPjly5MiIExMTEyeOHBkZGf3wvs+f8KRSKeZSpFPMp8ikWEiRTbGYIpdiKYWQYjlFPsVKikKK1RTFFGspxBTrKaQUGynkFJspSim2UigptlOUU+ykqKTYTaGm2EtRTbGfQktxkEJPcZiiluIohZHiOEU9xUkKM8VpCivFWYpGivMUzRQXKVopLlO0U1yl6KS4TtFN8S9FL8VNCjvFbQonxV2Kfor7FG6KhxReiscUfoqnFEGK5xSDFC8pwhSvKYYp3lKMUrynGKf4SBGl+EwRp/hKkaT4TjFJ8ZNimuI3xSzFX4pUKs1cmnSa+TSZNAtpsmkW0+TSLKUR0iynyadZSVNIs5qmmGYtjZhmPY2UZiONnGYzTSnNVholzXaacpqdNJU0u2nUNHtpqmn202hpDtLoaQ7T1NIcpTHSHKeppzlJY6Y5TWOlOUvTSHOeppnmIk0rzWWadpqrNJ0012m6af6l6aW5SWOnuU3jpLlL009zn8ZN85DGS/OYxk/zlCZI85xmkOYlTZjmNc0wzVuaUZr3NOM0H2miNJ9p4jRfaZI032kmaX7STNP8ppml+Uv/n5VhLkM6w3yGTIaFDNkMixlyGZYyCBmWM+QzrGQoZFjNUMywlkHMsJ5ByrCRQc6wmaGUYSuDkmE7QznDToZKht0Maoa9DNUM+xm0DAcZ9AyHGWoZjjIYGY4z1DOcZDAznGawMpxlaGQ4z9DMcJGhleEyQzvDVYZOhusM3Qz/MvQy3GSwM9xmcDLcZehnuM/gZnjI4GV4zOBneMoQZHjOMMjwkiHM8JphmOEtwyjDe4Zxho8MUYbPDHGGrwxJhu8Mkww/GaYZfjPMMvxl/s/KMpclnWU+SybLQpZslsUsuSxLWYQsy1nyWVayFLKsZilmWcsiZlnPImXZyCJn2cxSyrKVRcmynaWcZSdLJctuFjXLXpZqlv0sWpaDLHqWwyy1LEdZjCzHWepZTrKYWU6zWFnOsjSynGdpZrnI0spymaWd5SpLJ8t1lm6Wf1l6WW6y2FluszhZ7rL0s9xncbM8ZPGyPGbxszxlCbI8ZxlkeckSZnnNMszylmWU5T3LOMtHlijLZ5Y4y1eWJMt3lkmWnyzTLL9ZZln+sv9n5ZjLkc4xnyOTYyFHNsdijlyOpRxCjuUc+RwrOQo5VnMUc6zlEHOs55BybOSQc2zmKOXYyqHk2M5RzrGTo5JjN4eaYy9HNcd+Di3HQQ49x2GOWo6jHEaO4xz1HCc5zBynOawcZzkaOc5zNHNc5GjluMzRznGVo5PjOkc3x78cvRw3OewctzmcHHc5+jnuc7g5HnJ4OR5z+DmecgQ5nnMMcrzkCHO85hjmeMsxyvGeY5zjI0eU4zNHnOMrR5LjO8ckx0+OaY7fHLMcf7n/swTmBNIC8wIZgQWBrMCiQE5gSUAQWBbIC6wIFARWBYoCawKiwLqAJLAhIAtsCpQEtgQUgW2BssCOQEVgV0AV2BOoCuwLaAIHArrAoUBN4EjAEDgWqAucCJgCpwKWwJlAQ+BcoClwIdASuBRoC1wJdASuBboC/wR6AjcCtsCtgCNwJ9AXuBdwBR4EPIFHAV/gSSAQeBYYCLwIhAKvAkOBN4GRwLvAWOBDIBL4FIgFvgQSgW+BicCPwFTgV2Am8Cf8n5VnLk86z3yeTJ6FPNk8i3lyeZbyCHmW8+TzrOQp5FnNU8yzlkfMs55HyrORR86zmaeUZyuPkmc7TznPTp5Knt08ap69PNU8+3m0PAd59DyHeWp5jvIYeY7z1POc5DHznOax8pzlaeQ5z9PMc5GnlecyTzvPVZ5Onus83Tz/8vTy3OSx89zmcfLc5ennuc/j5nnI4+V5zOPnecoT5HnOM8jzkifM85pnmOctzyjPe55xno88UZ7PPHGerzxJnu88kzw/eaZ5fvPM8vzl/88qMFcgXWC+QKbAQoFsgcUCuQJLBYQCywXyBVYKFAqsFigWWCsgFlgvIBXYKCAX2CxQKrBVQCmwXaBcYKdApcBuAbXAXoFqgf0CWoGDAnqBwwK1AkcFjALHBeoFTgqYBU4LWAXOCjQKnBdoFrgo0CpwWaBd4KpAp8B1gW6BfwV6BW4K2AVuCzgF7gr0C9wXcAs8FPAKPBbwCzwVCAo8FxgUeCkQFngtMCzwVmBU4L3AuMBHgajAZ4G4wFeBpMB3gUmBnwLTAr8FZgX+Cv9nFZkrki4yXyRTZKFItshikVyRpSJCkeUi+SIrRQpFVosUi6wVEYusF5GKbBSRi2wWKRXZKqIU2S5SLrJTpFJkt4haZK9Itch+Ea3IQRG9yGGRWpGjIkaR4yL1IidFzCKnRawiZ0UaRc6LNItcFGkVuSzSLnJVpFPkuki3yL8ivSI3Rewit0WcIndF+kXui7hFHop4RR6L+EWeigRFnosMirwUCYu8FhkWeSsyKvJeZFzko0hU5LNIXOSrSFLku8ikyE+RaZHfIrMif8X/s0TmRNIi8yIZkQWRrMiiSE5kSUQQWRbJi6yIFERWRYoiayKiyLqIJLIhIotsipREtkQUkW2RssiOSEVkV0QV2ROpiuyLaCIHIrrIoUhN5EjEEDkWqYuciJgipyKWyJlIQ+RcpClyIdISuRRpi1yJdESuRboi/0R6IjcitsitiCNyJ9IXuRdxRR5EPJFHEV/kSSQQeRYZiLyIhCKvIkORN5GRyLvIWORDJBL5FIlFvkQSkW+RiciPyFTkV2Qm8if+nyUxJ5GWmJfISCxIZCUWJXISSxKCxLJEXmJFoiCxKlGUWJMQJdYlJIkNCVliU6IksSWhSGxLlCV2JCoSuxKqxJ5EVWJfQpM4kNAlDiVqEkcShsSxRF3iRMKUOJWwJM4kGhLnEk2JC4mWxKVEW+JKoiNxLdGV+CfRk7iRsCVuJRyJO4m+xL2EK/Eg4Uk8SvgSTxKBxLPEQOJFIpR4lRhKvEmMJN4lxhIfEpHEp0Qs8SWRSHxLTCR+JKYSvxIziT/p/yyZOZm0zLxMRmZBJiuzKJOTWZIRZJZl8jIrMgWZVZmizJqMKLMuI8lsyMgymzIlmS0ZRWZbpiyzI1OR2ZVRZfZkqjL7MprMgYwucyhTkzmSMWSOZeoyJzKmzKmMJXMm05A5l2nKXMi0ZC5l2jJXMh2Za5muzD+ZnsyNjC1zK+PI3Mn0Ze5lXJkHGU/mUcaXeZIJZJ5lBjIvMqHMq8xQ5k1mJPMuM5b5kIlkPmVimS+ZROZbZiLzIzOV+ZWZyfzJ/2eVmCuRLjFfIlNioUS2xGKJXImlEkKJ5RL5EislCiVWSxRLrJUQS6yXkEpslJBLbJYoldgqoZTYLlEusVOiUmK3hFpir0S1xH4JrcRBCb3EYYlaiaMSRonjEvUSJyXMEqclrBJnJRolzks0S1yUaJW4LNEucVWiU+K6RLfEvxK9Ejcl7BK3JZwSdyX6Je5LuCUeSnglHkv4JZ5KBCWeSwxKvJQIS7yWGJZ4KzEq8V5iXOKjRFTis0Rc4qtEUuK7xKTET4lpid8SsxJ/pf+zFOYU0grzChmFBYWswqJCTmFJQVBYVsgrrCgUFFYVigprCqLCuoKksKEgK2wqlBS2FBSFbYWywo5CRWFXQVXYU6gq7CtoCgcKusKhQk3hSMFQOFaoK5womAqnCpbCmUJD4VyhqXCh0FK4VGgrXCl0FK4Vugr/FHoKNwq2wq2Co3Cn0Fe4V3AVHhQ8hUcFX+FJIVB4VhgovCiECq8KQ4U3hZHCu8JY4UMhUvhUiBW+FBKFb4WJwo/CVOFXYabwp/yfVWauTLrMfJlMmYUy2TKLZXJllsoIZZbL5MuslCmUWS1TLLNWRiyzXkYqs1FGLrNZplRmq4xSZrtMucxOmUqZ3TJqmb0y1TL7ZbQyB2X0ModlamWOyhhljsvUy5yUMcuclrHKnJVplDkv0yxzUaZV5rJMu8xVmU6Z6zLdMv/K9MrclLHL3JZxytyV6Ze5L+OWeSjjlXks45d5KhOUeS4zKPNSJizzWmZY5q3MqMx7mXGZjzJRmc8ycZmvMkmZ7zKTMj9lpmV+y8zK/JX/z6owVyFdYb5CpsJChWyFxQq5CksVhArLFfIVVioUKqxWKFZYqyBWWK8gVdioIFfYrFCqsFVBqbBdoVxhp0Klwm4FtcJehWqF/QpahYMKeoXDCrUKRxWMCscV6hVOKpgVTitYFc4qNCqcV2hWuKjQqnBZoV3hqkKnwnWFboV/FXoVbirYFW4rOBXuKvQr3FdwKzxU8Co8VvArPFUIKjxXGFR4qRBWeK0wrPBWYVThvcK4wkeFqMJnhbjCV4WkwneFSYWfCtMKvxVmFf4q/2epzKmkVeZVMioLKlmVRZWcypKKoLKskldZUSmorKoUVdZURJV1FUllQ0VW2VQpqWypKCrbKmWVHZWKyq6KqrKnUlXZV9FUDlR0lUOVmsqRiqFyrFJXOVExVU5VLJUzlYbKuUpT5UKlpXKp0la5UumoXKt0Vf6p9FRuVGyVWxVH5U6lr3Kv4qo8qHgqjyq+ypNKoPKsMlB5UQlVXlWGKm8qI5V3lbHKh0qk8qkSq3ypJCrfKhOVH5Wpyq/KTOVP/T+rylyVdJX5KpkqC1WyVRar5KosVRGqLFfJV1mpUqiyWqVYZa2KWGW9ilRlo4pcZbNKqcpWFaXKdpVylZ0qlSq7VdQqe1WqVfaraFUOquhVDqvUqhxVMaocV6lXOaliVjmtYlU5q9Kocl6lWeWiSqvKZZV2lasqnSrXVbpV/lXpVbmpYle5reJUuavSr3Jfxa3yUMWr8ljFr/JUJajyXGVQ5aVKWOW1yrDKW5VRlfcq4yofVaIqn1XiKl9VkirfVSZVfqpMq/xWmVX5q/6fpTGnkdaY18hoLGhkNRY1chpLGoLGskZeY0WjoLGqUdRY0xA11jUkjQ0NWWNTo6SxpaFobGuUNXY0Khq7GqrGnkZVY19D0zjQ0DUONWoaRxqGxrFGXeNEw9Q41bA0zjQaGucaTY0LjZbGpUZb40qjo3Gt0dX4p9HTuNGwNW41HI07jb7GvYar8aDhaTxq+BpPGoHGs8ZA40Uj1HjVGGq8aYw03jXGGh8akcanRqzxpZFofGtMNH40phq/GjONP+3/LJ05nbTOvE5GZ0Enq7Ook9NZ0hF0lnXyOis6BZ1VnaLOmo6os64j6WzoyDqbOiWdLR1FZ1unrLOjU9HZ1VF19nSqOvs6ms6Bjq5zqFPTOdIxdI516jonOqbOqY6lc6bT0DnXaepc6LR0LnXaOlc6HZ1rna7OP52ezo2OrXOr4+jc6fR17nVcnQcdT+dRx9d50gl0nnUGOi86oc6rzlDnTWek864z1vnQiXQ+dWKdL51E51tnovOjM9X51Znp/On/Z9WYq5GuMV8jU2OhRrbGYo1cjaUaQo3lGvkaKzUKNVZrFGus1RBrrNeQamzUkGts1ijV2Kqh1NiuUa6xU6NSY7eGWmOvRrXGfg2txkENvcZhjVqNoxpGjeMa9RonNcwapzWsGmc1GjXOazRrXNRo1bis0a5xVaNT47pGt8a/Gr0aNzXsGrc1nBp3Nfo17mu4NR5qeDUea/g1nmoENZ5rDGq81AhrvNYY1nirMarxXmNc46NGVOOzRlzjq0ZS47vGpMZPjWmN3xqzGn+1/7MM5gzSBvMGGYMFg6zBokHOYMlAMFg2yBusGBQMVg2KBmsGosG6gWSwYSAbbBqUDLYMFINtg7LBjkHFYNdANdgzqBrsG2gGBwa6waFBzeDIwDA4NqgbnBiYBqcGlsGZQcPg3KBpcGHQMrg0aBtcGXQMrg26Bv8MegY3BrbBrYFjcGfQN7g3cA0eDDyDRwPf4MkgMHg2GBi8GIQGrwZDgzeDkcG7wdjgwyAy+DSIDb4MEoNvg4nBj8HU4NdgZvBn/J9VZ65Ous58nUydhTrZOot1cnWW6gh1luvk66zUKdRZrVOss1ZHrLNeR6qzUUeus1mnVGerjlJnu065zk6dSp3dOmqdvTrVOvt1tDoHdfQ6h3VqdY7qGHWO69TrnNQx65zWseqc1WnUOa/TrHNRp1Xnsk67zlWdTp3rOt06/+r06tzUsevc1nHq3NXp17mv49Z5qOPVeazj13mqE9R5rjOo81InrPNaZ1jnrc6oznudcZ2POlGdzzpxna86SZ3vOpM6P3WmdX7rzOr81f/PMpkzSZvMm2RMFkyyJosmOZMlE8Fk2SRvsmJSMFk1KZqsmYgm6yaSyYaJbLJpUjLZMlFMtk3KJjsmFZNdE9Vkz6Rqsm+imRyY6CaHJjWTIxPD5NikbnJiYpqcmlgmZyYNk3OTpsmFScvk0qRtcmXSMbk26Zr8M+mZ3JjYJrcmjsmdSd/k3sQ1eTDxTB5NfJMnk8Dk2WRg8mISmryaDE3eTEYm7yZjkw+TyOTTJDb5MklMvk0mJj8mU5Nfk5nJn/l/lsWcRdpi3iJjsWCRtVi0yFksWQgWyxZ5ixWLgsWqRdFizUK0WLeQLDYsZItNi5LFloVisW1RttixqFjsWqgWexZVi30LzeLAQrc4tKhZHFkYFscWdYsTC9Pi1MKyOLNoWJxbNC0uLFoWlxZtiyuLjsW1Rdfin0XP4sbCtri1cCzuLPoW9xauxYOFZ/Fo4Vs8WQQWzxYDixeL0OLVYmjxZjGyeLcYW3xYRBafFrHFl0Vi8W0xsfixmFr8Wsws/qz/sxrMNUg3mG+QabDQINtgsUGuwVIDocFyg3yDlQaFBqsNig3WGogN1htIDTYayA02G5QabDVQGmw3KDfYaVBpsNtAbbDXoNpgv4HW4KCB3uCwQa3BUQOjwXGDeoOTBmaD0wZWg7MGjQbnDZoNLhq0Glw2aDe4atBpcN2g2+Bfg16DmwZ2g9sGToO7Bv0G9w3cBg8NvAaPDfwGTw2CBs8NBg1eGoQNXhsMG7w1GDV4bzBu8NEgavDZIG7w1SBp8N1g0uCnwbTBb4NZg7/G/1lN5pqkm8w3yTRZaJJtstgk12SpidBkuUm+yUqTQpPVJsUma03EJutNpCYbTeQmm01KTbaaKE22m5Sb7DSpNNltojbZa1Jtst9Ea3LQRG9y2KTW5KiJ0eS4Sb3JSROzyWkTq8lZk0aT8ybNJhdNWk0um7SbXDXpNLlu0m3yr0mvyU0Tu8ltE6fJXZN+k/smbpOHJl6TxyZ+k6cmQZPnJoMmL03CJq9Nhk3emoyavDcZN/loEjX5bBI3+WqSNPluMmny02Ta5LfJrMlf8/+sFnMt0i3mW2RaLLTItlhskWux1EJosdwi32KlRaHFaotii7UWYov1FlKLjRZyi80WpRZbLZQW2y3KLXZaVFrstlBb7LWotthvobU4aKG3OGxRa3HUwmhx3KLe4qSF2eK0hdXirEWjxXmLZouLFq0Wly3aLa5adFpct+i2+Nei1+Kmhd3itoXT4q5Fv8V9C7fFQwuvxWMLv8VTi6DFc4tBi5cWYYvXFsMWby1GLd5bjFt8tIhafLaIW3y1SFp8t5i0+GkxbfHbYtbir/V/Vpu5Nuk2820ybRbaZNsstsm1WWojtFluk2+z0qbQZrVNsc1aG7HNehupzUYbuc1mm1KbrTZKm+025TY7bSptdtuobfbaVNvst9HaHLTR2xy2qbU5amO0OW5Tb3PSxmxz2sZqc9am0ea8TbPNRZtWm8s27TZXbTptrtt02/xr02tz08Zuc9vGaXPXpt/mvo3b5qGN1+axjd/mqU3Q5rnNoM1Lm7DNa5thm7c2ozbvbcZtPtpEbT7bxG2+2iRtvttM2vy0mbb5bTNr89f+P6vDXId0h/kOmQ4LHbIdFjvkOix1EDosd8h3WOlQ6LDaodhhrYPYYb2D1GGjg9xhs0Opw1YHpcN2h3KHnQ6VDrsd1A57Haod9jtoHQ466B0OO9Q6HHUwOhx3qHc46WB2OO1gdTjr0Ohw3qHZ4aJDq8Nlh3aHqw6dDtcduh3+deh1uOlgd7jt4HS469DvcN/B7fDQwevw2MHv8NQh6PDcYdDhpUPY4bXDsMNbh1GH9w7jDh8dog6fHeIOXx2SDt8dJh1+Okw7/HaYdfjr/J/VZa5Lust8l0yXhS7ZLotdcl2Wughdlrvku6x0KXRZ7VLsstZF7LLeReqy0UXustml1GWri9Jlu0u5y06XSpfdLmqXvS7VLvtdtC4HXfQuh11qXY66GF2Ou9S7nHQxu5x2sbqcdWl0Oe/S7HLRpdXlsku7y1WXTpfrLt0u/7r0utx0sbvcdnG63HXpd7nv4nZ56OJ1eezid3nqEnR57jLo8tIl7PLaZdjlrcuoy3uXcZePLlGXzy5xl68uSZfvLpMuP12mXX67zLr8df/P6jHXI91jvkemx0KPbI/FHrkeSz2EHss98j1WehR6rPYo9ljrIfZY7yH12Ogh99jsUeqx1UPpsd2j3GOnR6XHbg+1x16Pao/9HlqPgx56j8MetR5HPYwexz3qPU56mD1Oe1g9zno0epz3aPa46NHqcdmj3eOqR6fHdY9uj389ej1uetg9bns4Pe569Hvc93B7PPTwejz28Hs89Qh6PPcY9HjpEfZ47THs8dZj1OO9x7jHR4+ox2ePuMdXj6THd49Jj58e0x6/PWY9/nr/Z9nM2aRt5m0yNgs2WZtFm5zNko1gs2yTt1mxKdis2hRt1mxEm3UbyWbDRrbZtCnZbNkoNts2ZZsdm4rNro1qs2dTtdm30WwObHSbQ5uazZGNYXNsU7c5sTFtTm0smzObhs25TdPmwqZlc2nTtrmy6dhc23Rt/tn0bG5sbJtbG8fmzqZvc2/j2jzYeDaPNr7Nk01g82wzsHmxCW1ebYY2bzYjm3ebsc2HTWTzaRPbfNkkNt82E5sfm6nNr83M5s/+P8thziHtMO+QcVhwyDosOuQclhwEh2WHvMOKQ8Fh1aHosOYgOqw7SA4bDrLDpkPJYctBcdh2KDvsOFQcdh1Uhz2HqsO+g+Zw4KA7HDrUHI4cDIdjh7rDiYPpcOpgOZw5NBzOHZoOFw4th0uHtsOVQ8fh2qHr8M+h53DjYDvcOjgOdw59h3sH1+HBwXN4dPAdnhwCh2eHgcOLQ+jw6jB0eHMYObw7jB0+HCKHT4fY4cshcfh2mDj8OEwdfh1mDn/O/1l95vqk+8z3yfRZ6JPts9gn12epj9BnuU++z0qfQp/VPsU+a33EPut9pD4bfeQ+m31Kfbb6KH22+5T77PSp9Nnto/bZ61Pts99H63PQR+9z2KfW56iP0ee4T73PSR+zz2kfq89Zn0af8z7NPhd9Wn0u+7T7XPXp9Lnu0+3zr0+vz00fu89tH6fPXZ9+n/s+bp+HPl6fxz5+n6c+QZ/nPoM+L33CPq99hn3e+oz6vPcZ9/noE/X57BP3+eqT9PnuM+nz02fa57fPrM9f//8slzmXtMu8S8ZlwSXrsuiSc1lyEVyWXfIuKy4Fl1WXosuai+iy7iK5bLjILpsuJZctF8Vl26XssuNScdl1UV32XKou+y6ay4GL7nLoUnM5cjFcjl3qLicupsupi+Vy5tJwOXdpuly4tFwuXdouVy4dl2uXrss/l57LjYvtcuviuNy59F3uXVyXBxfP5dHFd3lyCVyeXQYuLy6hy6vL0OXNZeTy7jJ2+XCJXD5dYpcvl8Tl22Xi8uMydfl1mbn8uf9necx5pD3mPTIeCx5Zj0WPnMeSh+Cx7JH3WPEoeKx6FD3WPESPdQ/JY8ND9tj0KHlseSge2x5ljx2Piseuh+qx51H12PfQPA48dI9Dj5rHkYfhcexR9zjxMD1OPSyPM4+Gx7lH0+PCo+Vx6dH2uPLoeFx7dD3+efQ8bjxsj1sPx+POo+9x7+F6PHh4Ho8evseTR+Dx7DHwePEIPV49hh5vHiOPd4+xx4dH5PHpEXt8eSQe3x4Tjx+Pqcevx8zjz/s/y2fOJ+0z75PxWfDJ+iz65HyWfASfZZ+8z4pPwWfVp+iz5iP6rPtIPhs+ss+mT8lny0fx2fYp++z4VHx2fVSfPZ+qz76P5nPgo/sc+tR8jnwMn2Ofus+Jj+lz6mP5nPk0fM59mj4XPi2fS5+2z5VPx+fap+vzz6fnc+Nj+9z6OD53Pn2fex/X58HH83n08X2efAKfZ5+Bz4tP6PPqM/R58xn5vPuMfT58Ip9Pn9jnyyfx+faZ+Pz4TH1+fWY+f/7/WQFzAemA+YBMwEJANmAxIBewFCAELAfkA1YCCgGrAcWAtQAxYD1ACtgIkAM2A0oBWwFKwHZAOWAnoBKwG6AG7AVUA/YDtICDAD3gMKAWcBRgBBwH1ANOAsyA0wAr4CygEXAe0Ay4CGgFXAa0A64COgHXAd2AfwG9gJsAO+A2wAm4C+gH3Ae4AQ8BXsBjgB/wFBAEPAcMAl4CwoDXgGHAW8Ao4D1gHPAREAV8BsQBXwFJwHfAJOAnYBrwGzAL+Av+zxowNyA9YH5AZsDCgOyAxQG5AUsDhAHLA/IDVgYUBqwOKA5YGyAOWB8gDdgYIA/YHFAasDVAGbA9oDxgZ0BlwO4AdcDegOqA/QHagIMB+oDDAbUBRwOMAccD6gNOBpgDTgdYA84GNAacD2gOuBjQGnA5oD3gakBnwPWA7oB/A3oDbgbYA24HOAPuBvQH3A9wBzwM8AY8DvAHPA0IBjwPGAx4GRAOeB0wHPA2YDTgfcB4wMeAaMDngHjA14BkwPeAyYCfAdMBvwNmA/4G/2eFzIWkQ+ZDMiELIdmQxZBcyFKIELIckg9ZCSmErIYUQ9ZCxJD1EClkI0QO2QwphWyFKCHbIeWQnZBKyG6IGrIXUg3ZD9FCDkL0kMOQWshRiBFyHFIPOQkxQ05DrJCzkEbIeUgz5CKkFXIZ0g65CumEXId0Q/6F9EJuQuyQ2xAn5C6kH3If4oY8hHghjyF+yFNIEPIcMgh5CQlDXkOGIW8ho5D3kHHIR0gU8hkSh3yFJCHfIZOQn5BpyG/ILOQv/D9ryNyQ9JD5IZkhC0OyQxaH5IYsDRGGLA/JD1kZUhiyOqQ4ZG2IOGR9iDRkY4g8ZHNIacjWEGXI9pDykJ0hlSG7Q9Qhe0OqQ/aHaEMOhuhDDofUhhwNMYYcD6kPORliDjkdYg05G9IYcj6kOeRiSGvI5ZD2kKshnSHXQ7pD/g3pDbkZYg+5HeIMuRvSH3I/xB3yMMQb8jjEH/I0JBjyPGQw5GVIOOR1yHDI25DRkPch4yEfQ6Ihn0PiIV9DkiHfQyZDfoZMh/wOmQ35G/6fNWJuRHrE/IjMiIUR2RGLI3IjlkYII5ZH5EesjCiMWB1RHLE2QhyxPkIasTFCHrE5ojRia4QyYntEecTOiMqI3RHqiL0R1RH7I7QRByP0EYcjaiOORhgjjkfUR5yMMEecjrBGnI1ojDgf0RxxMaI14nJEe8TViM6I6xHdEf9G9EbcjLBH3I5wRtyN6I+4H+GOeBjhjXgc4Y94GhGMeB4xGPEyIhzxOmI44m3EaMT7iPGIjxHRiM8R8YivEcmI7xGTET8jpiN+R8xG/I3+zxozNyY9Zn5MZszCmOyYxTG5MUtjhDHLY/JjVsYUxqyOKY5ZGyOOWR8jjdkYI4/ZHFMaszVGGbM9pjxmZ0xlzO4YdczemOqY/THamIMx+pjDMbUxR2OMMcdj6mNOxphjTsdYY87GNMacj2mOuRjTGnM5pj3makxnzPWY7ph/Y3pjbsbYY27HOGPuxvTH3I9xxzyM8cY8jvHHPI0JxjyPGYx5GROOeR0zHPM2ZjTmfcx4zMeYaMznmHjM15hkzPeYyZifMdMxv2NmY/7G/2dFzEWkI+YjMhELEdmIxYhcxFKEELEckY9YiShErEYUI9YixIj1CCliI0KO2IwoRWxFKBHbEeWInYhKxG6EGrEXUY3Yj9AiDiL0iMOIWsRRhBFxHFGPOIkwI04jrIiziEbEeUQz4iKiFXEZ0Y64iuhEXEd0I/5F9CJuIuyI2wgn4i6iH3Ef4UY8RHgRjxF+xFNEEPEcMYh4iQgjXiOGEW8Ro4j3iHHER0QU8RkRR3xFJBHfEZOIn4hpxG/ELOIv+j8rZi4mHTMfk4lZiMnGLMbkYpZihJjlmHzMSkwhZjWmGLMWI8asx0gxGzFyzGZMKWYrRonZjinH7MRUYnZj1Ji9mGrMfowWcxCjxxzG1GKOYoyY45h6zEmMGXMaY8WcxTRizmOaMRcxrZjLmHbMVUwn5jqmG/MvphdzE2PH3MY4MXcx/Zj7GDfmIcaLeYzxY55igpjnmEHMS0wY8xozjHmLGcW8x4xjPmKimM+YOOYrJon5jpnE/MRMY35jZjF/8f9ZCXMJ6YT5hEzCQkI2YTEhl7CUICQsJ+QTVhIKCasJxYS1BDFhPUFK2EiQEzYTSglbCUrCdkI5YSehkrCboCbsJVQT9hO0hIMEPeEwoZZwlGAkHCfUE04SzITTBCvhLKGRcJ7QTLhIaCVcJrQTrhI6CdcJ3YR/Cb2EmwQ74TbBSbhL6CfcJ7gJDwlewmOCn/CUECQ8JwwSXhLChNeEYcJbwijhPWGc8JEQJXwmxAlfCUnCd8Ik4SdhmvCbMEv4S/7PmjA3IT1hfkJmwsKE7ITFCbkJSxOECcsT8hNWJhQmrE4oTlibIE5YnyBN2JggT9icUJqwNUGZsD2hPGFnQmXC7gR1wt6E6oT9CdqEgwn6hMMJtQlHE4wJxxPqE04mmBNOJ1gTziY0JpxPaE64mNCacDmhPeFqQmfC9YTuhH8TehNuJtgTbic4E+4m9CfcT3AnPEzwJjxO8Cc8TQgmPE8YTHiZEE54nTCc8DZhNOF9wnjCx4RowueEeMLXhGTC94TJhJ8J0wm/E2YT/ib/Z02Zm5KeMj8lM2VhSnbK4pTclKUpwpTlKfkpK1MKU1anFKesTRGnrE+RpmxMkadsTilN2ZqiTNmeUp6yM6UyZXeKOmVvSnXK/hRtysEUfcrhlNqUoynGlOMp9SknU8wpp1OsKWdTGlPOpzSnXExpTbmc0p5yNaUz5XpKd8q/Kb0pN1PsKbdTnCl3U/pT7qe4Ux6meFMep/hTnqYEU56nDKa8TAmnvE4ZTnmbMpryPmU85WNKNOVzSjzla0oy5XvKZMrPlOmU3ymzKX/T/7NmzM1Iz5ifkZmxMCM7Y3FGbsbSDGHG8oz8jJUZhRmrM4oz1maIM9ZnSDM2ZsgzNmeUZmzNUGZszyjP2JlRmbE7Q52xN6M6Y3+GNuNghj7jcEZtxtEMY8bxjPqMkxnmjNMZ1oyzGY0Z5zOaMy5mtGZczmjPuJrRmXE9ozvj34zejJsZ9ozbGc6Muxn9Gfcz3BkPM7wZjzP8GU8zghnPMwYzXmaEM15nDGe8zRjNeJ8xnvExI5rxOSOe8TUjmfE9YzLjZ8Z0xu+M2Yy/Gf8BU87VM2XlK8wAAAAASUVORK5CYII=',
    }),
    // references and the native file dialog, faked.
    attachment_ref: (a) => {
      const target = String(a.target);
      const attachmentId = uid();
      if (a.kind === 'chat') {
        const name = find(String(target)).chat.title;
        sentRefList.set(attachmentId, { kind: 'chat', name, target });
        return { attachmentId, kind: 'chat', name };
      }
      const name = target.split(/[\\/]/).filter(Boolean).pop() ?? target;
      const kind = /\.[a-z0-9]+$/i.test(name) ? 'file' : 'folder';
      sentRefList.set(attachmentId, { kind, name, target });
      return { attachmentId, kind, name };
    },
    files_pick: () => ({ paths: ['C:\\Users\\me\\Documents\\notes.md', 'C:\\Users\\me\\Pictures\\shot.png'] }),
    attachment_put: (a) => {
      const attachmentId = uid();
      attachments.set(attachmentId, { name: String(a.name), mime: String(a.mime), bytes: String(a.bytes) });
      return { attachmentId };
    },
    attachment_get: (a) => {
      const img = attachments.get(String(a.attachmentId));
      if (!img) throw { code: 'not_found', message: 'No such image.' };
      return a.thumb === true ? fakeThumb(img.mime, img.bytes) : { mime: img.mime, bytes: img.bytes };
    },
    skills_list: () => [
      { name: 'review', description: 'Review the current changes' },
      { name: 'compact', description: 'Compact the conversation' },
    ],
    attention_snooze: (a) => {
      flags.delete(String(a.chatId));
      emitAttention();
      return null;
    },
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
    chat_roll_over: () => ({ started: true }),
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
