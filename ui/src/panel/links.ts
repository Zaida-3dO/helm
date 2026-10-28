// The link gate: `handleLink` is the only place a click on a link turns into an action.
//
// It re-validates every target from scratch, whatever `classifyHref` decided when the Markdown was
// rendered, because a target can also arrive from data attributes, another component or a
// restored tab. Network shares and device paths are refused from the string alone (opening one on
// Windows offers the user's NTLM credentials to that server); so is the drive-relative "C:x" form
// that "C:javascript:..." hides behind. Only http, https and mailto ever leave the app, and only
// through the host's `open_external` op.

import type { LinkTarget } from '../markdown/links.ts';
import { driveForm, isRemoteOrDevicePath } from '../markdown/paths.ts';
import { splitPathSuffix } from '../markdown/pathrefs.ts';
import { parseChatLink } from '../markdown/links.ts';

export const MAX_LINK_BYTES = 8 * 1024;

// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f]/;

export type GateResult =
  | { action: 'external'; url: string }
  | { action: 'file'; path: string; line?: number; endLine?: number; column?: number }
  /** A path written in a reply that the host must resolve (`~`, chat-relative, Git Bash `/c/`). */
  | { action: 'path'; path: string; line?: number; endLine?: number; column?: number }
  | { action: 'vault'; note: string; heading?: string; block?: string; vault?: string }
  | { action: 'anchor'; heading?: string; block?: string }
  /** A chat link from a reply or a tool result: opens that chat. */
  | { action: 'chat'; chatId: string; messageId?: number }
  | { action: 'refuse'; reason: string };

export const REFUSED = {
  share: 'Links to network shares and device paths are blocked.',
  scheme: 'This link uses a kind of address Helm does not open.',
  relative: 'Relative file links open only from inside a document.',
  driveRelative: 'This link is not a complete file path, so it was not opened.',
  malformed: 'This link is malformed, so it was not opened.',
  empty: 'This link does not point anywhere.',
} as const;

const refuse = (reason: string): GateResult => ({ action: 'refuse', reason });

function decodeStrict(s: string): string | null {
  try {
    return decodeURIComponent(s);
  } catch {
    return null;
  }
}

/** `path:line[:col]`, read before percent-decoding so an encoded colon stays part of the name. */
function splitLine(raw: string): { path: string; line?: number; column?: number } {
  const m = /:(\d+)(?::(\d+))?$/.exec(raw);
  if (!m) return { path: raw };
  const path = raw.slice(0, m.index);
  // "C:" alone is a drive, not a path with a line.
  if (path.length <= 2) return { path: raw };
  const line = Number(m[1]);
  const column = m[2] === undefined ? undefined : Number(m[2]);
  if (!(line >= 1) || (column !== undefined && !(column >= 1))) return { path: raw };
  return column === undefined ? { path, line } : { path, line, column };
}

/** A local `file:` URL to a path, or a refusal reason. */
function fileUrlPath(href: string): { path: string; line?: number; column?: number } | string {
  if (/[?#]/.test(href) || href.includes('\\')) return REFUSED.malformed;
  let rest = href.slice(5);
  if (rest.startsWith('//')) {
    const after = rest.slice(2);
    const slash = after.indexOf('/');
    const authority = slash < 0 ? after : after.slice(0, slash);
    const tail = slash < 0 ? '' : after.slice(slash);
    // `file://C:/x` is a common misspelling of `file:///C:/x`: a lone drive letter is not a host.
    if (/^[a-z]:$/i.test(authority)) rest = `/${authority}${tail}`;
    else if (authority !== '' && authority.toLowerCase() !== 'localhost') return REFUSED.share;
    else rest = tail;
  }
  if (rest.startsWith('//')) return REFUSED.share;
  const { path: encoded, line, column } = splitLine(rest);
  const decoded = decodeStrict(encoded);
  if (decoded === null || CONTROL.test(decoded)) return REFUSED.malformed;
  // file:///C:/x → C:/x
  const path = /^\/[a-z]:[\\/]/i.test(decoded) ? decoded.slice(1) : decoded;
  if (isRemoteOrDevicePath(path)) return REFUSED.share;
  if (driveForm(path) !== 'absolute') return REFUSED.relative;
  const out: { path: string; line?: number; column?: number } = { path };
  if (line !== undefined) out.line = line;
  if (column !== undefined) out.column = column;
  return out;
}

/**
 * Decides what a link may do. Pure: no host call and no filesystem access, so every branch is
 * tested.
 */
export function gateTarget(target: unknown): GateResult {
  if (!target || typeof target !== 'object') return refuse(REFUSED.empty);
  const t = target as Partial<LinkTarget>;

  if (t.kind === 'url') {
    const href = typeof t.href === 'string' ? t.href.trim() : '';
    if (!href) return refuse(REFUSED.empty);
    if (CONTROL.test(href) || new TextEncoder().encode(href).length > MAX_LINK_BYTES) return refuse(REFUSED.malformed);
    if (isRemoteOrDevicePath(href)) return refuse(REFUSED.share);
    const drive = driveForm(href);
    if (drive === 'relative') return refuse(REFUSED.driveRelative);
    if (drive === 'absolute') {
      if (href.includes('?') || href.includes('#')) return refuse(REFUSED.malformed);
      return { action: 'file', ...splitLine(href) };
    }
    const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(href)?.[1]?.toLowerCase();
    if (scheme === 'http' || scheme === 'https') {
      let url: URL;
      try {
        url = new URL(href);
      } catch {
        return refuse(REFUSED.malformed);
      }
      if (url.username || url.password) return refuse(REFUSED.malformed);
      return { action: 'external', url: url.href };
    }
    if (scheme === 'mailto') return { action: 'external', url: href };
    if (scheme === 'file') {
      const r = fileUrlPath(href);
      return typeof r === 'string' ? refuse(r) : { action: 'file', ...r };
    }
    if (scheme) return refuse(REFUSED.scheme);
    // A path with no scheme and no drive: relative (or rooted on the current drive).
    return refuse(REFUSED.relative);
  }

  if (t.kind === 'chat') {
    const chat = typeof t.href === 'string' ? parseChatLink(t.href.trim()) : null;
    return chat ? { action: 'chat', ...chat } : refuse(REFUSED.malformed);
  }

  if (t.kind === 'path') {
    const raw = typeof t.href === 'string' ? t.href.trim() : '';
    if (!raw) return refuse(REFUSED.empty);
    if (CONTROL.test(raw) || new TextEncoder().encode(raw).length > MAX_LINK_BYTES) return refuse(REFUSED.malformed);
    if (isRemoteOrDevicePath(raw)) return refuse(REFUSED.share);
    const { path, line, endLine, column } = splitPathSuffix(raw);
    if (isRemoteOrDevicePath(path)) return refuse(REFUSED.share);
    const drive = driveForm(path);
    if (drive === 'relative') return refuse(REFUSED.driveRelative);
    const out: { path: string; line?: number; endLine?: number; column?: number } = { path };
    if (line !== undefined) out.line = line;
    if (endLine !== undefined) out.endLine = endLine;
    if (column !== undefined) out.column = column;
    return drive === 'absolute' ? { action: 'file', ...out } : { action: 'path', ...out };
  }

  if (t.kind === 'vault') {
    const note = typeof t.note === 'string' ? t.note : '';
    const heading = typeof t.heading === 'string' && t.heading ? t.heading : undefined;
    const block = typeof t.block === 'string' && t.block ? t.block : undefined;
    const vault = typeof t.vault === 'string' && t.vault ? t.vault : undefined;
    for (const s of [note, heading ?? '', block ?? '', vault ?? '']) {
      if (CONTROL.test(s) || new TextEncoder().encode(s).length > MAX_LINK_BYTES) return refuse(REFUSED.malformed);
    }
    if (isRemoteOrDevicePath(note)) return refuse(REFUSED.share);
    if (driveForm(note) === 'relative') return refuse(REFUSED.driveRelative);
    if (vault !== undefined && /[\\/]/.test(vault)) return refuse(REFUSED.malformed);
    if (!note) {
      if (!heading && !block) return refuse(REFUSED.empty);
      return { action: 'anchor', heading, block };
    }
    const out: GateResult = { action: 'vault', note };
    if (heading) out.heading = heading;
    if (block) out.block = block;
    if (vault) out.vault = vault;
    return out;
  }

  return refuse(REFUSED.scheme);
}

type ChatRoute = (req: Extract<GateResult, { action: 'chat' }>, ctx: LinkContext) => Promise<boolean | string> | boolean | string;

export type LinkOutcome = { ok: true; action: GateResult['action'] } | { ok: false; reason: string };

export interface LinkContext {
  chatId: string | null;
  /** Set when the link sits inside a vault note shown in the panel. */
  from?: { vault: string; path: string };
}

type FileRoute = (req: Extract<GateResult, { action: 'file' }>, ctx: LinkContext) => Promise<boolean> | boolean;
type PathRoute = (req: Extract<GateResult, { action: 'path' }>, ctx: LinkContext) => Promise<boolean | string> | boolean | string;
type VaultRoute = (
  req: Extract<GateResult, { action: 'vault' | 'anchor' }>,
  ctx: LinkContext,
) => Promise<boolean | string> | boolean | string;

interface LinkDeps {
  openExternal: (url: string) => Promise<unknown>;
  notify: (message: string) => void;
}

const routes: { file?: FileRoute; vault?: VaultRoute; path?: PathRoute; chat?: ChatRoute } = {};
let deps: LinkDeps = {
  openExternal: () => Promise.reject(new Error('The link handler is not connected yet.')),
  notify: (m) => console.warn(m),
};

/** The side panel connects the gate to the host when it mounts. */
export function configureLinks(next: Partial<LinkDeps>): void {
  deps = { ...deps, ...next };
}

/** The file viewer and the vault doc tab register where their links go. Returns an unregister. */
export function registerLinkRoute(kind: 'file', route: FileRoute): () => void;
export function registerLinkRoute(kind: 'vault', route: VaultRoute): () => void;
export function registerLinkRoute(kind: 'path', route: PathRoute): () => void;
export function registerLinkRoute(kind: 'chat', route: ChatRoute): () => void;
export function registerLinkRoute(kind: 'file' | 'vault' | 'path' | 'chat', route: FileRoute | VaultRoute | PathRoute | ChatRoute): () => void {
  (routes as Record<string, unknown>)[kind] = route;
  return () => {
    if ((routes as Record<string, unknown>)[kind] === route) delete (routes as Record<string, unknown>)[kind];
  };
}

interface WebRoute {
  match: (url: string) => unknown;
  open: (matched: unknown, ctx: LinkContext) => Promise<boolean> | boolean;
}
const webRoutes: WebRoute[] = [];

/**
 * A web address may open inside the app instead of the browser. `match` reads the address (null
 * means not mine); `open` shows it and answers false when it could not, so the address falls
 * through to the browser. Routes are asked in the order they registered.
 */
export function registerWebRoute<T>(match: (url: string) => T | null, open: (matched: T, ctx: LinkContext) => Promise<boolean> | boolean): () => void {
  const route: WebRoute = { match, open: open as WebRoute['open'] };
  webRoutes.push(route);
  return () => {
    const i = webRoutes.indexOf(route);
    if (i >= 0) webRoutes.splice(i, 1);
  };
}

const NOT_AVAILABLE = 'This kind of link cannot be opened in this version of Helm yet.';

/**
 * The gate itself: validate, then act. Never navigates the webview. A refusal shows a notice and
 * changes nothing else.
 */
export async function handleLink(target: LinkTarget, ctx: LinkContext, opts: { skipWebRoutes?: boolean } = {}): Promise<LinkOutcome> {
  const gate = gateTarget(target);
  const fail = (reason: string): LinkOutcome => {
    deps.notify(reason);
    return { ok: false, reason };
  };
  try {
    switch (gate.action) {
      case 'refuse':
        return fail(gate.reason);
      case 'external':
        if (!opts.skipWebRoutes && gate.url.startsWith('http')) {
          for (const r of webRoutes) {
            const m = r.match(gate.url);
            if (m !== null && (await r.open(m, ctx))) return { ok: true, action: 'external' };
          }
        }
        await deps.openExternal(gate.url);
        return { ok: true, action: 'external' };
      case 'file': {
        const route = routes.file;
        if (!route) return fail(NOT_AVAILABLE);
        return (await route(gate, ctx)) ? { ok: true, action: 'file' } : fail('The file could not be opened.');
      }
      case 'path': {
        const route = routes.path;
        if (!route) return fail(NOT_AVAILABLE);
        const r = await route(gate, ctx);
        if (r === true) return { ok: true, action: 'path' };
        return fail(typeof r === 'string' ? r : 'The file could not be opened.');
      }
      case 'chat': {
        const route = routes.chat;
        if (!route) return fail(NOT_AVAILABLE);
        const r = await route(gate, ctx);
        if (r === true) return { ok: true, action: 'chat' };
        return fail(typeof r === 'string' ? r : 'The chat could not be opened.');
      }
      case 'vault':
      case 'anchor': {
        const route = routes.vault;
        if (!route) return gate.action === 'anchor' && !ctx.from ? { ok: true, action: 'anchor' } : fail(NOT_AVAILABLE);
        const r = await route(gate, ctx);
        if (r === true) return { ok: true, action: gate.action };
        return fail(typeof r === 'string' ? r : 'The note could not be opened.');
      }
    }
  } catch (e) {
    return fail(e instanceof Error && e.message ? e.message : 'The link could not be opened.');
  }
}
