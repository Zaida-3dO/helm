import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Api } from '../api/client.ts';
import { ApiError } from '../api/client.ts';
import type { ChatOps } from '../api/ops.ts';
import type { ProjectNode } from '../api/projects.ts';
import type { ChatRecord } from '../api/types.ts';
import { LAST_PROJECT_KEY, NewChatController, draftOps, type NewChatEnv } from './controller.ts';
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
  let n = 0;
  const ops = {
    create: async (cwd: string, model?: string, effort?: string) => {
      calls.push({ op: 'create', args: [cwd, model, effort] });
      return chatRecord(`chat-${++n}`, cwd, `fp:${cwd}`);
    },
    attach: async (chatId: string, name: string) => {
      calls.push({ op: 'attach', args: [chatId, name] });
      return { attachmentId: `att-${name}` };
    },
    send: async (chatId: string, submissionId: string, text: string, _action: string, _rev: number, extras: { attachments?: string[] }) => {
      calls.push({ op: 'send', args: [chatId, submissionId, text, extras.attachments ?? []] });
      if (failSends.n > 0) {
        failSends.n--;
        throw new ApiError('refused', 'no');
      }
      return { submission: { id: submissionId }, chat: chatRecord(chatId, '', '') };
    },
  } as unknown as ChatOps;
  const api = (async (op: string, args?: unknown) => {
    calls.push({ op, args: [args] });
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
  assert.ok(!h.calls.some((c) => c.op === 'create'), 'nothing was created on the host');
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
  assert.deepEqual(seen, ['create', 'storeChat', 'chat_organize', 'attach', 'send', 'select']);
  assert.deepEqual(h.calls[0]!.args, ['C:/code/site', 'opus', 'high']);
  const chatId = (h.calls[1]!.args[0] as string);
  assert.deepEqual(h.calls[2]!.args[0], { chatId, groupProjectId: 'group', parentChatId: null });
  assert.deepEqual(h.calls[4]!.args, [chatId, 'sub-1', 'hello', ['att-shot.png']]);
  assert.equal(h.selected.id, chatId);
  assert.equal(h.ctl.open, false);
  assert.equal(h.ctl.draft, null);
  assert.equal(h.storage.getItem(LAST_PROJECT_KEY), 'group');
  assert.deepEqual(readRecents(h.storage), ['C:/code/site']);
});

test('the chat is not re-filed when the project is the folder’s own', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  // The fake host names the folder project "fp:<cwd>", so a group that is not it would be filed.
  h.ctl.setProjects([...PROJECTS, node('fp:C:/code/helm', { path: 'C:/code/helm' })]);
  h.ctl.move('fp:C:/code/helm');
  await draftOps(h.env.ops, h.ctl).send('x', 's', 'hi', 'default', 1);
  assert.ok(!h.calls.some((c) => c.op === 'chat_organize'));
});

test('a second press while the first send runs makes no second chat', async () => {
  const h = harness();
  h.ctl.show({ projectId: 'helm' });
  const ops = draftOps(h.env.ops, h.ctl);
  const [a, b] = await Promise.all([ops.send('x', 's1', 'hi', 'default', 1), ops.send('x', 's2', 'hi', 'default', 1)]);
  assert.equal(a, b);
  assert.equal(h.calls.filter((c) => c.op === 'create').length, 1);
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
  assert.equal(h.calls.filter((c) => c.op === 'create').length, 1);
  const sends = h.calls.filter((c) => c.op === 'send');
  assert.equal(sends.length, 2);
  assert.equal(sends[0]!.args[0], sends[1]!.args[0]);
});

test('without any folder the send is refused and nothing is created', async () => {
  const h = harness();
  h.ctl.setProjects([node('bare')]);
  h.ctl.show({ projectId: 'bare' });
  await assert.rejects(draftOps(h.env.ops, h.ctl).send('x', 's', 'hi', 'default', 1), (e: unknown) => e instanceof ApiError && e.code === 'needs_folder');
  assert.ok(!h.calls.some((c) => c.op === 'create'));
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
