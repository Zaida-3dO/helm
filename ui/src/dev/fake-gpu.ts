// The dev fake host's graphics-card lock (the top-bar control and Settings → Local models' Graphics
// card section in a plain browser). Unload stores the lock at once and finishes a moment later, as
// the real one does; the `local_lock` event says when. `?gpu=game` in the page URL starts with a game
// holding the lock and `?gpu=manual` with an unload already done. Nothing here touches a server.
//
// Dev builds only. fakeHost.ts imports this file, so this file only imports types from it.

import { ApiError } from '../api/client.ts';
import type { LockView } from '../api/generated/localmodels/index.ts';
import type { GpuSource } from './fake-localmodels.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const WORK_MS = 1200;
const GAME = 'game.exe';

export function gpuFakeOps(source: GpuSource): { prefixes: string[]; handler: FakeOpHandler } {
  const start = typeof location !== 'undefined' ? new URLSearchParams(location.search).get('gpu') : null;
  let state: LockView['state'] = start === 'game' ? 'game' : start === 'manual' ? 'manual' : 'on';
  let since: number | undefined = state === 'on' ? undefined : Date.now() - 20 * 60_000;
  let working = false;
  let report: string[] = state === 'manual' ? doneReport() : [];

  function doneReport(): string[] {
    const task = source.restartTask();
    return [
      '2 local-model chats were stopped.',
      ...(task ? [`The restart task "${task}" was ended.`] : []),
      'Ollama unloaded 1 model.',
      "Strata's model was unloaded and its server stopped.",
    ];
  }

  const view = (): LockView => ({
    state,
    ...(since !== undefined ? { since } : {}),
    ...(state === 'game' ? { game: GAME } : {}),
    working,
    report,
    anyLocal: source.anyLocal(),
    liftMinutes: source.liftMinutes(),
    ...(source.restartTask() ? { restartTask: source.restartTask() } : {}),
  });

  const handler: FakeOpHandler = (op, _a, ctx) => {
    switch (op) {
      case 'local_lock_state':
        return view();
      case 'local_unload':
        if (state === 'game') {
          // Keep the game's lock past the game: same time, nothing more to unload.
          state = 'manual';
          ctx.emit({ kind: 'local_lock' });
          return view();
        }
        if (state !== 'on') throw new ApiError('refused', 'Local models are already unloaded.');
        state = 'manual';
        since = Date.now();
        working = true;
        report = [];
        setTimeout(() => {
          working = false;
          report = doneReport();
          ctx.emit({ kind: 'local_lock' });
        }, WORK_MS);
        ctx.emit({ kind: 'local_lock' });
        return view();
      case 'local_reload':
        state = 'on';
        since = undefined;
        working = false;
        report = [];
        ctx.emit({ kind: 'local_lock' });
        return view();
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['local_lock', 'local_unload', 'local_reload'], handler };
}
