// Pull request links: which web addresses open a PR tab, and how a mention becomes a link.
// Pure, so every shape is a table row in the tests.

import type { PrAnchor, PrRef, PrTabData, PrView } from './types.ts';

const NAME = '[A-Za-z0-9._-]{1,100}';
const PR_PATH = new RegExp(`^/(${NAME})/(${NAME})/pull/([1-9][0-9]{0,8})(/files|/checks)?/?$`);

function validName(s: string): boolean {
  return s !== '.' && s !== '..';
}

/**
 * The tab a github.com pull request address opens, or null when the address is anything else:
 * another host, user info, a port, a sub-path such as commits. The query is ignored. A comment
 * anchor in the fragment moves Checks back to the conversation.
 */
export function parsePrUrl(href: string): PrTabData | null {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return null;
  }
  if (url.protocol !== 'https:' || url.hostname !== 'github.com') return null;
  if (url.username || url.password || url.port) return null;
  const m = PR_PATH.exec(url.pathname);
  if (!m) return null;
  const [, owner, repo, num, tail] = m as unknown as [string, string, string, string, string | undefined];
  if (!validName(owner) || !validName(repo)) return null;
  let view: PrView = tail === '/files' ? 'files' : tail === '/checks' ? 'checks' : 'conversation';
  let anchor: PrAnchor | undefined;
  const frag = /^#(issuecomment|discussion_r)-?(\d{1,18})$/.exec(url.hash);
  if (frag) {
    anchor = { kind: frag[1] === 'issuecomment' ? 'issue' : 'review', id: Number(frag[2]) };
    if (view === 'checks') view = 'conversation';
  }
  const data: PrTabData = { owner, repo, number: Number(num), view };
  if (anchor) data.anchor = anchor;
  return data;
}

export const prKey = (r: PrRef): string => `${r.owner}/${r.repo}#${r.number}`.toLowerCase();

export function prUrl(r: PrRef, view: PrView = 'conversation'): string {
  const tail = view === 'files' ? '/files' : view === 'checks' ? '/checks' : '';
  return `https://github.com/${r.owner}/${r.repo}/pull/${r.number}${tail}`;
}

/**
 * The tab data an open should store. A bare link (conversation, no anchor) keeps the view an
 * existing tab already shows; a link that names a view or an anchor changes to it.
 */
export function mergeOpen(existing: PrTabData | null, next: PrTabData): PrTabData {
  if (existing && next.view === 'conversation' && !next.anchor) return existing;
  return next;
}

/** A reference like `owner/repo#12` or a bare `#12`: not after a name character, slash, at sign, ampersand or hash. */
export const MENTION = /(^|[^A-Za-z0-9_./@&#-])((?:([A-Za-z0-9._-]{1,100})\/([A-Za-z0-9._-]{1,100}))?#([1-9][0-9]{0,8}))(?![A-Za-z0-9_-])/g;

export interface Mention {
  start: number;
  end: number;
  ref: PrRef;
}

/**
 * Where plain text mentions a pull request. `owner/repo#12` always counts; a bare `#12` only when
 * `repo` (the chat's own GitHub repository) is known. The caller skips code, links and raw HTML.
 */
export function findMentions(text: string, repo: { owner: string; repo: string } | null): Mention[] {
  const out: Mention[] = [];
  const re = new RegExp(MENTION.source, 'g');
  for (let m = re.exec(text); m; m = re.exec(text)) {
    const lead = m[1] ?? '';
    const full = m[2] as string;
    const owner = m[3];
    const name = m[4];
    const number = Number(m[5]);
    const ref = owner && name ? { owner, repo: name, number } : repo ? { ...repo, number } : null;
    if (!ref || !validName(ref.owner) || !validName(ref.repo)) continue;
    const start = m.index + lead.length;
    out.push({ start, end: start + full.length, ref });
  }
  return out;
}

/** The owner and name from a github.com remote (https, ssh or scp form); null for anything else. */
export function repoFromRemote(remote: string | undefined | null): { owner: string; repo: string } | null {
  if (!remote) return null;
  const m =
    /^(?:https:\/\/(?:[^@/]+@)?github\.com\/|ssh:\/\/git@github\.com\/|git@github\.com:)([A-Za-z0-9._-]{1,100})\/([A-Za-z0-9._-]{1,100}?)(?:\.git)?\/?$/.exec(
      remote.trim(),
    );
  if (!m) return null;
  const [, owner, repo] = m as unknown as [string, string, string];
  return validName(owner) && validName(repo) ? { owner, repo } : null;
}
