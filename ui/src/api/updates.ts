// Typed wrappers over the update ops (helm-updates). Helm updates from a folder on this
// computer: the host finds a newer setup file there and, on confirmation, quits and runs it.

import type { Api } from './client.ts';

export interface UpdateStatus {
  /** The running app's version. */
  current: string;
  /** The folder Helm looks in; null until the user chooses one. */
  source: string | null;
  /** The newer setup file found in the folder, if any. */
  available: { version: string; fileName: string } | null;
  /** Unix ms of the last look in the folder. */
  checkedAt: number | null;
  /** Why the last look failed (the folder is gone, say); null when it worked. */
  error: string | null;
}

/** What the restart dialog shows: the version and the chats the restart would stop. */
export interface UpdatePrepare {
  version: string;
  /** At most four lines, one per running chat, as "Title (folder)". */
  busy: string[];
  busyTotal: number;
}

export type UpdateInstallOutcome = 'restarting' | 'save_failed' | 'busy' | 'cancelled';

export function updateOps(api: Api) {
  return {
    status: () => api<UpdateStatus>('update_status'),
    check: () => api<UpdateStatus>('update_check'),
    setSource: (path: string | null) => api<UpdateStatus>('update_source_set', { path }),
    prepare: () => api<UpdatePrepare>('update_prepare'),
    install: () => api<{ outcome: UpdateInstallOutcome }>('update_install'),
    /** The native folder dialog (helm-shell `folder_pick`); null when cancelled. */
    pickFolder: () => api<{ path: string | null }>('folder_pick', { title: 'Folder with the Helm setup file' }),
  };
}

export type UpdateOps = ReturnType<typeof updateOps>;
