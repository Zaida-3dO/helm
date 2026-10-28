// The Pull requests group in Ctrl+K: GitHub's search over pull requests the user is involved in,
// ten rows, only while the PR view is on. Choosing a row opens its tab.

import type { Api } from '../api/client.ts';
import { prClient } from '../pr/client.ts';
import { arr, num, obj, str } from '../pr/parse.ts';
import { prViewEnabled } from '../pr/route.ts';
import type { PrRef } from '../pr/types.ts';
import { rowId, type Row, type SearchSource } from './sources.ts';

export interface PrsDeps {
  api: Api;
  open: (ref: PrRef) => void;
}

/** `owner/repo` from a search item: its repository address, or a `repository.nameWithOwner`. */
function repoOf(item: Record<string, unknown>): { owner: string; repo: string } | null {
  const url = str(item.repository_url);
  const m = /\/repos\/([A-Za-z0-9._-]+)\/([A-Za-z0-9._-]+)$/.exec(url) ?? /^([A-Za-z0-9._-]+)\/([A-Za-z0-9._-]+)$/.exec(str(obj(item.repository).nameWithOwner));
  return m ? { owner: m[1] as string, repo: m[2] as string } : null;
}

export function parsePrSearch(data: unknown): { ref: PrRef; title: string; state: string }[] {
  const d = obj(data);
  const list = arr(d.items).length ? arr(d.items) : arr(data);
  const out: { ref: PrRef; title: string; state: string }[] = [];
  for (const raw of list) {
    const it = obj(raw);
    const repo = repoOf(it);
    const number = num(it.number);
    if (!repo || number < 1) continue;
    out.push({ ref: { ...repo, number }, title: str(it.title), state: str(it.state).toLowerCase() });
    if (out.length >= 10) break;
  }
  return out;
}

export function prsSource(d: PrsDeps): SearchSource {
  const client = prClient(d.api);
  return {
    id: 'prs',
    minChars: 2,
    timeoutMs: 10_000,
    run: async (query, signal) => {
      if (!(await prViewEnabled())) return [];
      const a = await client.search(`is:pr involves:@me ${query}`);
      if (signal.aborted) return [];
      return parsePrSearch(a.data).map(
        (h): Row => ({
          id: rowId('prs', `${h.ref.owner}/${h.ref.repo}#${h.ref.number}`),
          title: `#${h.ref.number} ${h.title}`,
          subtitle: `${h.ref.owner}/${h.ref.repo}`,
          tag: h.state === 'closed' ? 'closed' : undefined,
          open: () => d.open(h.ref),
        }),
      );
    },
  };
}
