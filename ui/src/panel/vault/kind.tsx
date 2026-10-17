// The `vault-doc` tab: an Obsidian note in the side panel.
// Chat-scoped, at most 4 per chat, keyed `<vault>/<path>`, saved as `{vault, path, heading?}`. It
// reads only when shown (a restored tab reads nothing until selected), keeps an 8-note session
// cache, renders embeds one level deep, and offers Open in Obsidian, Refresh and Reveal.

import { useEffect, useMemo, useRef, useState } from 'react';
import { panelOps } from '../../api/panel.ts';
import type { VaultDoc } from '../../api/vault.ts';
import { vaultOps } from '../../api/vault.ts';
import { Markdown } from '../../markdown/index.ts';
import type { LinkTarget } from '../../markdown/index.ts';
import { Button } from '../../ui/index.ts';
import { NoteIcon } from '../icons.tsx';
import { handleLink, registerLinkRoute } from '../links.ts';
import type { PanelTab } from '../model.ts';
import { openInPanel, panelStore, registerPanelKind } from '../registry.ts';
import type { TabContext } from '../registry.ts';
import { report } from '../context/store.ts';
import { embedText, headingKey } from './section.ts';

export const VAULT_DOC = 'vault-doc';
const CACHE_LIMIT = 8;

interface DocData {
  vault: string;
  path?: string;
  heading?: string;
  /** An ambiguous link: the notes it could mean. */
  candidates?: string[];
  note?: string;
}

function ops() {
  const api = panelStore.api;
  if (!api) throw new Error('The side panel is not connected to the host yet.');
  return vaultOps(api);
}

/** The session cache: up to 8 notes, least recently used dropped. */
const cache = new Map<string, VaultDoc>();
function remember(doc: VaultDoc) {
  const k = `${doc.vault}/${doc.path}`.toLowerCase();
  cache.delete(k);
  cache.set(k, doc);
  while (cache.size > CACHE_LIMIT) cache.delete(cache.keys().next().value as string);
}
function cached(vault: string, path: string): VaultDoc | undefined {
  const k = `${vault}/${path}`.toLowerCase();
  const d = cache.get(k);
  if (d) remember(d);
  return d;
}

function toBlobUrl(doc: VaultDoc): string | null {
  if (!doc.bytes || !doc.mime?.startsWith('image/')) return null;
  const bin = atob(doc.bytes);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return URL.createObjectURL(new Blob([bytes], { type: doc.mime }));
}

const keyOf = (vault: string, path: string) => `${vault}/${path}`.toLowerCase();

function open(data: DocData, chatId: string | null): boolean {
  const key = data.path ? keyOf(data.vault, data.path) : `?${data.vault}/${data.note ?? ''}`.toLowerCase();
  return openInPanel(VAULT_DOC, key, data, { intent: 'user', chatId });
}

function DocBody({ tab, ctx }: { tab: PanelTab; ctx: TabContext }) {
  const data = tab.data as DocData;
  const chatId = tab.set === '' ? null : tab.set;
  const [doc, setDoc] = useState<VaultDoc | null>(() => (data.path ? (cached(data.vault, data.path) ?? null) : null));
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const blobs = useRef<string[]>([]);

  // Armed read: only while shown, and only what this session has not read yet (or on Refresh).
  useEffect(() => {
    if (!ctx.visible || !data.path) return;
    if (doc && reload === 0 && doc.path === data.path) return;
    let cancelled = false;
    ops()
      .read(data.vault, data.path)
      .then((d) => {
        if (cancelled) return;
        remember(d);
        setDoc(d);
        setError(null);
        ctx.setTitle(d.title);
        report(tab.id, { kind: 'file', path: d.fullPath });
      })
      .catch((e: unknown) => !cancelled && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => !cancelled && setReload(0));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ctx.visible, data.vault, data.path, reload]);

  useEffect(() => () => blobs.current.forEach((u) => URL.revokeObjectURL(u)), []);

  // Scroll to the linked heading once the note is on screen.
  useEffect(() => {
    if (!doc || !data.heading || !root.current) return;
    const want = headingKey(data.heading);
    const h = [...root.current.querySelectorAll<HTMLElement>('h1,h2,h3,h4,h5,h6')].find((el) => headingKey(el.textContent ?? '') === want);
    h?.scrollIntoView({ block: 'start' });
  }, [doc, data.heading]);

  const onLink = useMemo(
    () => (t: LinkTarget) => {
      if (t.kind === 'vault' && !t.note && t.heading && root.current) {
        const want = headingKey(t.heading);
        const h = [...root.current.querySelectorAll<HTMLElement>('h1,h2,h3,h4,h5,h6')].find((el) => headingKey(el.textContent ?? '') === want);
        h?.scrollIntoView({ block: 'start' });
        return;
      }
      void handleLink(t, { chatId, from: data.path ? { vault: data.vault, path: data.path } : undefined });
    },
    [chatId, data.vault, data.path],
  );

  const resolveEmbed = useMemo(
    () => async (t: LinkTarget): Promise<string | null> => {
      if (!t.note) return null;
      const r = await ops().resolve({ note: t.note, fromVault: data.vault, fromPath: data.path, chatId });
      if (!('path' in r)) return null;
      const d = cached(r.vault, r.path) ?? (await ops().read(r.vault, r.path));
      remember(d);
      if (d.bytes) {
        const u = toBlobUrl(d);
        if (u) blobs.current.push(u);
        return u;
      }
      return embedText(d.text ?? '', t.heading, t.block);
    },
    [data.vault, data.path, chatId],
  );

  if (data.candidates) {
    return (
      <div className="vd-root" data-testid="vault-candidates">
        <div className="vd-title">Which note did you mean?</div>
        <div className="ch-hint">&ldquo;{data.note}&rdquo; matches more than one note in {data.vault}.</div>
        {data.candidates.map((c) => (
          <button key={c} type="button" className="fv-entry" onClick={() => open({ vault: data.vault, path: c }, chatId)}>
            <NoteIcon size={15} />
            <span className="fv-entry-name">{c}</span>
          </button>
        ))}
      </div>
    );
  }
  if (error) {
    return (
      <div className="fv-message" role="alert" data-testid="vault-error">
        {error}
      </div>
    );
  }
  if (!doc) return <div className="fv-message">{ctx.visible ? 'Loading…' : ''}</div>;
  return (
    <div className="vd-root" data-testid="vault-doc" data-vault={doc.vault} data-path={doc.path} ref={root}>
      {doc.truncated && <div className="fv-notice">Showing the first 512 KiB of this note.</div>}
      <div className="fv-markdown">
        <Markdown text={doc.text ?? ''} onLink={onLink} resolveEmbed={resolveEmbed} />
      </div>
    </div>
  );
}

function Actions({ tab }: { tab: PanelTab }) {
  const data = tab.data as DocData;
  if (!data.path) return null;
  const doc = cached(data.vault, data.path);
  const fail = (e: unknown) => panelStore.showNotice(e instanceof Error ? e.message : String(e));
  return (
    <>
      <Button variant="text" data-testid="open-in-obsidian" onClick={() => void ops().openInObsidian(data.vault, data.path!).catch(fail)}>
        Open in Obsidian
      </Button>
      <Button
        variant="text"
        onClick={() => {
          cache.delete(keyOf(data.vault, data.path!));
          panelStore.update((m) => {
            // A new data object makes the body read again.
            const t = m.tabs.find((x) => x.id === tab.id);
            if (t) t.data = { ...(t.data as DocData) };
          });
        }}
      >
        Refresh
      </Button>
      {doc && (
        <Button variant="text" onClick={() => panelStore.api && void panelOps(panelStore.api).reveal(doc.fullPath).catch(fail)}>
          Reveal
        </Button>
      )}
    </>
  );
}

registerPanelKind({
  kind: VAULT_DOC,
  scope: 'chat',
  maxTabs: 4,
  title: (tab) => {
    const d = tab.data as DocData;
    if (d.candidates) return d.note ?? 'Note';
    return (d.path ?? '').split('/').pop()?.replace(/\.md$/i, '') ?? 'Note';
  },
  icon: () => <NoteIcon />,
  render: (tab, ctx) => <DocBody key={JSON.stringify(tab.data)} tab={tab as PanelTab} ctx={ctx} />,
  headerActions: (tab) => <Actions tab={tab as PanelTab} />,
  persist: {
    version: 1,
    save: (tab) => {
      const d = tab.data as DocData;
      if (!d.path) return null;
      return d.heading ? { vault: d.vault, path: d.path, heading: d.heading } : { vault: d.vault, path: d.path };
    },
    restore: (data) => {
      const d = data as Partial<DocData> | null;
      if (!d || typeof d.vault !== 'string' || typeof d.path !== 'string') return null;
      if (/[\\/]/.test(d.vault) || d.path.includes('..') || /^[\\/]|^[a-z]:/i.test(d.path)) return null;
      return d.heading && typeof d.heading === 'string' ? { vault: d.vault, path: d.path, heading: d.heading } : { vault: d.vault, path: d.path };
    },
  },
});

/** Vault links (wikilinks, relative `.md` links, `obsidian://`) open the note in a tab. */
registerLinkRoute('vault', async (req, ctx) => {
  if (req.action === 'anchor') return true; // an in-page heading link in a reply: nothing to open
  try {
    const r = await ops().resolve({
      note: req.note,
      vault: req.vault,
      chatId: ctx.chatId,
      fromVault: ctx.from?.vault,
      fromPath: ctx.from?.path,
    });
    const ok =
      'path' in r
        ? open({ vault: r.vault, path: r.path, ...(req.heading ? { heading: req.heading } : {}) }, ctx.chatId)
        : open({ vault: r.vault, candidates: r.candidates, note: req.note }, ctx.chatId);
    return ok || 'The side panel has no room for another note. Close a tab and try again.';
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
});
