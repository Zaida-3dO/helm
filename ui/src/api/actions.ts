// Typed wrappers over the action-button ops. Every call still goes through
// api(op, args).

import type { Api } from './client.ts';
import type { ActionButton, ActionSet } from './generated/actions/index.ts';

export type { ActionButton, ActionKind, ActionSet, Placement } from './generated/actions/index.ts';

export function actionOps(api: Api) {
  return {
    get: () => api<ActionSet>('actions_get', {}),
    save: (revision: number, buttons: ActionButton[]) => api<ActionSet>('actions_save', { revision, buttons }),
  };
}

export type ActionOps = ReturnType<typeof actionOps>;
