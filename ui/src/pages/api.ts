// Typed wrappers over the sealed-page ops (host/crates/pages/src/ops.rs). Hand-written shapes:
// the host types are plain JSON, not exported by the bindings generator.

import type { Api } from '../api/client.ts';

/** One pinned page, as the host lists it. */
export interface PinnedPage {
  id: string;
  /** Empty for a page pinned before profiles were set up. */
  profileId: string;
  path: string;
  title: string;
  /** The chat the page's messages go to. */
  chatId: string | null;
  pinnedAt: number;
}

/** An open page: the address to load, the channel it speaks on, and what it saved last. */
export interface PageSession {
  session: string;
  channel: string;
  url: string;
  path: string;
  key: string;
  title: string;
  network: boolean;
  /** The page's scripts run. False only offline, in a web view that cannot keep them off the network. */
  scripts: boolean;
  revision: number;
  size: number;
  state: unknown;
  stateRevision: number;
  pins: PinnedPage[];
}

/** The pages pinned on one other PC of the household, as `devices_pages` lists them. */
export interface RemotePages {
  deviceUid: string;
  name: string;
  pages: PinnedPage[];
  /** Why that PC could not be read (offline, its switch off...), in words. */
  problem: { code: string; message: string } | null;
}

export interface PageStatus {
  revision: number;
  changed: boolean;
  missing: boolean;
  error: string | null;
}

export function pageOps(api: Api) {
  return {
    /**
     * Reads the file and starts a session. The host decides the network: it is on when the page
     * is pinned for the acting profile, off otherwise, and fixed for the session's life.
     */
    open: (path: string, chatId: string | null) =>
      api<PageSession>('page_open', { path, ...(chatId ? { chatId } : {}) }),
    reload: (session: string) => api<Omit<PageSession, 'state' | 'stateRevision' | 'pins'>>('page_reload', { session }),
    close: (session: string) => api<{ closed: boolean }>('page_close', { session }),
    /** The file watch: has the file changed since the session last read it? */
    status: (session: string) => api<PageStatus>('page_status', { session }),
    saveState: (session: string, state: unknown, revision: number) =>
      api<{ revision: number }>('page_state_save', { session, state, revision }),
    resetState: (session: string) => api<{ revision: number }>('page_state_reset', { session }),
    pin: (path: string, opts: { title?: string; chatId?: string | null; profileId?: string | null } = {}) =>
      api<{ page: PinnedPage; created: boolean }>('page_pin', {
        path,
        ...(opts.title ? { title: opts.title } : {}),
        ...(opts.chatId ? { chatId: opts.chatId } : {}),
        ...(opts.profileId ? { profileId: opts.profileId } : {}),
      }),
    unpin: (id: string) => api<{ removed: boolean }>('page_unpin', { id }),
    linkChat: (id: string, chatId: string | null) => api<{ linked: boolean }>('page_link_chat', { id, ...(chatId ? { chatId } : {}) }),
    /** The pages pinned on the household's other PCs that the active profile's person may open. */
    devicePages: () => api<{ devices: RemotePages[] }>('devices_pages', {}),
    /** A page from another PC, brought across and opened here; answers as `open` does. */
    openRemote: (deviceUid: string, pageId: string) => api<PageSession>('devices_page_open', { deviceUid, pageId }),
    list: (profileId?: string | null) => api<{ pages: PinnedPage[] }>('page_list', profileId ? { profileId } : {}),
    /** The bundle's own favicon as a data URL, or null; for the Pages list row icon. */
    icon: (path: string) => api<{ icon: string | null }>('page_icon', { path }),
  };
}

export type PageOps = ReturnType<typeof pageOps>;
