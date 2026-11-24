// A fake host for checking the UI in a plain browser. It implements the host's ops and
// events with a scripted turn: streamed text, a tool card, and an approval, question or plan
// card when the prompt asks for one ("approve", "question", "plan"). "gate" holds a question card and answers it for you, "gate escalate" hands it over with the gatekeeper's write-up, and "gate fail" shows the quiet line. "signin" fails the turn with an expired-sign-in error. "todo" keeps a live checklist across the turn (a recap card follows any turn with a checklist or three tool steps), "pictures" shows a gallery card, "board" offers an option board ("board multi" lets several be picked) and "code" streams a fenced code block. "options" ends the reply with a numbered list of choices. "long" in a prompt adds a
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
  SessionMark,
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
import { cardsFakeOps, fakeBoard, fakeGallery } from './fake-cards.ts';
import { usageFakeOps } from './fake-usage.ts';
import { updatesFakeOps } from './fake-updates.ts';
import { itemLinksFakeOps } from './fake-itemlinks.ts';
import { hubFakeOps } from './fake-hub.ts';
import { profilesFakeOps } from './fake-profiles.ts';
import { devicesFakeOps, remoteFakeCatalogue, remoteFakeChat, remoteFakeRefusal } from './fake-devices.ts';
import { knowledgeFakeOps } from './fake-knowledge.ts';
import { githubFakeOps } from './fake-github.ts';
import { webSearchFakeOps } from './fake-websearch.ts';
import { dictationFakeOps } from './fake-dictation.ts';
import { fakeGateAddDecided, fakeGateAddStop, gatekeeperFakeOps } from './fake-gatekeeper.ts';
import { localModelsFakeOps } from './fake-localmodels.ts';
import { memindexFakeOps } from './fake-memindex.ts';
import { gpuFakeOps } from './fake-gpu.ts';
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
{ const cards = cardsFakeOps(); registerFakeOps(cards.prefixes, cards.handler); }
{ const us = usageFakeOps(); registerFakeOps(us.prefixes, us.handler); }
{ const up = updatesFakeOps(); registerFakeOps(up.prefixes, up.handler); }
{ const il = itemLinksFakeOps(); registerFakeOps(il.prefixes, il.handler); }
{ const oc = orchestratorFakeOps(); registerFakeOps(oc.prefixes, oc.handler); }
{ const nc = newChatFakeOps(); registerFakeOps(nc.prefixes, nc.handler); }
{ const hub = hubFakeOps(); registerFakeOps(hub.prefixes, hub.handler); }
{ const pf = profilesFakeOps(); registerFakeOps(pf.prefixes, pf.handler); }
{ const dv = devicesFakeOps(); registerFakeOps(dv.prefixes, dv.handler); }
{ const kn = knowledgeFakeOps(); registerFakeOps(kn.prefixes, kn.handler); }
{ const gh = githubFakeOps(); registerFakeOps(gh.prefixes, gh.handler); }
{ const ws = webSearchFakeOps(); registerFakeOps(ws.prefixes, ws.handler); }
{ const dc = dictationFakeOps(); registerFakeOps(dc.prefixes, dc.handler); }
{ const gk = gatekeeperFakeOps(); registerFakeOps(gk.prefixes, gk.handler); }
// Local models: the catalogue holds nothing until a refresh asks the (made-up) servers, as in the real host.
const localModels = localModelsFakeOps();
registerFakeOps(localModels.prefixes, localModels.handler);
{ const gpu = gpuFakeOps(localModels.gpu); registerFakeOps(gpu.prefixes, gpu.handler); }
{ const mi = memindexFakeOps(); registerFakeOps(mi.prefixes, mi.handler); }

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
  /** The idle check's priority for a flagged chat ("high" when the prompt says so), and chats whose check gave up (a prompt containing "gave up"; Retry clears it). */
  const flagPriority = new Map<string, string>();
  const gaveUp = new Set<string>();
  let sendBehavior: SendBehavior = { ...HOME_SEND_BEHAVIOR };
  const prefs = new Map<string, unknown>();

  // A small capacity queue for the dev host: held starts so the Queue panel has rows to show,
  // reorder and drag alike.
  type FakeHold = { chatId: string; title: string; reason: string; class: string; serverId: string | null; since: number; top: boolean };
  let fakeHolds: FakeHold[] = [
    { chatId: 'dev-queue-chat', title: 'Waiting chat', reason: 'local_cap', class: 'normal', serverId: 'strata', since: 0, top: false },
    { chatId: 'dev-queue-chat-2', title: 'Another waiting chat', reason: 'local_cap', class: 'normal', serverId: 'strata', since: 0, top: false },
    { chatId: 'dev-queue-chat-3', title: 'A parked chat', reason: 'manual', class: 'normal', serverId: null, since: 0, top: false },
    { chatId: 'dev-queue-chat-4', title: 'Held by the plan limit', reason: 'claude_capacity', class: 'normal', serverId: null, since: 0, top: false },
  ];
  const fakeQueueList = () => ({
    entries: fakeHolds.map((h) => ({ ...h, since: Date.now() - 60_000 })),
    servers: [{ serverId: 'strata', working: 3, queued: fakeHolds.filter((h) => h.serverId === 'strata').length }],
    claudeQueued: fakeHolds.filter((h) => h.reason === 'claude_capacity').length,
    claudeFull: true,
    claudeUntil: Date.now() + 40 * 60_000,
  });
  const fakeQueueAct = (chatId: string, action: string, beforeChatId?: string) => {
    if (action === 'cancel' || action === 'release') fakeHolds = fakeHolds.filter((h) => h.chatId !== chatId);
    else if (action === 'start_anyway') {
      fakeHolds = fakeHolds.filter((h) => !(h.chatId === chatId && h.reason === 'claude_capacity'));
      // The chat's own hold lifts with its queue row, and one waiting send opens the turn.
      const c = chats.get(chatId);
      if (c) {
        c.chat = { ...c.chat, state: null, queueReason: null };
        setStatus(c, 'idle');
        const next = c.pending.find((s) => s.state === 'accepted');
        if (next) {
          c.pending = c.pending.filter((s) => s !== next);
          emitPending(c);
          addRow(c, { role: 'user', text: next.text, sourceKey: next.id, details: { submissionId: next.id, ...sentExtras.get(next.id) } });
          void runTurn(c, next.text);
        }
      }
    }
    else if (action === 'hold') fakeHolds = fakeHolds.map((h) => (h.chatId === chatId ? { ...h, reason: 'manual', serverId: null } : h));
    else if (action === 'release') fakeHolds = fakeHolds.map((h) => (h.chatId === chatId ? { ...h, reason: 'local_cap' } : h));
    else if (action === 'top') fakeHolds = fakeHolds.map((h) => (h.chatId === chatId ? { ...h, top: true } : h));
    else if (action === 'move') {
      const at = fakeHolds.findIndex((h) => h.chatId === chatId);
      if (at >= 0) {
        const moved = fakeHolds.splice(at, 1)[0]!;
        const anchor = beforeChatId ? fakeHolds.findIndex((h) => h.chatId === beforeChatId) : -1;
        moved.top = anchor >= 0 ? fakeHolds[anchor]!.top : false; // as on the host: the anchor's front state
        fakeHolds.splice(anchor >= 0 ? anchor : fakeHolds.length, 0, moved);
      }
    }
    return fakeQueueList();
  };
  // A small build queue for the dev host: one feature mid-build, one waiting, one paused
  // and one finished, so the Build queue tab has every state to show.
  type FakeBuildEntry = { id: string; profileId: string; title: string; brief?: string; itemId?: string; projectId?: string | null; model?: string | null; position: number; state: string; chatId?: string | null; startedAt?: number | null; finishedAt?: number | null };
  let fakeBuilds: FakeBuildEntry[] = [
    { id: 'dev-build-1', profileId: '', title: 'Wire the importer', brief: 'Read the folder and import each file, reporting what arrives.', model: 'claude-sonnet-5', position: 0, state: 'running', chatId: 'dev-queue-chat', startedAt: Date.now() - 900_000 },
    { id: 'dev-build-2', profileId: '', title: 'Fix the report footer', itemId: 'item-42', position: 1, state: 'waiting' },
    { id: 'dev-build-3', profileId: '', title: 'Retire the old banner', brief: 'Remove the banner and the styles it needed.', position: 2, state: 'paused' },
    { id: 'dev-build-4', profileId: '', title: 'Rename the sidebar tile', brief: 'Say "Seat" where it said "Host".', position: 3, state: 'done', chatId: 'dev-queue-chat-2', startedAt: Date.now() - 4_000_000, finishedAt: Date.now() - 3_000_000 },
  ];
  const fakeBuildList = () =>
    fakeBuilds
      .filter((b) => b.state !== 'removed')
      .sort((a, b) => a.position - b.position)
      .map((b, i) => ({ ...b, position: i }));
  const fakeBuildAct = (kind: 'add' | 'move' | 'pause' | 'resume' | 'remove', a: Record<string, unknown>) => {
    if (kind === 'add') {
      fakeBuilds.push({
        id: `dev-build-${fakeBuilds.length + 1}-${Math.floor(Math.random() * 1e6)}`,
        profileId: '',
        title: String(a.title ?? ''),
        ...(typeof a.brief === 'string' ? { brief: a.brief } : {}),
        ...(typeof a.itemId === 'string' ? { itemId: a.itemId } : {}),
        ...(typeof a.model === 'string' ? { model: a.model } : {}),
        position: fakeBuilds.length,
        state: 'waiting',
      });
    } else if (kind === 'move') {
      const id = String(a.id);
      const at = fakeBuilds.findIndex((b) => b.id === id);
      if (at >= 0) {
        const moved = fakeBuilds.splice(at, 1)[0]!;
        const anchor = typeof a.beforeId === 'string' ? fakeBuilds.findIndex((b) => b.id === a.beforeId) : -1;
        fakeBuilds.splice(anchor >= 0 ? anchor : fakeBuilds.length, 0, moved);
      }
    } else {
      const b = fakeBuilds.find((x) => x.id === String(a.id));
      if (b) {
        if (kind === 'pause' && (b.state === 'waiting' || b.state === 'running')) b.state = 'paused';
        else if (kind === 'resume' && b.state === 'paused') b.state = b.chatId ? 'running' : 'waiting';
        else if (kind === 'remove') {
          b.state = 'removed';
          b.finishedAt = Date.now();
        }
      }
    }
    emit({ kind: 'build_queue_changed' });
    return fakeBuildList();
  };

  const answers = new Map<string, (a: Record<string, unknown>) => void>();
  /** forkId → the chat that fork made, so a repeated request answers the same chat. */
  const forks = new Map<string, string>();

  const rev = () => ++revision;
  const now = () => Date.now();
  const uid = () => (globalThis.crypto?.randomUUID?.() ?? `id-${Math.random().toString(16).slice(2)}`);
  /** Images put by the composer, kept so a sent message's thumbnail can be read back.
   *  `sent` is the submission it is linked to; null means it waits unstaged. */
  const attachments = new Map<string, { chatId: string; name: string; mime: string; bytes: string; sent: string | null }>();
  /** References made by the composer, kept so the sent message's bubble can show them;
   *  same sent/unstaged rule as images. */
  const sentRefList = new Map<string, { chatId: string; kind: string; name: string; target: string; sent: string | null }>();
  /** The attachment ids a send took, so withdrawing or recovering it unstages them again. */
  const sentIds = new Map<string, string[]>();
  /** The images and references of a send, for the row written when its turn starts. */
  const sentExtras = new Map<string, Pick<MessageDetails, 'images' | 'refs'>>();

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
  function emitPending(c: FakeChat) {
    emit({ kind: 'pending', chatId: c.chat.id, revision: rev(), pending: c.pending });
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

  function newChat(cwd: string, model = 'claude-opus-5-5', effort = 'xhigh'): FakeChat {
    const id = uid();
    const t = now();
    const c: FakeChat = {
      chat: {
        id,
        projectId: `p:${cwd}`,
        cwd,
        title: 'New chat',
        status: 'idle',
        stackId: null,
        prevSessionId: null,
        nextSessionId: null,
        fastMode: null,
        error: null,
        harness: 'claude',
        model,
        effort,
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
        localAhead: null,
        state: null,
        started: false,
        queueReason: null,
        profileId: '',
        hostDeviceId: '',
        sharedWith: [],
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
    if (n > 0) c.chat = { ...c.chat, started: true };
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

  // A stand-in for the host's short phrase, so the browser-only preview shows what the real
  // app will show on the activity dock and the Details list.
  function fakeToolSummary(name: string, args: Record<string, unknown>): string | null {
    const text = (v: unknown) => (typeof v === 'string' ? v.trim() : '');
    const desc = text(args.description);
    if (desc) return desc;
    const file = text(args.file_path) || text(args.notebook_path) || text(args.path);
    if (file) {
      const verb = name === 'Read' ? 'Reading' : name === 'Write' ? 'Writing' : name === 'LS' ? 'Listing' : 'Editing';
      return `${verb} ${file.split(/[\\/]/).pop()}`;
    }
    const pattern = text(args.pattern);
    if (pattern) return `Searching ${pattern}`;
    const query = text(args.query);
    if (query) return name === 'WebSearch' ? `Web search: ${query}` : query;
    return null;
  }

  async function tool(c: FakeChat, name: string, args: Record<string, unknown>, output: string, cancelled: () => boolean, ms = 900) {
    const toolCallId = uid();
    const summary = fakeToolSummary(name, args);
    let r = addRow(c, { role: 'toolResult', toolName: name, toolCallId, status: 'streaming', details: { toolArgs: JSON.stringify(args), stoppable: true, ...(summary ? { toolSummary: summary } : {}) } });
    c.thread = { ...c.thread, liveTools: [...c.thread.liveTools, { toolCallId, name, summary, startedAt: now(), stoppable: name === 'Bash' }] };
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
    emitPending(c);
  }

  const isLocal = (c: FakeChat) => !!c.chat.model?.startsWith('local/');

  async function runTurn(c: FakeChat, text: string) {
    let stopped = false;
    const cancelled = () => stopped;
    c.turn = { cancel: () => (stopped = true) };
    flags.delete(c.chat.id);
    gaveUp.delete(c.chat.id);
    c.chat = { ...c.chat, started: true };
    // "[loading]" on a local model: the host's "Loading <model>…" line while the server brings it up.
    if (isLocal(c) && /\[loading\]/i.test(text)) {
      setStatus(c, 'starting');
      c.thread = { ...c.thread, loading: { text: `Loading ${c.chat.model?.split('/').slice(2).join('/')}…`, startedAt: now() } };
      emitThread(c);
      try {
        await sleep(step * 14, cancelled);
      } catch {
        setStatus(c, 'stopped');
        c.turn = null;
        return;
      } finally {
        const { loading: _gone, ...rest } = c.thread;
        void _gone;
        c.thread = rest;
        emitThread(c);
      }
    }
    // "signin": the turn fails the way an expired Claude sign-in does, so the row offers "Sign in again".
    if (/signin/i.test(text)) {
      setStatus(c, 'running');
      try {
        await sleep(step, cancelled);
        addRow(c, {
          role: 'assistant',
          status: 'error',
          details: { errorMessage: 'Failed to authenticate: OAuth session expired and could not be refreshed' },
        });
        setStatus(c, 'idle');
      } catch {
        setStatus(c, 'stopped');
      } finally {
        c.turn = null;
      }
      return;
    }
    if (/flag/i.test(text)) flags.set(c.chat.id, `You asked for "${text.slice(0, 40)}" and the last reply stopped before the tests were run. It looks unfinished: nobody has confirmed the change works.`);
    if (/flag/i.test(text)) flagPriority.set(c.chat.id, /high/i.test(text) ? 'high' : 'normal');
    if (/gave ?up/i.test(text)) gaveUp.add(c.chat.id);
    setStatus(c, 'running');
    try {
      await sleep(step * 2, cancelled);
      await streamText(c, `Looking into "${text.slice(0, 40)}". I'll check the folder first. `, cancelled, 'The user wants something. **First** I should look at the files, then decide.');
      if (/long/i.test(text)) {
        c.thread = { ...c.thread, backgroundTasks: [{ id: uid(), title: 'npm test', command: 'npm test', kind: 'shell', startedAt: now() }] };
        emitThread(c);
      }
      // "todo" keeps a checklist the whole turn: written first, then updated as each step finishes.
      const steps = ['Read the code', 'Change it', 'Test it'];
      const plan = async (done: number) => {
        if (!/todo/i.test(text)) return;
        const todos = steps.map((content, i) => ({ content, status: i < done ? 'completed' : i === done ? 'in_progress' : 'pending', activeForm: content }));
        await tool(c, 'TodoWrite', { todos }, 'Todos updated', cancelled, 500);
      };
      await plan(0);
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
      await plan(1);
      await tool(c, 'Edit', { file_path: 'src/main.ts', old_string: 'export {}', new_string: 'export const x = 1;\nexport const y = 2;' }, 'ok', cancelled);
      await plan(2);
      if (/pictures/i.test(text)) {
        const g = fakeGallery(c.chat.id);
        const images = Array.from({ length: g.count }, (_, i) => ({ path: `C:/work/banner-${i + 1}.png`, caption: `Option ${i + 1}` }));
        await tool(c, 'mcp__helm__app_show_images', { title: 'Banner options', images }, JSON.stringify(g), cancelled, 600);
      }
      if (/board/i.test(text)) {
        const labels = ['Sunset', 'Ocean', 'Forest', 'Gold'];
        const multiSelect = /multi/i.test(text);
        const question = multiSelect ? 'Which colours for the bar?' : 'Which colour for the bar?';
        const boardId = fakeBoard(c.chat.id, question, labels, { multiSelect });
        await tool(c, 'mcp__helm__app_offer_options', { question, options: labels.map((label) => ({ label })), multiSelect }, JSON.stringify({ boardId, next: 'The board is on screen.' }), cancelled, 600);
      }
      if (/code/i.test(text)) await streamText(c, 'Here is the change:\n\n```ts\nexport const x = 1;\nexport const y = 2;\n```\n\n', cancelled);
      if (/subagent/i.test(text)) runFakeSubagents(c, { addRow, updateRow, emitThread, uid });
      // The gatekeeper's card lifecycle by eye: "gate" holds a card and answers it, "gate
      // escalate" hands it over with the brief, "gate fail" leaves the quiet line.
      if (/gate/i.test(text)) {
        const mode = /escalate/i.test(text) ? 'escalate' : /fail/i.test(text) ? 'fail' : 'decide';
        const id = uid();
        const card: Interaction = {
          chatId: c.chat.id, id, method: 'select', attention: 'question',
          title: 'Which export format?', options: ['CSV', 'JSON'],
          optionDescriptions: ['Opens straight in a spreadsheet', 'Keeps the nested lists'],
          allowOther: true, gate: { state: 'checking' },
        };
        c.thread = { ...c.thread, interactions: [...c.thread.interactions, card] };
        emitThread(c);
        emitAttention();
        const answered = new Promise<Record<string, unknown>>((resolve) => answers.set(id, resolve));
        await sleep(2500, cancelled);
        if (mode === 'decide') {
          c.thread = { ...c.thread, interactions: c.thread.interactions.filter((x) => x.id !== id) };
          emitThread(c);
          emitAttention();
          const rec = fakeGateAddDecided(c.chat.id, c.chat.title || 'Untitled', 'Which export format?', 'CSV', 'it opens straight in a spreadsheet and the import accepts it back');
          emit({ kind: 'gate', askId: rec.id, chatId: c.chat.id, what: 'decided' });
          addRow(c, { role: 'context', text: `«CSV», because ${rec.reason}.`, details: { contextKind: 'gatekeeper' } });
          await streamText(c, 'Exported as CSV. If you would rather have JSON, the Needs attention list can change that answer. ', cancelled);
        } else {
          const next: Interaction =
            mode === 'escalate'
              ? {
                  ...card,
                  prefill: 'JSON',
                  gate: {
                    state: 'escalated',
                    brief: {
                      problem: 'The chat wants to pick one export format for every note. Changing it later means exporting everything again.',
                      options: [
                        { label: 'CSV', means: 'A plain table in one file.', pros: 'Opens anywhere', cons: 'Flattens the nested lists', cost: 'None' },
                        { label: 'JSON', means: 'Keeps the structure as it is.', pros: 'Nothing is lost', cons: 'Awkward in a spreadsheet', cost: 'None' },
                      ],
                      recommendation: 'JSON',
                      why: 'These notes have nested lists that CSV would flatten.',
                    },
                  },
                }
              : { ...card, gate: { state: 'failed', note: "Helm couldn't check this one." } };
          c.thread = { ...c.thread, interactions: c.thread.interactions.map((x) => (x.id === id ? next : x)) };
          emitThread(c);
          emitAttention();
          emit({ kind: 'gate', askId: id, chatId: c.chat.id, what: mode === 'escalate' ? 'escalated' : 'failed' });
          const a = await answered;
          await streamText(c, `You picked ${String(a.value ?? 'nothing')}. `, cancelled);
        }
      }
      // The working mode's stop check by eye: "stop" ends the turn with the quiet checking line,
      // then Helm's nudge arrives and the chat carries on with the default; "stop blocker" ends
      // on a Blocker row that counts in attention.
      if (/stop/i.test(text)) {
        const blocker = /blocker/i.test(text);
        if (blocker) {
          await streamText(c, 'I need you to sign in before I can carry on. ', cancelled);
          c.thread = { ...c.thread, stopCheck: { startedAt: now() } };
          emitThread(c);
          await sleep(2500, cancelled);
          c.thread = { ...c.thread, stopCheck: undefined };
          emitThread(c);
          addRow(c, { role: 'context', text: 'Only you can sign in, so it stopped.', details: { contextKind: 'stopBlocker' } });
          const rec = fakeGateAddStop(c.chat.id, c.chat.title || 'Untitled', 'Sign in to continue?', 'blocker', undefined, 'Only you can sign in.');
          emit({ kind: 'gate', askId: rec.id, chatId: c.chat.id, what: 'escalated' });
          emitAttention();
        } else {
          await streamText(c, 'The first part is done. Shall I go ahead with the next step? ', cancelled);
          c.thread = { ...c.thread, stopCheck: { startedAt: now() } };
          emitThread(c);
          await sleep(2500, cancelled);
          c.thread = { ...c.thread, stopCheck: undefined };
          emitThread(c);
          const rec = fakeGateAddStop(c.chat.id, c.chat.title || 'Untitled', 'Start the next step?', 'asked', 'the next step', 'the plan names the next step and it is easy to undo');
          emit({ kind: 'gate', askId: rec.id, chatId: c.chat.id, what: 'decided' });
          addRow(c, { role: 'user', text: '[Helm · Run to completion] Carry on without asking. Go ahead with: «the next step». (You were about to ask: «Start the next step?».)' });
          await streamText(c, 'Carrying on: I took the next step and kept it easy to undo. ', cancelled);
        }
      }
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
      await plan(3);
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
    const next = c.pending.find((s) => s.mode === 'prompt');
    if (!next) return;
    c.pending = c.pending.filter((s) => s !== next);
    emitPending(c);
    addRow(c, { role: 'user', text: next.text, sourceKey: next.id, details: { submissionId: next.id, ...sentExtras.get(next.id) } });
    void runTurn(c, next.text);
  }

  /** A first cut of the host's attention summary: running chats and pending interactions. */
  function attention() {
    const all = [...chats.values()].filter((c) => !c.chat.archived && !c.chat.sideSourceId);
    const list = all.filter((c) => !c.chat.muted);
    const row = (c: FakeChat, kind: string) => ({ chatId: c.chat.id, title: c.chat.title || 'Untitled', project: folderName(c.chat.cwd), kind, reason: kind, profileId: '' });
    // A card the gatekeeper is still holding is not waiting on you yet: like the host, it is
    // left out of the badge, the inbox and the muted list until the gate answers or fails.
    const awaiting = (i: Interaction) => i.attention && i.gate?.state !== 'checking';
    const busy = list.filter((c) => ['starting', 'running', 'stopping'].includes(c.chat.status));
    const needs = list.filter((c) => c.thread.interactions.some(awaiting));
    // Like the host: only a manual mark needs attention; a clean finish is held in `finished`.
    const marked = list.filter((c) => c.chat.unread.markToken);
    const finished = list.filter((c) => !c.chat.unread.markToken && c.chat.unread.completionId && c.chat.status === 'idle' && !c.chat.error);
    return {
      badge: new Set([...needs, ...marked]).size,
      busy: all.filter((c) => ['starting', 'running', 'stopping'].includes(c.chat.status)).length,
      needsYou: needs.slice(0, 8).map((c) => {
        const a = c.thread.interactions.find(awaiting)?.attention;
        return row(c, a === 'trust' ? 'approval' : String(a));
      }),
      running: busy.slice(0, 8).map((c) => row(c, 'running')),
      unread: marked.slice(0, 8).map((c) => row(c, 'unread')),
      finished: finished.slice(0, 8).map((c) => row(c, 'unread')),
      stalled: [],
      muted: all
        .filter((c) => c.chat.muted && c.thread.interactions.some(awaiting))
        .map((c) => {
          const a = c.thread.interactions.find(awaiting)?.attention;
          return { ...row(c, a === 'trust' ? 'approval' : String(a)), reason: a === 'plan' ? 'Has a plan for you to review' : 'Waiting for your approval' };
        }),
      triage: [
        ...list.filter((c) => flags.has(c.chat.id) && !gaveUp.has(c.chat.id)).map((c) => ({ chatId: c.chat.id, title: c.chat.title || 'Untitled', project: folderName(c.chat.cwd), priority: flagPriority.get(c.chat.id) ?? null, failed: false, kind: null })),
        ...list.filter((c) => gaveUp.has(c.chat.id)).map((c) => ({ chatId: c.chat.id, title: c.chat.title || 'Untitled', project: folderName(c.chat.cwd), priority: null, failed: true, kind: null })),
      ],
      flagged: list
        .filter((c) => flags.has(c.chat.id) && !['starting', 'running', 'stopping'].includes(c.chat.status) && !needs.includes(c) && !marked.includes(c) && !finished.includes(c))
        .slice(0, 8)
        .map((c) => ({ ...row(c, 'triage'), reason: flags.get(c.chat.id) ?? '' })),
      revision,
    };
  }

  // Settings → Runtime's stored settings.
  const chatVerbosity = new Map<string, string>();
  const runtimeSettings: Record<string, unknown> = { executable: '', skipPermissions: true, fastMode: false, worktreeTools: false, scheduleTools: false, shellChanges: true, lowPriority: true, subagentLimits: 'orchestrator', subagentHardStop: false, subagentSoftTurns: 100, toolOutputCap: true, model: null, effort: 'xhigh', responseVerbosity: 'low', problem: null };

  // Settings → Runtime's repositories.
  const repos: { repoId: string; remoteUrl?: string; worktreesAllowed: boolean; locations: { id: string; path: string }[] }[] = [
    { repoId: 'github.com/me/demo', remoteUrl: 'https://github.com/me/demo', worktreesAllowed: true, locations: [{ id: 'l1', path: 'C:/Users/me/Documents/Coding/demo' }] },
  ];

  function find(chatId: unknown): FakeChat {
    let c = chats.get(String(chatId));
    // A chat of another device is made here the first time it is asked for (fake-devices.ts).
    const remote = c ? null : remoteFakeChat(String(chatId));
    if (remote) {
      c = newChat(remote.cwd, remote.model ?? undefined, remote.effort ?? undefined);
      chats.delete(c.chat.id);
      c.chat = { ...remote };
      c.thread = { ...c.thread, chatId: remote.id, status: remote.status };
      chats.set(remote.id, c);
      seed(c, 3);
    }
    if (!c) throw { code: 'not_found', message: 'No such chat.' };
    return c;
  }

  /** A change to a chat on a device that cannot take it now is refused in the host's words. */
  function remoteGuard(c: FakeChat) {
    const refusal = c.chat.hostDeviceId ? remoteFakeRefusal(c.chat.hostDeviceId) : null;
    if (refusal) throw refusal;
  }

  /** The chat's roll-over sessions, oldest first: the chain it sits in, or just it. */
  function sessionsOf(c: FakeChat): FakeChat[] {
    if (!c.chat.stackId) return [c];
    const chain = [c];
    let cur = c;
    while (cur.chat.prevSessionId) {
      const p = chats.get(cur.chat.prevSessionId);
      if (!p) break;
      chain.unshift(p);
      cur = p;
    }
    let nxt = c;
    while (nxt.chat.nextSessionId) {
      const n = chats.get(nxt.chat.nextSessionId);
      if (!n) break;
      chain.push(n);
      nxt = n;
    }
    return chain;
  }

  function page(c: FakeChat, a: Record<string, unknown>): TranscriptPage {
    // With `stack` the page covers the chat's whole roll-over stack as one transcript, ids
    // ascending across sessions, with a mark wherever two sessions meet inside the page.
    const all = a.stack === true ? sessionsOf(c).flatMap((s) => s.rows) : c.rows;
    let end = all.length;
    if (typeof a.before === 'string') end = Math.max(0, all.findIndex((r) => String(r.id) === a.before));
    if (typeof a.around === 'number') {
      const at = all.findIndex((r) => r.id === a.around);
      if (at >= 0) end = Math.min(all.length, at + 1 + Math.floor(PAGE / 2));
    }
    const start = Math.max(0, end - PAGE);
    const messages = all.slice(start, end);
    const sessionMarks: SessionMark[] = [];
    for (let j = 1; j < messages.length; j++) {
      const above = messages[j - 1]!;
      const below = messages[j]!;
      if (above.chatId === below.chatId) continue;
      sessionMarks.push({
        aboveMessageId: above.id,
        belowMessageId: below.id,
        sessionId: above.chatId,
        title: chats.get(above.chatId)?.chat.title ?? above.chatId,
        rolledOverAt: chats.get(below.chatId)?.chat.createdAt ?? 0,
      });
    }
    return {
      chatId: c.chat.id,
      messages,
      hasOlder: start > 0,
      hasNewer: end < all.length,
      olderCursor: start > 0 ? String(all[start]!.id) : null,
      newerCursor: null,
      runs: [],
      pending: c.pending,
      sessionMarks,
      revision,
    };
  }

  // The Claude hold names a real chat, so the queue row's Open reaches a chat that carries
  // the same hold: its waiting line (holdLine, Start anyway) and the host's parking of a
  // send (chat_send) are then reachable in the dev fake. The chat sits idle like any seeded
  // chat — arm_hold only shows it "starting" once a send is actually parked there.
  if (opts.seedRows) {
    const c = newChat('C:/Users/me/Documents/Coding/demo');
    c.chat.title = 'Seeded chat';
    seed(c, opts.seedRows);
    c.chat = { ...c.chat, queueReason: 'claude_capacity', state: { kind: 'queued', reason: 'claude_capacity', pct: 96, until: Date.now() + 40 * 60_000 } };
    const held = fakeHolds.find((h) => h.reason === 'claude_capacity');
    if (held) {
      held.chatId = c.chat.id;
      held.title = c.chat.title;
    }
  } else {
    const c = newChat('C:/Users/me/Documents/Coding/plan-limit');
    c.chat.title = 'Held by the plan limit';
    seed(c, 3);
    c.chat = { ...c.chat, queueReason: 'claude_capacity', state: { kind: 'queued', reason: 'claude_capacity', pct: 96, until: Date.now() + 40 * 60_000 } };
    const held = fakeHolds.find((h) => h.reason === 'claude_capacity');
    if (held) held.chatId = c.chat.id; // the same title already names it
  }

  // Settings → Usage governor: a calm stand-in. Save updates the settings and echoes them back.
  let governor = { enabled: true, selectivePct: 70, windDownPct: 85, stopPct: 95, atStop: 'stop' as 'stop' | 'wrapUp' };
  const governorView = () => ({ settings: governor, defaults: governor, band: 'surplus', usedPct: 20, since: null, told: 0, stopped: 0 });

  const ops: Record<string, (a: Record<string, unknown>) => unknown> = {
    app_info: () => ({ name: 'Helm', edition: 'desktop', version: 'dev', schemaVersion: 1, profileDir: '(fake host)' }),
    quit_request: () => null,
    governor_state: () => governorView(),
    governor_settings: (a) => {
      governor = { ...governor, ...(a.settings as object) } as typeof governor;
      return governorView();
    },
    chat_create: (a) => {
      const chat = newChat(String(a.cwd), typeof a.model === 'string' ? a.model : undefined, typeof a.effort === 'string' ? a.effort : undefined).chat;
      // The board it asked for: the projects fake shows it there (without one it picks the board itself).
      if (typeof a.boardId === 'string') fakeOpFor('chat_organize')?.('chat_organize', { chatId: chat.id, groupProjectId: a.boardId }, { emit, call: (o, x = {}) => ops[o]?.(x) });
      return chat;
    },
    // As the host: a copy of the chat (whole, or up to a reply) titled "Fork · <source>"; a side chat
    // starts from a quoted excerpt and stays out of the list. A repeat of one forkId answers the same fork.
    chat_fork: (a) => {
      const src = find(a.chatId);
      remoteGuard(src);
      const known = forks.get(String(a.forkId));
      if (known) return chats.get(known)!.chat;
      const side = a.side === true;
      const excerpt = typeof a.excerpt === 'string' ? a.excerpt : '';
      if (side && (excerpt.trim() === '' || excerpt.length > 16000)) throw { code: 'bad_args', message: 'A side chat needs an excerpt of 1 to 16,000 characters.' };
      let rows = src.rows;
      if (typeof a.messageId === 'number') {
        const at = rows.findIndex((r) => r.id === a.messageId);
        if (at < 0) throw { code: 'not_found', message: 'No such message.' };
        rows = rows.slice(0, at + 1);
      }
      const c = newChat(src.chat.cwd, src.chat.model ?? undefined, src.chat.effort ?? undefined);
      const base = src.chat.title.replace(/^(?:Fork|Side chat) · /, '').trim() || 'New chat';
      const quoted = excerpt.split('\n').map((l) => (l ? `> ${l}` : '>')).join('\n');
      const draft = side ? `Regarding this excerpt:\n\n${quoted}\n\n` : typeof a.messageId === 'number' ? '' : src.chat.draft;
      c.chat = { ...c.chat, projectId: src.chat.projectId, title: `${side ? 'Side chat' : 'Fork'} · ${base}`, titleState: 'generated', draft, sideSourceId: side ? src.chat.id : null, started: false };
      c.rows = rows.map((r) => {
        const id = rowId++;
        return { ...r, id, chatId: c.chat.id, sourceKey: `${r.sourceKey}-fork-${id}`, sequence: id, revision: rev() };
      });
      forks.set(String(a.forkId), c.chat.id);
      emitChat(c);
      // A fork is shown on its source's board (a side chat is not shown in the sidebar at all).
      if (!side) {
        const ctx = { emit, call: (o: string, x: Record<string, unknown> = {}) => ops[o]?.(x) };
        const [from] = (fakeOpFor('sidebar_rows')?.('sidebar_rows', { chatIds: [src.chat.id] }, ctx) as { groupProjectId: string }[] | undefined) ?? [];
        if (from) fakeOpFor('chat_organize')?.('chat_organize', { chatId: c.chat.id, groupProjectId: from.groupProjectId }, ctx);
        emit({ kind: 'sidebar', chatIds: [c.chat.id] });
        // The profiles fake reads the new chat's owner the next time it is asked.
        emit({ kind: 'profiles_changed' });
      }
      return c.chat;
    },
    chat_list: () => [...chats.values()].map((c) => c.chat).filter((c) => !c.archived && !c.sideSourceId && !c.hostDeviceId).sort((x, y) => y.createdAt - x.createdAt),
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
    chat_stack: (a) => {
      const sessions = sessionsOf(find(a.chatId));
      return {
        stackId: sessions[0]!.chat.stackId,
        latestId: sessions[sessions.length - 1]!.chat.id,
        sessions: sessions.map((s) => ({ id: s.chat.id, title: s.chat.title, archived: s.chat.archived, createdAt: s.chat.createdAt })),
      };
    },
    stack_find: (a) => {
      const sessions = sessionsOf(find(a.chatId));
      const q = String(a.query ?? '').toLowerCase();
      const rows: { sessionId: string; id: number; count: number }[] = [];
      if (q.trim()) {
        for (const s of [...sessions].reverse()) {
          for (const r of [...s.rows].reverse()) {
            const text = (r.role === 'user' || r.role === 'assistant' || a.includeActivity ? r.text : '').toLowerCase();
            let count = 0;
            for (let i = text.indexOf(q); i >= 0; i = text.indexOf(q, i + q.length)) count++;
            if (count) rows.push({ sessionId: s.chat.id, id: r.id, count });
          }
        }
      }
      return { rows: rows.slice(0, 50), truncated: rows.length > 50 };
    },
    chat_send: (a) => {
      const c = find(a.chatId);
      remoteGuard(c);
      const text = String(a.text ?? '');
      const ids = (a.attachments as string[] | undefined) ?? [];
      const sentImages: { id: string; name: string; mime: string; width: number; height: number }[] = [];
      const sentRefs: { kind: string; name: string; target: string }[] = [];
      // Every id must name an unstaged attachment of this chat; one another message already
      // sent is refused, the same way the real host refuses it.
      for (const id of ids) {
        const img = attachments.get(id);
        const ref = sentRefList.get(id);
        const owner = img ?? ref;
        if (!owner || owner.chatId !== c.chat.id || owner.sent !== null)
          throw { code: 'bad_args', message: 'An attachment is missing or already sent; attach it again.' };
        if (img) sentImages.push({ id, name: img.name, mime: img.mime, width: 160, height: 120 });
        else if (ref) sentRefs.push({ kind: ref.kind, name: ref.name, target: ref.target });
      }
      if (!text.trim() && sentImages.length === 0 && sentRefs.length === 0) throw { code: 'refused', message: 'Nothing to send.' };
      const busy = c.chat.status === 'running' || c.chat.status === 'starting';
      const sid = String(a.submissionId);
      // A send the host accepts takes its attachments out of the unstaged set; a refused one
      // puts them back (below, in the refusal path).
      for (const id of ids) {
        const owner = attachments.get(id) ?? sentRefList.get(id);
        if (owner) owner.sent = sid;
      }
      sentIds.set(sid, ids);
      sentExtras.set(sid, { ...(sentImages.length ? { images: sentImages } : {}), ...(sentRefs.length ? { refs: sentRefs } : {}) });
      const action = String(a.action);
      const mode = action === 'default' ? sendBehavior.mode : action;
      const s: Submission = {
        id: sid,
        chatId: c.chat.id,
        text,
        mode: 'prompt',
        state: 'accepted',
        origin: a.origin === 'action' || a.origin === 'notification' ? a.origin : 'typed',
        createdAt: now(),
        // A waiting bubble shows its pictures and files, as a sent bubble does.
        ...(sentImages.length ? { images: sentImages } : {}),
        ...(sentRefs.length ? { refs: sentRefs } : {}),
      };
      if (c.chat.title === 'New chat') c.chat = { ...c.chat, title: text.slice(0, 40) };
      if (c.chat.state?.kind === 'queued') {
        // The host's arm_hold: a queued chat keeps the send instead of opening a turn — it
        // waits as a pending row, the chat shows starting with its queued state kept, and
        // the queue's Start anyway (or the hold lifting) releases it into a turn.
        c.pending = [...c.pending, s];
        emitPending(c);
        // A parked send is accepted, so the composer's draft clears here too; setStatus
        // emits the chat, and the turn waits for the queue to release it, so this exits.
        if ((a.origin ?? 'composer') === 'composer') c.chat = { ...c.chat, draft: '' };
        setStatus(c, 'starting');
        return { submission: s, chat: c.chat };
      } else if (!busy && isLocal(c) && /\[fit\]/i.test(text)) {
        // The host's fit check refusing a turn: no turn runs, a notice says why, and the text
        // and attachments come back to the composer.
        const unstage = () => {
          for (const id of ids) {
            const owner = attachments.get(id) ?? sentRefList.get(id);
            if (owner) owner.sent = null;
          }
          sentIds.delete(sid);
        };
        const name = c.chat.model?.split('/').slice(2).join('/') ?? 'The model';
        addRow(c, { role: 'context', text: `${name} can't load while Strata is answering in Refactor parser. Send again when it finishes.`, details: { contextKind: 'notice' } });
        c.chat = { ...c.chat, draft: text, draftRevision: Math.max(now(), c.chat.draftRevision + 1) };
        emitChat(c);
        unstage();
        return { submission: { ...s, state: 'rejected' as const }, chat: c.chat };
      }
      if (!busy) {
        addRow(c, { role: 'user', text, sourceKey: s.id, details: { submissionId: s.id, ...(sentImages.length ? { images: sentImages } : {}), ...(sentRefs.length ? { refs: sentRefs } : {}) } });
        void runTurn(c, text);
      } else if (mode === 'asap' || mode === 'steer') {
        s.mode = 'steer';
        c.pending = [...c.pending, s];
        emitPending(c);
      } else {
        // Interrupt: the running turn stops, and this message opens the next one as soon as it has.
        c.pending = [...c.pending, s];
        emitPending(c);
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
      remoteGuard(c);
      if (!c.turn) return { status: c.chat.status };
      setStatus(c, 'stopping');
      c.turn.cancel();
      // One Stop: whatever Claude had not read goes back to it at once, the oldest opening the next turn.
      const firstUnread = c.pending.find((s) => s.state === 'accepted' && s.mode === 'steer');
      if (firstUnread) c.pending = c.pending.map((s) => (s === firstUnread ? { ...s, mode: 'prompt' as const } : s));
      setTimeout(() => startNext(c), step * 3);
      return { status: 'stopping' };
    },
    submission_action: (a) => {
      for (const c of chats.values()) {
        const s = c.pending.find((x) => x.id === a.submissionId);
        if (!s) continue;
        let out: Submission = s;
        if (a.action === 'withdraw' || a.action === 'recover') {
          // The message's images and files go back to the unstaged set with its text, so the
          // composer can stage them again for a new send.
          for (const id of sentIds.get(s.id) ?? []) {
            const owner = attachments.get(id) ?? sentRefList.get(id);
            if (owner) owner.sent = null;
          }
          sentIds.delete(s.id);
          out = { ...s, state: a.action === 'withdraw' ? 'withdrawn' : 'superseded' };
          delete out.images;
          delete out.refs;
          c.pending = c.pending.filter((x) => x.id !== s.id);
          c.chat = { ...c.chat, draft: s.text, draftRevision: Math.max(now(), c.chat.draftRevision + 1) };
          emitChat(c);
        }
        emitPending(c);
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
      remoteGuard(c);
      c.chat = { ...c.chat, model: (a.model as string) ?? c.chat.model, effort: (a.effort as string) ?? c.chat.effort };
      emitChat(c);
      addRow(c, { role: 'context', text: `Model set to ${c.chat.model} · ${c.chat.effort}`, details: { contextKind: 'modelChange' } });
      return c.chat;
    },
    // the fast-mode bolt sets the chat's own pick; null drops it back to the new-chat default.
    chat_set_fast_mode: (a) => {
      const c = find(a.chatId);
      c.chat = { ...c.chat, fastMode: (a.on as boolean | null) ?? null, revision: rev() };
      emitChat(c);
      return c.chat;
    },
    // the permission-mode pick lands as the live mode, as a worker would report it.
    chat_set_permission_mode: (a) => {
      const c = find(a.chatId);
      c.thread = { ...c.thread, permissionMode: String(a.mode) };
      emitThread(c);
      return { mode: String(a.mode) };
    },
    model_catalogue: (a) => typeof a.deviceUid === 'string' ? remoteFakeCatalogue(a.deviceUid) : ({
      harness: 'claude',
      models: [
        { id: 'claude-opus-5-5', name: 'Opus 5.5', description: 'Most capable' },
        { id: 'claude-sonnet-5', name: 'Sonnet 5', description: 'Balanced' },
        { id: 'claude-haiku-5', name: 'Haiku 5', description: 'Fastest' },
      ],
      efforts: ['low', 'medium', 'high', 'xhigh', 'max'],
      defaultEffort: 'xhigh',
      source: 'fallback',
      // Empty until the picker or Settings → Local models asks the servers (fake-localmodels.ts).
      local: localModels.catalogue.local(),
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
    // A chat's own reply length; a chat without one follows the new-chat default.
    chat_verbosity_get: (a) => {
      const own = chatVerbosity.get(String(a.chatId));
      return { level: own ?? runtimeSettings.responseVerbosity, own: own !== undefined, default: runtimeSettings.responseVerbosity };
    },
    chat_verbosity_set: (a) => {
      if (a.level === null || a.level === undefined) chatVerbosity.delete(String(a.chatId));
      else chatVerbosity.set(String(a.chatId), String(a.level));
      const own = chatVerbosity.get(String(a.chatId));
      return { level: own ?? runtimeSettings.responseVerbosity, own: own !== undefined, default: runtimeSettings.responseVerbosity };
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
    triage_retry: (a) => {
      const queued = gaveUp.delete(String(a.chatId));
      emitAttention();
      return { queued };
    },
    chat_retry_now: (a) => {
      find(a.chatId);
      // The fake chats keep no outage retries pending; always answer that the next look tries.
      return { scheduled: true };
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
      bytes: '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',
    }),
    // references and the native file dialog, faked.
    attachment_ref: (a) => {
      const target = String(a.target);
      const chatId = String(a.chatId);
      const name = a.kind === 'chat' ? find(String(target)).chat.title : target.split(/[\\/]/).filter(Boolean).pop() ?? target;
      const kind = a.kind === 'chat' ? 'chat' : /\.[a-z0-9]+$/i.test(name) ? 'file' : 'folder';
      // Attaching the same thing twice stages it once, as the host's does.
      for (const [id, r] of sentRefList) {
        if (r.chatId === chatId && r.kind === kind && r.target === target && r.sent === null) return { attachmentId: id, kind: r.kind, name: r.name };
      }
      const attachmentId = uid();
      sentRefList.set(attachmentId, { chatId, kind, name, target, sent: null });
      return { attachmentId, kind, name };
    },
    attachment_text_put: (a) => {
      const attachmentId = uid();
      const name = String(a.name);
      const target = `C:/Users/me/AppData/Local/Helm/attachments/${attachmentId}/${name}`;
      sentRefList.set(attachmentId, { chatId: String(a.chatId), kind: 'file', name, target, sent: null });
      return { attachmentId, kind: 'file', name, target };
    },
    title_suggest: () => ({ title: 'Suggested chat title', titles: ['Suggested chat title', 'Shop 2.0 · team X: suggested title', 'Another suggested name'] }),
    files_pick: () => ({ paths: ['C:\\Users\\me\\Documents\\notes.md', 'C:\\Users\\me\\Pictures\\shot.png'] }),
    attachment_put: (a) => {
      const attachmentId = uid();
      attachments.set(attachmentId, { chatId: String(a.chatId), name: String(a.name), mime: String(a.mime), bytes: String(a.bytes), sent: null });
      return { attachmentId };
    },
    attachment_get: (a) => {
      const img = attachments.get(String(a.attachmentId));
      if (!img) throw { code: 'not_found', message: 'No such image.' };
      return a.thumb === true ? fakeThumb(img.mime, img.bytes) : { mime: img.mime, bytes: img.bytes };
    },
    attachment_discard: (a) => {
      const id = String(a.attachmentId);
      const img = attachments.get(id);
      if (img && img.chatId === String(a.chatId) && img.sent === null) return { discarded: attachments.delete(id) };
      const ref = sentRefList.get(id);
      if (ref && ref.chatId === String(a.chatId) && ref.sent === null) return { discarded: sentRefList.delete(id) };
      return { discarded: false };
    },
    // The chat's attachments that are not sent yet: what a reopening composer stages again, and
    // what a taken-back send's message brings back.
    attachment_list: (a) => {
      const chatId = String(a.chatId);
      const images = [...attachments.entries()]
        .filter(([, x]) => x.chatId === chatId && x.sent === null)
        .map(([id, x]) => ({ id, name: x.name, mime: x.mime, size: atob(x.bytes).length, width: 160, height: 120 }));
      const refs = [...sentRefList.entries()]
        .filter(([, x]) => x.chatId === chatId && x.sent === null)
        .map(([id, x]) => ({ id, kind: x.kind, target: x.target, name: x.name }));
      return { images, refs };
    },
    // A recovered draft gets copies of its message's images and file references: the message
    // keeps its own, and the copies wait unstaged for the next send.
    attachment_recover: (a) => {
      const c = find(a.chatId);
      const row = c.rows.find((r) => r.id === Number(a.messageId) && r.role === 'user');
      if (!row) throw { code: 'not_found', message: 'No such message.' };
      let images = 0;
      let refs = 0;
      for (const im of row.details?.images ?? []) {
        const src = attachments.get(String(im.id));
        if (!src) continue;
        attachments.set(uid(), { ...src, sent: null });
        images++;
      }
      for (const rf of row.details?.refs ?? []) {
        let staged = false;
        for (const r of sentRefList.values()) {
          if (r.chatId === c.chat.id && r.kind === rf.kind && r.target === rf.target && r.sent === null) {
            staged = true;
            break;
          }
        }
        if (staged) continue;
        sentRefList.set(uid(), { chatId: c.chat.id, kind: rf.kind, name: rf.name, target: rf.target, sent: null });
        refs++;
      }
      return { images, refs };
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
    chat_warm: () => null,
    chat_roll_over: (a) => {
      const c = find(a.chatId);
      remoteGuard(c);
      if (c.chat.nextSessionId) return { started: true }; // already rolled over
      // The fake's roll over, shaped like the host's: a successor with the same title in the same
      // stack, linked both ways, the old session archived, the hand-off message opening it, and
      // the same open-chat event the real host sends while the reader is still on the old window.
      const s = newChat(c.chat.cwd, c.chat.model ?? undefined, c.chat.effort ?? undefined);
      const stackId = c.chat.stackId ?? c.chat.id;
      c.chat = { ...c.chat, archived: true, nextSessionId: s.chat.id, stackId, status: 'idle' };
      s.chat = { ...s.chat, title: c.chat.title, titleState: 'generated', projectId: c.chat.projectId, stackId, prevSessionId: c.chat.id, started: true };
      addRow(s, { role: 'user', text: 'Carry on where the earlier session left off: scroll up to read it.' });
      // Chats started from the old session now hang under its successor, as the host does.
      const moved: string[] = [];
      for (const k of chats.values()) {
        if (k.chat.parentChatId === c.chat.id) {
          k.chat = { ...k.chat, parentChatId: s.chat.id };
          moved.push(k.chat.id);
        }
      }
      for (const id of moved) emitChat(find(id));
      emitChat(c);
      emitThread(s);
      emitChat(s);
      emit({ kind: 'sidebar', chatIds: [c.chat.id, s.chat.id] });
      emit({ kind: 'shell_open_chat', chatId: s.chat.id, ifViewing: c.chat.id, title: s.chat.title });
      return { started: true };
    },
    // The desktop pet's page (open the dev server with ?window=pet) and the first-run intro.
    pet_ready: () => null,
    pet_moved: (a) => {
      prefs.set('desktopPet', { ...(prefs.get('desktopPet') as object | undefined), position: { x: a.x, y: a.y } });
      return null;
    },
    pet_open: (a) => {
      if (typeof a.chatId === 'string') emit({ kind: 'shell_open_chat', chatId: a.chatId });
      else if (a.chatId === null) emit({ kind: 'shell_open_attention' });
      else if (a.settings === true) emit({ kind: 'shell_open_settings', pane: 'appearance' });
      return null;
    },
    pet_hide: () => {
      const next = { ...(prefs.get('desktopPet') as object | undefined), enabled: false };
      prefs.set('desktopPet', next);
      emit({ kind: 'preferences', changed: { desktopPet: next } });
      return null;
    },
    intro_allowed: () => ({ allowed: true }),
    preference_get: (a) => (a.key === 'sendBehavior' ? sendBehavior : (prefs.get(String(a.key)) ?? null)),
    queue_list: () => fakeQueueList(),
    queue_action: (a) => fakeQueueAct(String(a.chatId), String(a.action), typeof a.beforeChatId === 'string' ? a.beforeChatId : undefined),
    build_queue_list: () => fakeBuildList(),
    build_queue_add: (a) => fakeBuildAct('add', a),
    build_queue_move: (a) => fakeBuildAct('move', a),
    build_queue_pause: (a) => fakeBuildAct('pause', a),
    build_queue_resume: (a) => fakeBuildAct('resume', a),
    build_queue_remove: (a) => fakeBuildAct('remove', a),
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
