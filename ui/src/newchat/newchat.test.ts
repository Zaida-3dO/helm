import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Api } from '../api/client.ts';
import { ApiError } from '../api/client.ts';
import type { ChatOps } from '../api/ops.ts';
import type { ProjectNode } from '../api/projects.ts';
import type { ChatRecord } from '../api/types.ts';
import { LAST_PROJECT_KEY, NewChatController, draftOps, newChat, type NewChatEnv } from './controller.ts';
import { primaryFolderMenuItem } from './menu.ts';
import { addExistingFolder, addNewFolder, currentFolder, setPrimaryFolder } from './projects.ts';
import { ASK_EACH_TIME, NO_DEFAULT_BOARD, createBoardProject, getDefaultBoard, setDefaultBoard } from './boards.ts';
import { LEGACY_LAST_FOLDER_KEY, MAX_RECENTS, RECENTS_KEY, boardFor, sharedFolders, folderMenu, folderProjectFor, pickerProjects, pushRecent, readRecents, resolveFolder, samePath } from './places.ts';

function node(id: string, over: Partial<ProjectNode> = {}): ProjectNode {
  return {
    id,
    kind: 'folder',
    name: id,
    path: null,
    parentId: null,
    parentSource: null,
    description: null,
    icon: null,
    colour: null,
    pinned: false,
    collapsed: false,
    position: null,
    archived: false,
    closed: false,
    board: null,
    primaryFolderProjectId: null,
    chatCount: 0,
    createdAt: 0,
    revision: 0,
    ...over,
  };
}

const PROJECTS: ProjectNode[] = [
  node('helm', { path: 'C:/code/helm' }),
  node('site', { path: 'C:/code/site' }),
  node('group', { primaryFolderProjectId: 'site' }),
  node('bare'),
  node('old', { path: 'C:/code/old', archived: true }),
];

/** A board project; `completed` marks all its work finished on the board. */
function boardNode(id: string, over: Partial<ProjectNode> = {}, info: { completed?: boolean; stale?: boolean } = {}): ProjectNode {
  return node(id, {
    kind: 'board',
    board: { boardProjectId: id, url: '', title: id, headline: null, area: null, repo: null, priority: null, lastActivity: null, stale: info.stale ?? false, completed: info.completed ?? false, editedLocally: false, titlePending: false },
    ...over,
  });
}

/** Boards over the projects above: one over the site folder, a default one over nothing in the
 *  list, and one with no folder yet. */
const BOARDS: ProjectNode[] = [boardNode('b-site', { primaryFolderProjectId: 'site' }), boardNode('b-default', { primaryFolderProjectId: 'elsewhere' }), boardNode('b-bare')];

class MemoryStorage {
  private m = new Map<string, string>();
  getItem(k: string): string | null {
    return this.m.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.m.set(k, v);
  }
}

// ---- places ------------------------------------------------------------------------------------

test('folder resolution: chosen, then the project, its primary, the open chat, the first open folder', () => {
  // A board with no folder yet takes the open chat's folder, but never an unrelated first folder.
  assert.deepEqual(resolveFolder('b-bare', null, [...PROJECTS, ...BOARDS], 'E:/y'), { folder: 'E:/y', source: 'open-chat' });
  assert.deepEqual(resolveFolder('b-bare', null, [...PROJECTS, ...BOARDS], null), { folder: null, source: null });
  assert.deepEqual(resolveFolder('b-site', null, [...PROJECTS, ...BOARDS], null), { folder: 'C:/code/site', source: 'primary' });
  assert.deepEqual(resolveFolder('helm', 'D:/x', PROJECTS, 'E:/y'), { folder: 'D:/x', source: 'chosen' });
  assert.deepEqual(resolveFolder('helm', null, PROJECTS, 'E:/y'), { folder: 'C:/code/helm', source: 'project' });
  assert.deepEqual(resolveFolder('group', null, PROJECTS, 'E:/y'), { folder: 'C:/code/site', source: 'primary' });
  assert.deepEqual(resolveFolder('bare', null, PROJECTS, 'E:/y'), { folder: 'E:/y', source: 'open-chat' });
  assert.deepEqual(resolveFolder('bare', null, PROJECTS, null), { folder: 'C:/code/helm', source: 'first-open' });
  // An archived project's folder is skipped.
  assert.deepEqual(resolveFolder('old', null, PROJECTS, null), { folder: 'C:/code/helm', source: 'first-open' });
  assert.deepEqual(resolveFolder(null, null, [node('bare')], null), { folder: null, source: null });
});

test('the chat goes into the chosen project only when it is a folder project over that exact folder', () => {
  assert.equal(folderProjectFor('helm', 'c:\\code\\HELM\\', PROJECTS), 'helm');
  assert.equal(folderProjectFor('helm', 'C:/code/site', PROJECTS), null);
  assert.equal(folderProjectFor('group', 'C:/code/site', PROJECTS), null);
  assert.equal(folderProjectFor('old', 'C:/code/old', PROJECTS), null);
  assert.equal(folderProjectFor(null, 'C:/code/helm', PROJECTS), null);
  assert.ok(samePath('C:\\a\\b', 'c:/A/B/'));
  assert.ok(!samePath('C:/a/b', 'C:/a/bc'));
});

test('recent folders: seeded once from the old path box, newest first, no duplicates, capped', () => {
  const s = new MemoryStorage();
  assert.deepEqual(readRecents(s), []);
  s.setItem(LEGACY_LAST_FOLDER_KEY, ' C:/legacy ');
  assert.deepEqual(readRecents(s), ['C:/legacy']);
  pushRecent(s, 'C:/a');
  assert.deepEqual(readRecents(s), ['C:/a', 'C:/legacy']);
  pushRecent(s, 'c:\\LEGACY');
  assert.deepEqual(readRecents(s), ['c:\\LEGACY', 'C:/a']);
  for (let i = 0; i < 20; i++) pushRecent(s, `C:/f${i}`);
  const r = readRecents(s);
  assert.equal(r.length, MAX_RECENTS);
  assert.equal(r[0], 'C:/f19');
  s.setItem(RECENTS_KEY, '{not json');
  assert.deepEqual(readRecents(s), ['C:/legacy']);
});

test('the folder menu lists the project folder first, then open folders, then recent ones not already listed', () => {
  const m = folderMenu(PROJECTS, PROJECTS.find((p) => p.id === 'group'), ['C:/code/helm', 'D:/elsewhere']);
  assert.deepEqual(
    m.open.map((o) => o.path),
    ['C:/code/site', 'C:/code/helm'],
  );
  assert.deepEqual(m.recent, ['D:/elsewhere']);
});

// ---- the controller ------------------------------------------------------------------------------

interface Harness {
  ctl: NewChatController;
  env: NewChatEnv;
  calls: { op: string; args: unknown[] }[];
  selected: { id: string | null };
  storage: MemoryStorage;
  fireSelection: () => void;
  failSends: { n: number };
  /** While set, sends wait for it. */
  gate: { wait: Promise<void> | null };
  /** The Runtime default model the host answers with (null: Claude Code's default). */
  runtime: { model: string | null };
}

function chatRecord(id: string, cwd: string, projectId: string): ChatRecord {
  return { id, projectId, cwd } as ChatRecord;
}

function harness(opts: { open?: { id: string; cwd: string; projectId: string }; groups?: Record<string, string> } = {}): Harness {
  const calls: { op: string; args: unknown[] }[] = [];
  const selected = { id: opts.open?.id ?? null };
  const storage = new MemoryStorage();
  const listeners = new Set<() => void>();
  const failSends = { n: 0 };
  const gate: { wait: Promise<void> | null } = { wait: null };
  let n = 0;
  const ops = {
    attach: async (chatId: string, name: string) => {
      calls.push({ op: 'attach', args: [chatId, name] });
      return { attachmentId: `att-${name}` };
    },
    send: async (chatId: string, submissionId: string, text: string, _action: string, _rev: number, extras: { attachments?: string[] }) => {
      calls.push({ op: 'send', args: [chatId, submissionId, text, extras.attachments ?? []] });
      if (gate.wait) await gate.wait;
      if (failSends.n > 0) {
        failSends.n--;
        throw new ApiError('refused', 'no');
      }
      return { submission: { id: submissionId }, chat: chatRecord(chatId, '', '') };
    },
    attachRef: async (chatId: string, kind: string, target: string) => {
      calls.push({ op: 'attachRef', args: [chatId, kind, target] });
      return { attachmentId: `ref-${target}`, kind, name: target };
    },
  } as unknown as ChatOps;
  const runtime = { model: null as string | null };
  const api = (async (op: string, args?: unknown) => {
    if (op === 'runtime_settings_get') return { model: runtime.model };
    calls.push({ op, args: [args] });
    const a = (args ?? {}) as { cwd?: string; projectId?: string };
    // The host's folder project is the named one, else "fp:<cwd>".
    if (op === 'chat_create') return chatRecord(`chat-${++n}`, a.cwd!, a.projectId ?? `fp:${a.cwd}`);
    if (op === 'chat_discard_untouched') return { discarded: true };
    if (op === 'devices_create_chat') return { chatId: 'remote-1' };
    if (op === 'sidebar_rows') return Object.entries(opts.groups ?? {}).map(([chatId, groupProjectId]) => ({ chatId, groupProjectId }));
    return null;
  }) as Api;
  const env: NewChatEnv = {
    api,
    ops,
    selected: () => selected.id,
    select: (id) => {
      calls.push({ op: 'select', args: [id] });
      selected.id = id;
      for (const l of [...listeners]) l();
    },
    onSelection: (f) => {
      listeners.add(f);
      return () => listeners.delete(f);
    },
    chatPlace: (id) => (opts.open && id === opts.open.id ? { cwd: opts.open.cwd, projectId: opts.open.projectId } : null),
    storeChat: (c) => calls.push({ op: 'storeChat', args: [c.id] }),
    storage,
    now: () => 1000,
    uid: () => `u${++n}`,
  };
  const ctl = new NewChatController();
  ctl.setProjects(PROJECTS);
  ctl.attach(env);
  return {
    ctl,
    env,
    calls,
    selected,
    storage,
    failSends,
    gate,
    runtime,
    fireSelection: () => {
      for (const l of [...listeners]) l();
    },
  };
}

test('an untouched draft is discarded on leaving; a typed one is kept and reused by the next New chat', () => {
  const h = harness();
  h.ctl.show();
  assert.equal(h.ctl.open, true);
  const first = h.ctl.draft!.id;
  h.ctl.leave();
  assert.equal(h.ctl.open, false);
  assert.equal(h.ctl.draft, null, 'untouched: nothing left behind');

  // (Read through a function: the assertion above narrows the field to null for the compiler.)
  const draft = () => h.ctl.draft;
  h.ctl.show();
  const second = draft()!.id;
  assert.notEqual(second, first);
  h.ctl.saveText('fix the build', 5);
  h.ctl.leave();
  assert.equal(draft()?.id, second, 'typed: kept');
  h.ctl.show();
  assert.equal(draft()?.id, second);
  assert.equal(h.ctl.record()?.draft, 'fix the build');
  assert.ok(!h.calls.some((c) => c.op === 'chat_create'), 'nothing was created on the host');

  // Text handed to New chat goes into the draft, after a blank line when the draft already has text,
  // with a newer revision so an open composer takes it. Nothing is sent.
  h.ctl.show({ text: 'from a pull request' });
  assert.equal(draft()?.text, 'fix the build\n\nfrom a pull request\n');
  assert.ok((draft()?.revision ?? 0) > 5);
  h.ctl.leave();
  draft()!.text = '';
  h.ctl.show({ text: 'only this' });
  assert.equal(draft()?.text, 'only this\n');
  assert.ok(!h.calls.some((c) => c.op === 'send'));
});

test('opening any chat leaves the page', () => {
  const h = harness();
  h.ctl.show();
  h.env.select('some-chat');
  assert.equal(h.ctl.open, false);
  assert.equal(h.ctl.draft, null);
});

test('New chat from an open chat deselects it and starts on that chat’s board, in its folder', async () => {
  const h = harness({ open: { id: 'c1', cwd: 'E:/work', projectId: 'bare' } });
  h.ctl.show();
  assert.equal(h.ctl.open, true, 'the deselect does not count as leaving');
  assert.equal(h.selected.id, null);
  assert.equal(h.ctl.draft?.projectId, null, 'a folder project is not a board');
  assert.deepEqual(h.ctl.place(), { folder: 'E:/work', source: 'open-chat' });

  // The board the open chat is shown on, once the host says which it is.
  const b = harness({ open: { id: 'c1', cwd: 'C:/code/site', projectId: 'site' }, groups: { c1: 'b-default' } });
  b.ctl.setProjects([...PROJECTS, ...BOARDS]);
  b.ctl.show();
  assert.equal(b.ctl.draft?.projectId, 'b-site', 'at first the board over its folder');
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(b.ctl.draft?.projectId, 'b-default', 'then the board it is really on');
});

test('which board a new chat goes on: the one picked, the board over its folder, else the default; a board with no folder asks for one', async () => {
  const projects = [...PROJECTS, ...BOARDS];
  const cases: [Parameters<NewChatController['show']>[0], string | null, string | null][] = [
    [{ projectId: 'b-site' }, 'b-site', 'C:/code/site'],
    [{ projectId: 'site' }, 'b-site', 'C:/code/site'], // a folder project stands for the board over it
    [{ folder: 'c:/code/site' }, 'b-site', 'c:/code/site'],
    [{ projectId: 'helm' }, 'b-default', 'C:/code/helm'], // no board over it: the default board
    [{}, 'b-default', 'C:/code/helm'],
  ];
  for (const [open, board, folder] of cases) {
    const h = harness();
    h.ctl.setProjects(projects);
    h.ctl.setBoards({ ...NO_DEFAULT_BOARD, boardId: 'b-default', name: 'Default' });
    h.ctl.show(open);
    assert.deepEqual([h.ctl.board(), h.ctl.place().folder, h.ctl.dialog], [board, folder, null], JSON.stringify(open));
  }
  // Without a default board and with no board over the folder, none is named: the host decides.
  const none = harness();
  none.ctl.show({ projectId: 'helm' });
  assert.equal(none.ctl.board(), null);

  const h = harness();
  h.ctl.setProjects(projects);
  h.ctl.setBoards({ ...NO_DEFAULT_BOARD, boardId: 'b-default', name: 'Default' });
  h.ctl.show({ projectId: 'b-bare' });
  assert.deepEqual(h.ctl.dialog, { kind: 'primary-folder', projectId: 'b-bare' });
  assert.deepEqual(h.ctl.place(), { folder: null, source: null }, 'no folder until one is set');
  h.ctl.closeDialog();
  h.ctl.move('helm');
  await draftOps(h.env.ops, h.ctl).send('x', 's', 'hi', 'default', 1);
  assert.deepEqual(h.calls.find((c) => c.op === 'chat_create')!.args[0], { cwd: 'C:/code/helm', projectId: 'helm', boardId: 'b-default', startInLastMode: true });
  assert.equal(h.storage.getItem(LAST_PROJECT_KEY), 'b-default');

  // "Ask each time": a new draft starts with nothing chosen (the last project is not reloaded) and
  // the default is no fallback; a pick or the board over the folder still counts, and a send needs one.
  const asking = harness();
  asking.storage.setItem(LAST_PROJECT_KEY, 'b-site');
  asking.ctl.setProjects(projects);
  asking.ctl.setBoards({ ...NO_DEFAULT_BOARD, boardId: 'b-default', name: 'Default', ask: true });
  asking.ctl.show();
  assert.deepEqual([asking.ctl.draft?.projectId, asking.ctl.board()], [null, null]);
  asking.ctl.setFolder('C:/code/helm');
  await assert.rejects(draftOps(asking.env.ops, asking.ctl).send('x', 's', 'hi', 'default', 1), /Choose a project/);
  assert.equal(asking.calls.some((c) => c.op === 'chat_create'), false, 'nothing is made before a project is picked');
  asking.ctl.move('b-site');
  assert.equal(asking.ctl.board(), 'b-site');
  asking.ctl.setFolder('c:/code/site');
  assert.equal(asking.ctl.board(), 'b-site');

  // boardFor: one open, unfinished board over a folder, never two, never a stale or archived one.
  const twice = [...projects, boardNode('b-site2', { primaryFolderProjectId: 'site' })];
  assert.equal(boardFor('site', twice), null);
  assert.equal(boardFor('site', [...PROJECTS, boardNode('b-site', { primaryFolderProjectId: 'site' }, { stale: true })]), null);
  assert.equal(boardFor('b-site', [...PROJECTS, boardNode('b-site', { archived: true })]), null);
  assert.equal(boardFor('b-site', [...PROJECTS, boardNode('b-site', { closed: true })]), null, 'a closed board is never chosen');

  // sharedFolders: open boards over one folder are named; closed ones and lone boards are not.
  const names = (list: ProjectNode[]) => sharedFolders(list).map((g) => g.boards.map((b) => b.id));
  assert.deepEqual(names(twice), [['b-site', 'b-site2']]);
  assert.deepEqual(names([...projects, boardNode('b-site2', { primaryFolderProjectId: 'site', closed: true })]), []);
  assert.deepEqual(names(projects), []);
});

test('New chat in a project moves a typed draft there, keeping the text; the folder follows the project', () => {
  const h = harness();
  h.ctl.show();
  h.ctl.setFolder('D:/chosen');
  h.ctl.saveText('keep me', 2);
  h.ctl.leave();
  h.ctl.show({ projectId: 'site' });
  assert.equal(h.ctl.draft?.projectId, 'site');
  assert.equal(h.ctl.draft?.text, 'keep me');
  assert.deepEqual(h.ctl.place(), { folder: 'C:/code/site', source: 'project' });
  h.ctl.show({ folder: 'c:/code/helm' });
  assert.equal(h.ctl.draft?.projectId, null, 'a folder with no board over it leaves the board to follow the folder');
  assert.equal(h.ctl.place().folder, 'c:/code/helm');
  h.ctl.setProjects([...PROJECTS, ...BOARDS]);
  h.ctl.show({ folder: 'c:/code/site' });
  assert.equal(h.ctl.draft?.projectId, 'b-site', 'a folder with a board over it picks that board');
  assert.equal(h.ctl.place().folder, 'c:/code/site');
});

test('the first send creates the chat on the chosen board, moves the images and opens it', async () => {
  const h = harness();
  h.ctl.setProjects([...PROJECTS, ...BOARDS]);
  h.ctl.show({ projectId: 'b-site' });
  const ops = draftOps(h.env.ops, h.ctl);
  await ops.setModel(h.ctl.draft!.id, 'opus', undefined);
  await ops.setModel(h.ctl.draft!.id, undefined, 'high');
  const img = await ops.attach(h.ctl.draft!.id, 'shot.png', 'image/png', 'AAAA');
  await ops.saveDraft(h.ctl.draft!.id, 'hello', 3);
  const r = await ops.send(h.ctl.draft!.id, 'sub-1', 'hello', 'default', 3, { attachments: [img.attachmentId] });
  assert.equal(r.submission.id, 'sub-1');
  const seen = h.calls.map((c) => c.op);
  assert.deepEqual(seen, ['chat_create', 'storeChat', 'attach', 'send', 'select']);
  // A board is not a folder project, so the host is told the board and finds the folder's own project.
  assert.deepEqual(h.calls[0]!.args[0], { cwd: 'C:/code/site', boardId: 'b-site', model: 'opus', effort: 'high', startInLastMode: true });
  const chatId = (h.calls[1]!.args[0] as string);
  assert.deepEqual(h.calls[3]!.args, [chatId, 'sub-1', 'hello', ['att-shot.png']]);
  assert.equal(h.selected.id, chatId);
  assert.equal(h.ctl.open, false);
  assert.equal(h.ctl.draft, null);
  assert.equal(h.storage.getItem(LAST_PROJECT_KEY), 'b-site');
  assert.deepEqual(readRecents(h.storage), ['C:/code/site']);
});

test('a chat or file dragged onto a draft is kept and attached to the chat the first send makes', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  const ops = draftOps(h.env.ops, h.ctl);
  const id = h.ctl.draft!.id;
  const chat = await ops.attachRef(id, 'chat', 'other-chat');
  const file = await ops.attachRef(id, 'path', 'C:\\work\\notes.txt');
  assert.deepEqual([chat.kind, chat.name, file.kind, file.name], ['chat', 'Chat', 'file', 'notes.txt']);
  assert.equal((await ops.attachRef(id, 'chat', 'other-chat')).attachmentId, chat.attachmentId, 'the same chat twice is one reference');
  await assert.rejects(ops.attachRef(id, 'path', 'relative/file.txt'), /absolute/);
  assert.equal(h.ctl.untouched(), false);
  await ops.discardAttachment(id, file.attachmentId);
  await ops.send(id, 'sub-r', 'hi', 'default', 1, { attachments: [chat.attachmentId] });
  assert.ok(!h.calls.some((c) => c.op === 'attachRef' && c.args[1] === 'path'), 'a discarded reference is not attached');
  assert.deepEqual(h.calls.find((c) => c.op === 'attachRef')!.args.slice(1), ['chat', 'other-chat']);
  assert.deepEqual(h.calls.find((c) => c.op === 'send')!.args.slice(2), ['hi', ['ref-other-chat']]);
});

test('a folder project over the folder is named to the host, and the chat is not re-filed', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  await draftOps(h.env.ops, h.ctl).send('x', 's', 'hi', 'default', 1);
  assert.deepEqual(h.calls[0]!.args[0], { cwd: 'C:/code/helm', projectId: 'helm', startInLastMode: true });
  assert.ok(!h.calls.some((c) => c.op === 'chat_organize'));
});

test('a second press while the first send runs makes no second chat', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  const ops = draftOps(h.env.ops, h.ctl);
  const [a, b] = await Promise.all([ops.send('x', 's1', 'hi', 'default', 1), ops.send('x', 's2', 'hi', 'default', 1)]);
  assert.equal(a, b);
  assert.equal(h.calls.filter((c) => c.op === 'chat_create').length, 1);
});

test('a refused send keeps the draft open, and the retry sends to the chat already made', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  const ops = draftOps(h.env.ops, h.ctl);
  h.failSends.n = 1;
  await assert.rejects(ops.send('x', 's1', 'hi', 'default', 1), /no/);
  assert.equal(h.ctl.open, true);
  assert.ok(h.ctl.draft);
  await ops.send('x', 's2', 'hi', 'default', 1);
  assert.equal(h.calls.filter((c) => c.op === 'chat_create').length, 1);
  const sends = h.calls.filter((c) => c.op === 'send');
  assert.equal(sends.length, 2);
  assert.equal(sends[0]!.args[0], sends[1]!.args[0]);
});

test('aimed at another device: its folder projects, its own model choice, create there then send text, a retry reuses the chat', async () => {
  const h = harness();
  h.ctl.setDevice({ uid: 'lap', name: 'Laptop' });
  h.ctl.show();
  // Only folders can hold a chat; the project used last time on that device comes back first.
  h.storage.setItem('helm.newchat.remoteProject.lap', 'p2');
  h.ctl.setDeviceProjects([
    { id: 'b1', name: 'Board', colour: null, kind: 'board' },
    { id: 'p1', name: 'Shop', colour: null, kind: 'folder' },
    { id: 'p2', name: 'Notes', colour: null, kind: 'folder' },
  ]);
  assert.deepEqual(h.ctl.deviceProjects.map((p) => p.id), ['p1', 'p2']);
  assert.equal(h.ctl.remoteProjectId, 'p2');
  const ops = draftOps(h.env.ops, h.ctl);
  // The model chosen is the device's own and never touches this PC's draft model.
  await ops.setModel(h.ctl.draft!.id, 'claude-haiku-5', 'low');
  assert.deepEqual([h.ctl.record()!.model, h.ctl.record()!.effort, h.ctl.record()!.hostDeviceId, h.ctl.draft!.model], ['claude-haiku-5', 'low', 'lap', null]);
  h.failSends.n = 1;
  await assert.rejects(ops.send('x', 's1', 'hi', 'default', 1, { attachments: ['ignored'] }), /no/);
  assert.equal(h.ctl.open, true);
  await ops.send('x', 's2', 'hi', 'default', 1);
  const made = h.calls.filter((c) => c.op === 'devices_create_chat');
  assert.equal(made.length, 1);
  assert.deepEqual(made[0]!.args[0], { deviceUid: 'lap', projectId: 'p2', model: 'claude-haiku-5', effort: 'low' });
  assert.equal(h.calls.some((c) => c.op === 'chat_create'), false);
  const sends = h.calls.filter((c) => c.op === 'send');
  assert.deepEqual(sends.map((s) => s.args), [['remote-1', 's1', 'hi', []], ['remote-1', 's2', 'hi', []]]);
  assert.equal(h.selected.id, 'remote-1');
  assert.equal(h.storage.getItem('helm.newchat.remoteProject.lap'), 'p2');
  // Back on this PC the remote choices are gone.
  h.ctl.setDevice(null);
  assert.deepEqual([h.ctl.device, h.ctl.deviceProjects, h.ctl.remoteProjectId], [null, [], null]);
});

test('without any folder the send is refused and nothing is created', async () => {
  const h = harness();
  h.ctl.setProjects([node('bare')]);
  h.ctl.show({ projectId: 'bare' });
  await assert.rejects(draftOps(h.env.ops, h.ctl).send('x', 's', 'hi', 'default', 1), (e: unknown) => e instanceof ApiError && e.code === 'needs_folder');
  assert.ok(!h.calls.some((c) => c.op === 'chat_create'));
});

test('a refused send then a retry uploads each pasted image once', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  const ops = draftOps(h.env.ops, h.ctl);
  const img = await ops.attach('x', 'shot.png', 'image/png', 'AAAA');
  h.failSends.n = 1;
  await assert.rejects(ops.send('x', 's1', 'hi', 'default', 1, { attachments: [img.attachmentId] }));
  await ops.send('x', 's2', 'hi', 'default', 1, { attachments: [img.attachmentId] });
  assert.equal(h.calls.filter((c) => c.op === 'attach').length, 1);
  const sends = h.calls.filter((c) => c.op === 'send');
  assert.deepEqual(sends[1]!.args[3], ['att-shot.png']);
});

test('the chat a refused send made is discarded when the page is left or moved elsewhere', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  const ops = draftOps(h.env.ops, h.ctl);
  await ops.saveDraft('x', 'typed', 5);
  h.failSends.n = 1;
  await assert.rejects(ops.send('x', 's1', 'typed', 'default', 1));
  const made = h.calls.find((c) => c.op === 'storeChat')!.args[0];
  h.ctl.leave();
  assert.deepEqual(
    h.calls.filter((c) => c.op === 'chat_discard_untouched').map((c) => c.args[0]),
    [{ chatId: made }],
  );
  assert.equal(h.ctl.draft?.text, 'typed', 'the typed draft stays');

  // Moving to another project drops the made chat too, and the next send makes a new one there.
  h.ctl.show();
  h.failSends.n = 1;
  await assert.rejects(ops.send('x', 's2', 'typed', 'default', 1));
  h.ctl.move('site');
  assert.equal(h.calls.filter((c) => c.op === 'chat_discard_untouched').length, 2);
  await ops.send('x', 's3', 'typed', 'default', 1);
  const creates = h.calls.filter((c) => c.op === 'chat_create').map((c) => (c.args[0] as { cwd: string }).cwd);
  assert.deepEqual(creates, ['C:/code/helm', 'C:/code/helm', 'C:/code/site']);
});

test('leaving forgets pasted images, so a draft with only an image is discarded', async () => {
  const h = harness();
  h.ctl.show();
  await draftOps(h.env.ops, h.ctl).attach('x', 'shot.png', 'image/png', 'AAAA');
  assert.equal(h.ctl.untouched(), false);
  h.ctl.leave();
  assert.equal(h.ctl.draft, null);
});

test('draft text saves only forward; a model change alone leaves the draft untouched', async () => {
  const h = harness();
  h.ctl.show();
  const ops = draftOps(h.env.ops, h.ctl);
  await ops.saveDraft('x', 'newer', 10);
  await ops.saveDraft('x', 'older', 9);
  assert.equal(h.ctl.draft?.text, 'newer');
  await ops.saveDraft('x', '', 11);
  const rec = await ops.setModel('x', 'sonnet');
  assert.equal(rec.model, 'sonnet');
  assert.equal(h.ctl.untouched(), true);
  h.ctl.leave();
  assert.equal(h.ctl.draft, null);
});

test('an open before the app connects is applied on connect; the ready hook hears the place', () => {
  const ctl = new NewChatController();
  ctl.setProjects(PROJECTS);
  ctl.show({ projectId: 'site' });
  assert.equal(ctl.open, false);
  const heard: unknown[] = [];
  ctl.onReady((p) => heard.push(p));
  const h = harness();
  ctl.attach(h.env);
  assert.equal(ctl.open, true);
  assert.deepEqual(heard, [{ projectId: 'site', folder: 'C:/code/site' }]);
});

// ---- Add project, Primary folder…, the menu seam ---------------------------------------------

function recordingApi(answers: Record<string, (args: Record<string, unknown>) => unknown> = {}) {
  const calls: { op: string; args: Record<string, unknown> }[] = [];
  const api = (async (op: string, args?: unknown) => {
    const a = (args ?? {}) as Record<string, unknown>;
    calls.push({ op, args: a });
    return answers[op]?.(a) ?? null;
  }) as Api;
  return { api, calls };
}

test('New project makes a board project, leaving empty fields out; the default board reads safely', async () => {
  const { api, calls } = recordingApi({ board_project_create: (a) => boardNode('new', { name: String(a.title) }), default_board_set: (a) => ({ boardId: a.boardId, name: 'X', chosen: a.boardId, ask: a.boardId === 'ask', hiddenBoardIds: ['inbox'] }) });
  await createBoardProject(api, { title: ' Garden plan ', description: '  ', folder: ' C:/code/garden ' });
  await createBoardProject(api, { title: 'Notes', description: ' What it is for. ' });
  assert.deepEqual(await setDefaultBoard(api, 'b1'), { boardId: 'b1', name: 'X', chosen: 'b1', ask: false, hiddenBoardIds: ['inbox'] });
  assert.equal((await setDefaultBoard(api, ASK_EACH_TIME)).ask, true);
  assert.deepEqual(calls, [
    { op: 'board_project_create', args: { title: 'Garden plan', folder: 'C:/code/garden' } },
    { op: 'board_project_create', args: { title: 'Notes', description: 'What it is for.' } },
    { op: 'default_board_set', args: { boardId: 'b1' } },
    { op: 'default_board_set', args: { boardId: 'ask' } },
  ]);
  // An older host, or no board yet: no default, nothing hidden.
  const refusing = (async () => {
    throw new ApiError('not_implemented', 'no');
  }) as Api;
  assert.deepEqual(await getDefaultBoard(refusing), NO_DEFAULT_BOARD);
  assert.deepEqual(await getDefaultBoard((async () => ({ boardId: 'b' })) as Api), { ...NO_DEFAULT_BOARD, boardId: 'b' });
});

test('Add project: an existing folder is named after it unless named; a new folder goes to the host', async () => {
  const { api, calls } = recordingApi({ project_create: (a) => node('new', { name: String(a.name), path: String(a.path) }) });
  await addExistingFolder(api, ' C:\\code\\site\\ ');
  await addExistingFolder(api, 'C:/code/site', '  Website ');
  await addNewFolder(api, ' fresh ', null);
  await addNewFolder(api, 'fresh', 'D:/repos');
  assert.deepEqual(calls, [
    { op: 'project_create', args: { name: 'site', path: 'C:\\code\\site\\' } },
    { op: 'project_create', args: { name: 'Website', path: 'C:/code/site' } },
    { op: 'project_create_folder', args: { name: 'fresh' } },
    { op: 'project_create_folder', args: { name: 'fresh', parent: 'D:/repos' } },
  ]);
});

test('Primary folder: a folder project takes the path; a board project points at a folder project, reusing an open one', async () => {
  const board = node('b', { kind: 'board', revision: 7 });
  const helm = PROJECTS.find((p) => p.id === 'helm')!;
  let r = recordingApi();
  await setPrimaryFolder(r.api, { ...helm, revision: 3 }, ' D:/moved ', PROJECTS);
  assert.deepEqual(r.calls, [{ op: 'project_update', args: { projectId: 'helm', revision: 3, path: 'D:/moved' } }]);

  r = recordingApi();
  await setPrimaryFolder(r.api, board, 'c:\\code\\SITE', PROJECTS);
  assert.deepEqual(r.calls, [{ op: 'project_update', args: { projectId: 'b', revision: 7, primaryFolderProjectId: 'site' } }]);

  r = recordingApi({ project_create: () => node('made', { path: 'E:/elsewhere' }) });
  await setPrimaryFolder(r.api, board, 'E:/elsewhere', PROJECTS);
  assert.deepEqual(
    r.calls.map((c) => c.op),
    ['project_create', 'project_update'],
  );
  assert.equal(r.calls[1]!.args.primaryFolderProjectId, 'made');
  // An archived project over the folder is not reused.
  r = recordingApi({ project_create: () => node('made2', { path: 'C:/code/old' }) });
  await setPrimaryFolder(r.api, board, 'C:/code/old', PROJECTS);
  assert.equal(r.calls[1]!.args.primaryFolderProjectId, 'made2');
  assert.equal(currentFolder(PROJECTS.find((p) => p.id === 'group')!, PROJECTS), 'C:/code/site');
});

test('the menu item opens the Primary folder dialog for its project and is off for an archived one', () => {
  const item = primaryFolderMenuItem(PROJECTS[0]!);
  assert.equal(item.label, 'Primary folder…');
  assert.equal(item.disabled, false);
  item.onSelect!();
  assert.deepEqual(newChat.dialog, { kind: 'primary-folder', projectId: 'helm' });
  newChat.closeDialog();
  assert.equal(newChat.dialog, null);
  assert.equal(primaryFolderMenuItem(PROJECTS.find((p) => p.id === 'old')!).disabled, true);
});

test('the project picker lists open boards only, keeps the draft’s own, leaves the hidden ones out and brings finished ones back on request', () => {
  const list = [node('a', { path: 'C:/a' }), boardNode('open'), boardNode('done', {}, { completed: true }), boardNode('done-stale', {}, { completed: true, stale: true }), boardNode('gone', { archived: true }), boardNode('inbox')];
  const ids = (l: ProjectNode[]) => l.map((p) => p.id);
  const cases: [boolean, string | null, string[], string[]][] = [
    [false, null, [], ['open', 'inbox']],
    [false, null, ['inbox'], ['open']],
    [true, null, ['inbox'], ['open', 'done']],
    [false, 'done', ['inbox'], ['open', 'done']],
    [false, 'gone', ['inbox'], ['open', 'gone']],
    [false, 'a', ['inbox'], ['a', 'open']], // a chat still under a folder keeps it as its choice
    [false, 'inbox', ['inbox'], ['open', 'inbox']],
  ];
  for (const [completed, current, hidden, want] of cases) assert.deepEqual(ids(pickerProjects(list, completed, current, hidden)), want, `${completed} ${current} ${hidden}`);

  // New project makes a board project when a task board is connected; without one it adds a folder
  // and starts a chat there (Settings asks for no chat).
  const np: [boolean, Parameters<NewChatController['newProject']>[0], unknown][] = [
    [true, { title: ' Brand new idea ' }, { kind: 'new-project', title: 'Brand new idea' }],
    [true, { noChat: true }, { kind: 'new-project', noChat: true }],
    [false, { title: 'Brand new idea' }, { kind: 'add-project', newName: 'Brand new idea', startChat: true }],
    [false, {}, { kind: 'add-project', startChat: true }],
    [false, { noChat: true }, { kind: 'add-project' }],
  ];
  for (const [connected, o, want] of np) {
    newChat.setBoardConnected(connected);
    newChat.newProject(o);
    assert.deepEqual(newChat.dialog, want, `${connected} ${JSON.stringify(o)}`);
    newChat.closeDialog();
  }
  newChat.setBoardConnected(false);
});

test('opening the chat a refused first send made does not discard it; a running send keeps its chat until it is refused', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  const ops = draftOps(h.env.ops, h.ctl);
  await ops.saveDraft('x', 'typed', 5);
  h.failSends.n = 1;
  await assert.rejects(ops.send('x', 's1', 'typed', 'default', 1));
  const made = h.calls.find((c) => c.op === 'storeChat')!.args[0] as string;
  h.env.select(made);
  assert.equal(h.ctl.open, false);
  assert.ok(!h.calls.some((c) => c.op === 'chat_discard_untouched'), 'the opened chat is kept');

  // While a first send runs (the chat is made, the send not answered yet), leaving does not
  // discard the chat it is sending to.
  const h2 = harness();
  h2.ctl.show({ projectId: 'helm' });
  const ops2 = draftOps(h2.env.ops, h2.ctl);
  let release = () => {};
  h2.gate.wait = new Promise<void>((r) => (release = r));
  h2.failSends.n = 1;
  const sending = ops2.send('x', 's1', 'hi', 'default', 1);
  while (!h2.calls.some((c) => c.op === 'send')) await new Promise((r) => setImmediate(r));
  h2.ctl.leave();
  release();
  assert.ok(!h2.calls.some((c) => c.op === 'chat_discard_untouched'), 'kept while the send runs');
  await assert.rejects(sending);
  // ...and once that send is refused with the page already left, the empty chat is discarded.
  assert.ok(h2.calls.some((c) => c.op === 'chat_discard_untouched'));
});

test('a new draft shows the Runtime default model, and a model the user picked first is kept', async () => {
  const h = harness();
  h.runtime.model = 'claude-test-model';
  h.ctl.show();
  assert.equal(h.ctl.draft!.model, null);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(h.ctl.draft!.model, 'claude-test-model');
  assert.equal(h.ctl.record()!.model, 'claude-test-model');

  const g = harness();
  g.runtime.model = 'claude-test-model';
  g.ctl.show();
  g.ctl.setModel('picked');
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(g.ctl.draft!.model, 'picked');
});

test('typed text survives a restart, and a sent or emptied draft leaves none behind', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  await draftOps(h.env.ops, h.ctl).saveDraft('x', 'half a thought', 3);
  // A fresh controller on the same storage (an app restart) opens with the text.
  const again = new NewChatController();
  again.setProjects(PROJECTS);
  again.attach(h.env);
  again.show({ projectId: 'helm' });
  assert.equal(again.draft!.text, 'half a thought');
  assert.equal(again.record()!.draft, 'half a thought');
  assert.equal(again.untouched(), false);

  await draftOps(h.env.ops, h.ctl).send('x', 's1', 'half a thought', 'default', 4);
  const third = new NewChatController();
  third.attach(h.env);
  third.show();
  assert.equal(third.draft!.text, '');
});

test('a chat made for a board item gets its details in the draft once, and is linked after the first send', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm', text: 'Task block id-1', itemId: 'id-1', itemTitle: 'Fix it' });
  assert.equal(h.ctl.draft!.text, 'Task block id-1\n');
  assert.equal(h.ctl.record()!.draftRevision, 1, 'the composer is told the draft changed');
  // Asking again for the same item adds nothing; other typed text stays, with the block after it.
  h.ctl.show({ text: 'Task block id-1', itemId: 'id-1' });
  assert.equal(h.ctl.draft!.text, 'Task block id-1\n');
  const other = harness();
  other.ctl.show({ projectId: 'helm' });
  await draftOps(other.env.ops, other.ctl).saveDraft('x', 'typed first', 3);
  other.ctl.show({ text: 'Block id-2', itemId: 'id-2' });
  assert.equal(other.ctl.draft!.text, 'typed first\n\nBlock id-2\n');

  const ops = draftOps(h.env.ops, h.ctl);
  await ops.send(h.ctl.draft!.id, 'sub-1', 'go', 'default', 3);
  const chatId = h.calls.find((c) => c.op === 'chat_create') && (h.calls.find((c) => c.op === 'storeChat')!.args[0] as string);
  assert.deepEqual(h.calls.map((c) => c.op), ['chat_create', 'storeChat', 'send', 'item_link_add', 'select']);
  assert.deepEqual(h.calls.find((c) => c.op === 'item_link_add')!.args[0], { itemId: 'id-1', chatId, title: 'Fix it' });
});
