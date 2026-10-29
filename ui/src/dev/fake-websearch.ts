// The dev fake host's web search ops (Settings → Web search in a plain browser). The key is
// kept only as a flag, like the real host, which never returns it. Dev builds only.

import { ApiError } from '../api/client.ts';
import type { WebSearchView } from '../settings/websearch/api.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const DEFAULT_PATH = 'C:\Users\you\.openclaw\openclaw.json';

export function webSearchFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const state = { hasKey: false, offer: false, importPath: null as string | null };
  const view = (): WebSearchView => ({
    hasKey: state.hasKey,
    offerInClaudeChats: state.offer,
    importPath: state.importPath ?? DEFAULT_PATH,
    defaultImportPath: DEFAULT_PATH,
  });
  const handler: FakeOpHandler = (op, a, ctx) => {
    switch (op) {
      case 'web_search_get':
        return view();
      case 'web_search_save':
        if (typeof a.offerInClaudeChats === 'boolean') state.offer = a.offerInClaudeChats;
        ctx.emit({ kind: 'web_search' });
        return view();
      case 'web_search_set_key':
        if (!/^[A-Za-z0-9_.~+/=-]{8,200}$/.test(String(a.key ?? '').trim())) {
          throw new ApiError('bad_args', 'That does not look like a Brave search key: it should be one run of letters and digits with no spaces.');
        }
        state.hasKey = true;
        return view();
      case 'web_search_forget_key':
        state.hasKey = false;
        return view();
      case 'web_search_test':
        return state.hasKey ? { ok: true, message: 'Brave search works (1 result for a test search).' } : { ok: false, message: 'No Brave search key is set.' };
      case 'web_search_import':
        if (!String(a.path ?? '').trim()) throw new ApiError('bad_args', 'Give the full path of the OpenClaw settings file.');
        if (String(a.path).includes('missing')) throw new ApiError('bad_args', 'That file could not be read.');
        state.hasKey = true;
        state.importPath = String(a.path).trim();
        return { imported: true };
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['web_search_'], handler };
}
