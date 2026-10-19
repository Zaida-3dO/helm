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
import { LEGACY_LAST_FOLDER_KEY, MAX_RECENTS, RECENTS_KEY, folderMenu, folderProjectFor, pushRecent, readRecents, resolveFolder, samePath } from './places.ts';

function node(id: string, over: Partial<ProjectNode> = {}): ProjectNode {
  return {
    id,
    kind: 'folder',
    name: id,
    path: null,
    parentId: null,
    parentSource: null,
    icon: null,
    colour: null,
    pinned: false,
    collapsed: false,
    position: null,
    archived: false,
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

function harness(opts: { open?: { id: string; cwd: string; projectId: string } } = {}): Harness {
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
  } as unknown as ChatOps;
  const runtime = { model: null as string | null };
  const api = (async (op: string, args?: unknown) => {
    if (op === 'runtime_settings_get') return { model: runtime.model };
    calls.push({ op, args: [args] });
    const a = (args ?? {}) as { cwd?: string; projectId?: string };
    // The host's folder project is the named one, else "fp:<cwd>".
    if (op === 'chat_create') return chatRecord(`chat-${++n}`, a.cwd!, a.projectId ?? `fp:${a.cwd}`);
    if (op === 'chat_discard_untouched') return { discarded: true };
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
});

test('opening any chat leaves the page', () => {
  const h = harness();
  h.ctl.show();
  h.env.select('some-chat');
  assert.equal(h.ctl.open, false);
  assert.equal(h.ctl.draft, null);
});

test('New chat from an open chat deselects it and starts in that chat’s project and folder', () => {
  const h = harness({ open: { id: 'c1', cwd: 'E:/work', projectId: 'bare' } });
  h.ctl.show();
  assert.equal(h.ctl.open, true, 'the deselect does not count as leaving');
  assert.equal(h.selected.id, null);
  assert.equal(h.ctl.draft?.projectId, 'bare');
  assert.deepEqual(h.ctl.place(), { folder: 'E:/work', source: 'open-chat' });
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
  assert.equal(h.ctl.draft?.projectId, 'helm', 'a folder picks its folder project');
  assert.equal(h.ctl.place().folder, 'c:/code/helm');
});

test('the first send creates the chat, files it under the chosen project, moves the images and opens it', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'group' });
  const ops = draftOps(h.env.ops, h.ctl);
  await ops.setModel(h.ctl.draft!.id, 'opus', undefined);
  await ops.setModel(h.ctl.draft!.id, undefined, 'high');
  const img = await ops.attach(h.ctl.draft!.id, 'shot.png', 'image/png', 'AAAA');
  await ops.saveDraft(h.ctl.draft!.id, 'hello', 3);
  const r = await ops.send(h.ctl.draft!.id, 'sub-1', 'hello', 'default', 3, { attachments: [img.attachmentId] });
  assert.equal(r.submission.id, 'sub-1');
  const seen = h.calls.map((c) => c.op);
  assert.deepEqual(seen, ['chat_create', 'storeChat', 'chat_organize', 'attach', 'send', 'select']);
  // A group is not a folder project, so the host is not told a project: it uses the folder's own.
  assert.deepEqual(h.calls[0]!.args[0], { cwd: 'C:/code/site', model: 'opus', effort: 'high' });
  const chatId = (h.calls[1]!.args[0] as string);
  assert.deepEqual(h.calls[2]!.args[0], { chatId, groupProjectId: 'group', parentChatId: null });
  assert.deepEqual(h.calls[4]!.args, [chatId, 'sub-1', 'hello', ['att-shot.png']]);
  assert.equal(h.selected.id, chatId);
  assert.equal(h.ctl.open, false);
  assert.equal(h.ctl.draft, null);
  assert.equal(h.storage.getItem(LAST_PROJECT_KEY), 'group');
  assert.deepEqual(readRecents(h.storage), ['C:/code/site']);
});

test('a folder project over the folder is named to the host, and the chat is not re-filed', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  await draftOps(h.env.ops, h.ctl).send('x', 's', 'hi', 'default', 1);
  assert.deepEqual(h.calls[0]!.args[0], { cwd: 'C:/code/helm', projectId: 'helm' });
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
