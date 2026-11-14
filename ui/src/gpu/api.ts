// The graphics-card lock ops (host/crates/localmodels): read the lock, unload local models, turn them back on.
// Unload answers at once with `working: true`; the `local_lock` event says when the work is done.

import type { Api } from '../api/client.ts';
import type { LockKind, LockView } from '../api/generated/localmodels/index.ts';

export type { LockKind, LockView };

export function gpuOps(api: Api) {
  return {
    state: () => api<LockView>('local_lock_state', {}),
    /** While a game holds the lock this makes it a manual one; refused with "Local models are already unloaded." when it is already manual. */
    unload: () => api<LockView>('local_unload', {}),
    reload: () => api<LockView>('local_reload', {}),
  };
}

export type GpuOps = ReturnType<typeof gpuOps>;
