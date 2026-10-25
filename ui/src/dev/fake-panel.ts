// Dev fake for the side panel ops (file viewer, default handler, saved tabs) so `vite` in a plain
// browser exercises the side panel without the host. A small made-up project lives in memory under
// C:\Demo\project. fakeHost.ts registers `panelFakeOps()` with its `registerFakeOps` seam.
// (fakeHost.ts imports this file, so this file only imports types from it.)

import { ApiError } from '../api/client.ts';
import type { FileView } from '../api/panel.ts';
import type { FakeOpHandler } from './fakeHost.ts';

export const FAKE_ROOT = 'C:\\Demo\\project';

const LONG = Array.from({ length: 400 }, (_, i) => `export const value${i} = ${i} * 2; // line ${i + 1}`).join('\n');

const FILES: Record<string, string> = {
  'README.md': '# Demo project\n\nSee [the guide](docs/guide.md) and `src/app.ts:3`, or src/values.ts:120-125 in prose.\n\n> [!note]\n> A callout.\n',
  'docs\\guide.md': '# Guide\n\nBack to the [readme](../README.md).\n',
  'src\\app.ts': `import { value1 } from './values';\n\nexport function main(): number {\n  return value1 + 1;\n}\n`,
  'src\\values.ts': LONG,
  'data\\config.json': JSON.stringify({ name: 'demo', version: '1.2.0', private: true, scripts: { build: 'vite build', test: 'node --test' }, tags: ['one', 'two', 'three'], nested: { deep: { ok: true, none: null, n: 42 } } }, null, 2),
  'data\\people.csv': 'name,team,note\nAnn,Core,"likes tests, mostly"\nBob,UI,\nCat,Hub,"says ""hi"""\n',
  '.env.example': 'TOKEN=changeme\n',
  'page.html': '<h1>Hello</h1><script>document.body.innerHTML = "scripts must not run"</script>',
};

const LOGO_SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="240" height="140"><rect width="240" height="140" fill="#3b6fb6"/><circle cx="120" cy="70" r="40" fill="#fff"/></svg>';

function norm(p: string): string {
  return p.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

function language(name: string): string | undefined {
  if (name.endsWith('.ts')) return 'typescript';
  if (name.endsWith('.md')) return 'markdown';
  if (name.endsWith('.json')) return 'json';
  return undefined;
}

function read(path: string): FileView {
  const n = norm(path);
  const root = norm(FAKE_ROOT);
  if (!n.startsWith(root)) throw new ApiError('not_found', 'The file was not found.');
  const rel = n.slice(root.length).replace(/^\\/, '');
  if (rel === 'logo.svg') {
    return { path, kind: 'image', size: LOGO_SVG.length, bytes: btoa(LOGO_SVG), mime: 'image/svg+xml', truncated: false };
  }
  const fileKey = Object.keys(FILES).find((k) => k.toLowerCase() === rel);
  if (fileKey !== undefined) {
    const text = FILES[fileKey] as string;
    return {
      path,
      kind: fileKey.endsWith('.html') ? 'html' : 'text',
      size: text.length,
      text,
      truncated: false,
      language: language(fileKey),
    };
  }
  const prefix = rel ? `${rel}\\` : '';
  const names = new Map<string, 'dir' | 'file'>();
  for (const k of Object.keys(FILES)) {
    const lk = k.toLowerCase();
    if (!lk.startsWith(prefix)) continue;
    const rest = k.slice(prefix.length);
    const at = rest.indexOf('\\');
    names.set(at < 0 ? rest : rest.slice(0, at), at < 0 ? 'file' : 'dir');
  }
  if (!rel) names.set('logo.svg', 'file');
  if (names.size === 0) throw new ApiError('not_found', 'The file was not found.');
  const entries = [...names].map(([name, kind]) => ({ name, kind })).sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'dir' ? -1 : 1));
  return { path, kind: 'dir', size: 0, entries, truncated: false };
}

/** A made-up vault for the vault-doc tab: an ambiguous name, an embed and a heading. */
const NOTES: Record<string, string> = {
  'Plans/Roadmap.md': '# Roadmap\n\nSee [[Ideas]] and ![[Ideas#Later]].\n\n## Now\n\nShip the panel.\n',
  'Ideas.md': '# Ideas\n\n## Soon\n\nA few.\n\n## Later\n\nThe embedded part.\n',
  'Daily/Ideas.md': '# Daily ideas\n',
};

export function panelFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  let saved: unknown = { version: 1, sets: {} };
  const contextOn = new Set<string>();
  let comments: Array<Record<string, unknown>> = [];
  // A made-up working tree: one staged, one partly staged, one untracked file and a conflict.
  let records = [
    { path: 'src/app.ts', status: 'M', section: 'staged', partial: false, added: 2, removed: 1 },
    { path: 'src/values.ts', status: 'M', section: 'staged', partial: true, added: 1, removed: 0 },
    { path: 'src/values.ts', status: 'M', section: 'unstaged', partial: true, added: 3, removed: 3 },
    { path: 'docs/new-note.md', status: 'U', section: 'unstaged', partial: false },
    { path: 'merge-me.txt', status: 'UU', section: 'conflicts', partial: false },
  ];
  const view = () => ({ repoTop: FAKE_ROOT, branch: 'main', records, truncated: false });
  const diffFor = (path: string) =>
    [
      `--- a/${path}`,
      `+++ b/${path}`,
      '@@ -1,30 +1,30 @@',
      ...Array.from({ length: 12 }, (_, i) => ` unchanged line ${i + 1}`),
      '-const before = 1;',
      '+const after = 2;',
      ...Array.from({ length: 17 }, (_, i) => ` unchanged line ${i + 14}`),
      '',
    ].join('\n');
  const handler: FakeOpHandler = (op, args) => {
    switch (op) {
      case 'review_turns':
        return (args.messageIds as number[]).map((messageId) => ({
          messageId,
          files: [
            { path: `${FAKE_ROOT}\\src\\app.ts`, repositoryRoot: FAKE_ROOT, repositoryPath: 'src/app.ts', added: 2, removed: 1, completeCounts: true },
            { path: `${FAKE_ROOT}\\docs\\guide.md`, repositoryRoot: FAKE_ROOT, repositoryPath: 'docs/guide.md', added: null, removed: null, completeCounts: false },
          ],
          truncated: false,
        }));
      case 'review_turn_diff': {
        const path = String(args.path);
        if (path.endsWith('guide.md')) return { patch: '', complete: false, time: 1700000000 };
        return {
          patch: ['@@ @@ edit', ' keep this line', '-const before = 1;', '+const after = 2;', ' and this one', ''].join('\n'),
          complete: true,
          time: 1700000000,
        };
      }
      case 'panel_context_get':
        return { on: contextOn.has(String(args.chatId)) };
      case 'panel_context_set':
        if (args.on) contextOn.add(String(args.chatId));
        else contextOn.delete(String(args.chatId));
        return { on: Boolean(args.on) };
      case 'review_comment_add': {
        const c = { ...args, quote: '(quoted lines)', createdAt: Date.now() } as Record<string, unknown>;
        delete c.chatId;
        delete c.fingerprint;
        comments.push(c);
        return c;
      }
      case 'review_comment_list':
        return comments;
      case 'review_comment_remove':
        comments = comments.filter((c) => c.id !== args.id);
        return comments;
      case 'review_comment_clear':
        comments = [];
        return comments;
      case 'vault_list':
        return [{ name: 'Demo Vault', path: 'C:\\Demo\\vault', source: 'obsidian', isDefault: true }];
      case 'vault_resolve': {
        const note = String(args.note ?? '').toLowerCase();
        const exact = Object.keys(NOTES).find((k) => k.toLowerCase() === `${note}.md`);
        if (exact) return { vault: 'Demo Vault', path: exact };
        const hits = Object.keys(NOTES).filter((k) => k.toLowerCase() === `${note}.md` || k.toLowerCase().endsWith(`/${note}.md`) || k.toLowerCase() === note);
        if (hits.length === 0) throw new ApiError('not_found', `No note named "${String(args.note)}" in this vault.`);
        return hits.length === 1 ? { vault: 'Demo Vault', path: hits[0] } : { vault: 'Demo Vault', candidates: hits };
      }
      case 'vault_read': {
        const path = String(args.path);
        const text = NOTES[path];
        if (text === undefined) throw new ApiError('not_found', 'The file was not found.');
        return { vault: 'Demo Vault', path, title: path.split('/').pop()!.replace(/\.md$/, ''), fullPath: `C:\\Demo\\vault\\${path}`, text, truncated: false };
      }
      case 'vault_open_in_obsidian':
        return null;
      case 'changes':
        return view();
      case 'diff':
        return { patch: diffFor(String(args.path)), fullContext: true, truncated: false };
      case 'changes_stage':
      case 'changes_unstage': {
        const to = op === 'changes_stage' ? 'staged' : 'unstaged';
        const from = op === 'changes_stage' ? 'unstaged' : 'staged';
        const hit = records.find((r) => r.path === args.path && r.section === from);
        if (!hit) throw new ApiError('conflict', 'The changes list is out of date. Refresh it and try again.');
        records = records.filter((r) => r !== hit && !(r.path === hit.path && r.section === to));
        records.push({ ...hit, section: to, partial: false, status: hit.status === 'U' ? 'A' : hit.status });
        return { changes: view(), noop: false };
      }
      case 'file_read':
      case 'dir_list':
        return read(String(args.path ?? ''));
      case 'default_handler':
        return String(args.path ?? '').endsWith('.md')
          ? { handler: { name: 'Demo Editor', appPath: 'C:\\Program Files\\Demo\\editor.exe' }, refusal: null }
          : { handler: null, refusal: { step: 'noHandler', reason: 'No default app' } };
      case 'file_open_default':
      case 'file_reveal':
        return null;
      case 'chat_folder':
        return { path: FAKE_ROOT };
      case 'path_resolve': {
        const raw = String(args.path ?? '').replace(/^\.[\\/]/, '');
        const abs = /^[a-z]:/i.test(raw) ? raw : `${FAKE_ROOT}\\${raw.replace(/\//g, '\\')}`;
        const v = read(abs);
        return { path: abs, kind: v.kind === 'dir' ? 'dir' : 'file' };
      }
      case 'quick_open': {
        const q = String(args.query ?? '').toLowerCase();
        const items = [...Object.keys(FILES), 'logo.svg']
          .filter((k) => k.toLowerCase().includes(q))
          .map((k, i) => ({ path: `${FAKE_ROOT}\\${k}`, label: k, kind: 'file', score: 100 - i }));
        return { mode: 'files', items, partial: false };
      }
      case 'panel_tabs_get':
        return saved;
      case 'panel_tabs_save':
        saved = args.value;
        return null;
      default:
        throw new ApiError('refused', 'This operation is not available here.');
    }
  };
  return { prefixes: ['file_', 'dir_list', 'default_handler', 'chat_folder', 'panel_tabs_', 'path_resolve', 'quick_open', 'changes', 'diff', 'vault_', 'panel_context_', 'review_comment_', 'review_turns', 'review_turn_diff'], handler };
}
