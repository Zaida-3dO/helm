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
  'README.md': '# Demo project\n\nSee [the guide](docs/guide.md) and `src/app.ts`.\n\n> [!note]\n> A callout.\n',
  'docs\\guide.md': '# Guide\n\nBack to the [readme](../README.md).\n',
  'src\\app.ts': `import { value1 } from './values';\n\nexport function main(): number {\n  return value1 + 1;\n}\n`,
  'src\\values.ts': LONG,
  'page.html': '<h1>Hello</h1><script>document.body.innerHTML = "scripts must not run"</script>',
};

function norm(p: string): string {
  return p.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

function language(name: string): string | undefined {
  if (name.endsWith('.ts')) return 'typescript';
  if (name.endsWith('.md')) return 'markdown';
  return undefined;
}

function read(path: string): FileView {
  const n = norm(path);
  const root = norm(FAKE_ROOT);
  if (!n.startsWith(root)) throw new ApiError('not_found', 'The file was not found.');
  const rel = n.slice(root.length).replace(/^\\/, '');
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
  if (names.size === 0) throw new ApiError('not_found', 'The file was not found.');
  const entries = [...names].map(([name, kind]) => ({ name, kind })).sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'dir' ? -1 : 1));
  return { path, kind: 'dir', size: 0, entries, truncated: false };
}

export function panelFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  let saved: unknown = { version: 1, sets: {} };
  const handler: FakeOpHandler = (op, args) => {
    switch (op) {
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
      case 'panel_tabs_get':
        return saved;
      case 'panel_tabs_save':
        saved = args.value;
        return null;
      default:
        throw new ApiError('refused', 'This operation is not available here.');
    }
  };
  return { prefixes: ['file_', 'dir_list', 'default_handler', 'chat_folder', 'panel_tabs_'], handler };
}
