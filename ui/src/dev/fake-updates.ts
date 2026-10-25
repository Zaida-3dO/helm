// The dev fake host's update ops (the top-bar pill and Settings → General's update folder in a
// plain browser). `?update=ready` in the page URL starts with a folder chosen and a newer setup
// found; otherwise no folder is chosen, and choosing one (the fake folder dialog answers
// C:/Helm/builds) finds the same newer setup.
//
// Dev builds only. fakeHost.ts imports this file, so this file only imports types from it.

import { ApiError } from '../api/client.ts';
import type { UpdatePrepare, UpdateStatus } from '../api/updates.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const FOLDER = 'C:/Helm/builds';

export function updatesFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const ready = typeof location !== 'undefined' && new URLSearchParams(location.search).get('update') === 'ready';
  let source: string | null = ready ? FOLDER : null;
  const status = (): UpdateStatus => ({
    current: '0.3.0',
    source,
    available: source ? { version: '0.3.1', fileName: 'Helm_0.3.1_x64-setup.exe' } : null,
    checkedAt: source ? Date.now() : null,
    error: null,
  });
  const handler: FakeOpHandler = (op, a, ctx) => {
    switch (op) {
      case 'update_status':
      case 'update_check':
        return status();
      case 'update_source_set':
        source = typeof a.path === 'string' && a.path ? a.path : null;
        ctx.emit({ kind: 'update' });
        return status();
      case 'update_prepare': {
        if (!source) throw new ApiError('conflict', 'No update folder is chosen.');
        const titles = ['Fix the login bug', 'Write the migration', 'Review the PR', 'Tidy the sidebar', 'Draft release notes', 'Profile startup'];
        const p: UpdatePrepare = { version: '0.3.1', busy: titles.slice(0, 4).map((t) => `${t} (C:/Users/me/demo)`), busyTotal: titles.length };
        return p;
      }
      case 'update_install':
        return { outcome: 'restarting' };
      default:
        throw new ApiError('refused', 'This operation is not available here.');
    }
  };
  return { prefixes: ['update_'], handler };
}
