// Which GitHub repository a chat belongs to, for a bare `#12` in a reply: the registered repository
// whose checkout holds the chat's folder (the deepest one wins), when its remote is on github.com.

import { useEffect, useState } from 'react';
import type { Api } from '../api/client.ts';
import { repoFromRemote } from './links.ts';

export interface RepoRow {
  remoteUrl?: string;
  locations: { path: string }[];
}

const norm = (p: string) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();

/** `owner/repo`, or '' when the folder is in no registered github.com repository. */
export function mentionRepoFor(rows: readonly RepoRow[], cwd: string | null | undefined): string {
  if (!cwd) return '';
  const here = norm(cwd);
  let best = { depth: -1, repo: '' };
  for (const r of rows) {
    const gh = repoFromRemote(r.remoteUrl);
    if (!gh) continue;
    for (const l of r.locations) {
      const loc = norm(l.path);
      if ((here === loc || here.startsWith(`${loc}/`)) && loc.length > best.depth) best = { depth: loc.length, repo: `${gh.owner}/${gh.repo}` };
    }
  }
  return best.repo;
}

let cache: { at: number; rows: RepoRow[] } | null = null;
let inflight: Promise<RepoRow[]> | null = null;
const TTL_MS = 60_000;

function repoRows(api: Api): Promise<RepoRow[]> {
  if (cache && Date.now() - cache.at < TTL_MS) return Promise.resolve(cache.rows);
  inflight ??= api<{ entries: RepoRow[] }>('repo_list', {})
    .then((r) => {
      cache = { at: Date.now(), rows: r.entries ?? [] };
      return cache.rows;
    })
    .catch(() => [] as RepoRow[])
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/** The chat's repository for mentions; empty until the (local) repository list has answered. */
export function useMentionRepo(api: Api, cwd: string | null | undefined): string {
  const [repo, setRepo] = useState('');
  useEffect(() => {
    let live = true;
    if (!cwd) {
      setRepo('');
      return;
    }
    void repoRows(api).then((rows) => live && setRepo(mentionRepoFor(rows, cwd)));
    return () => {
      live = false;
    };
  }, [api, cwd]);
  return repo;
}
