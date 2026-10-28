// What the PR tab does with the rest of the app: the chat draft, a new chat, a file on disk.

import { composerApi } from '../composer/slots.ts';
import { openNewChat } from '../newchat/index.ts';
import { handleLink } from '../panel/links.ts';
import { panelStore } from '../panel/registry.ts';
import { repoFromRemote } from './links.ts';
import type { PrRef } from './types.ts';

/** Puts a block into the draft of the chat beside the panel. Never sends. Returns a short status line. */
export function addToChat(block: string): { ok: boolean; text: string } {
  const chatId = panelStore.current;
  if (!chatId) return { ok: false, text: 'Select a chat first, then add this to its draft.' };
  if (!composerApi(chatId).insert(block)) return { ok: false, text: 'That chat is not open, so its draft cannot take this.' };
  return { ok: true, text: 'Added to the draft of this chat.' };
}

/** Starts a new chat with the block in its draft. Never sends. */
export function newChatWith(block: string): { ok: boolean; text: string } {
  openNewChat({ text: block });
  return { ok: true, text: 'Started a new chat with this in its draft.' };
}

interface RepoRow {
  remoteUrl?: string;
  locations: { path: string }[];
}

/** The folder of a registered checkout of this repository, or null. */
export async function checkoutFor(ref: PrRef, list: () => Promise<{ entries: RepoRow[] }>): Promise<string | null> {
  try {
    const { entries } = await list();
    for (const r of entries) {
      const rr = repoFromRemote(r.remoteUrl);
      if (rr && rr.owner.toLowerCase() === ref.owner.toLowerCase() && rr.repo.toLowerCase() === ref.repo.toLowerCase()) return r.locations[0]?.path ?? null;
    }
  } catch {
    // A repository list that cannot be read means no local file: GitHub opens instead.
  }
  return null;
}

/** Opens a PR file from a registered checkout, else on GitHub at the head commit. Nothing opens for removed files. */
export async function openPrFile(ref: PrRef, path: string, head: string): Promise<string | null> {
  if (path.split('/').some((s) => s === '' || s === '.' || s === '..')) return 'That path cannot be opened.';
  const api = panelStore.api;
  const root = api ? await checkoutFor(ref, () => api<{ entries: RepoRow[] }>('repo_list', {})) : null;
  if (root) {
    const abs = `${root.replace(/[\\/]+$/, '')}/${path}`;
    const r = await handleLink({ kind: 'path', href: abs }, { chatId: panelStore.current });
    return r.ok ? null : r.reason;
  }
  const url = `https://github.com/${ref.owner}/${ref.repo}/blob/${head}/${path.split('/').map(encodeURIComponent).join('/')}`;
  const r = await handleLink({ kind: 'url', href: url }, { chatId: panelStore.current });
  return r.ok ? `No checkout of ${ref.owner}/${ref.repo} is registered, so this opened on GitHub.` : r.reason;
}
