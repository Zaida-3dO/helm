// The Orchestrator switch in the composer toolbar. Icon-only; shown for
// Claude Code chats while Settings → Orchestrator is on. Inert states use aria-disabled so the
// tooltip stays reachable by keyboard. A host refusal shows as a red line after the button.

import { useEffect, useSyncExternalStore } from 'react';
import type { AccessoryContext } from '../composer/slots.ts';
import { useShell } from '../shell/context.ts';
import { switchModel } from './model.ts';
import { orchestratorStoreFor } from './store.ts';
import './orchestrator.css';

function WorkflowIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="3" y="3" width="6" height="6" rx="1" />
      <rect x="15" y="15" width="6" height="6" rx="1" />
      <path d="M12 6h3a3 3 0 0 1 3 3v6M6 9v3a3 3 0 0 0 3 3h6" />
    </svg>
  );
}

export function ModeSwitch({ ctx }: { ctx: AccessoryContext }) {
  const shell = useShell();
  const store = orchestratorStoreFor(shell.api, shell.bus);
  const chatId = ctx.chat.id;
  useEffect(() => store.ensure(chatId), [store, chatId]);
  useSyncExternalStore(store.subscribe, () => store.version);
  const view = store.view(chatId);
  const enabled = store.settings?.enabled === true;
  if (ctx.chat.harness !== 'claude' || !enabled || !view) return null;
  const m = switchModel(view, ctx.busy);
  const error = store.error(chatId);
  return (
    <>
      <span className="orch-divider" aria-hidden="true" />
      <button
        type="button"
        className="orch-switch"
        data-testid="orchestrator-switch"
        data-look={m.look}
        data-mode={view.mode}
        aria-pressed={view.mode === 'on'}
        aria-disabled={!m.clickable}
        aria-label={m.tooltip}
        title={m.tooltip}
        style={m.dimmed ? { opacity: 0.55 } : undefined}
        onClick={() => {
          if (m.next) void store.set(chatId, m.next);
        }}
      >
        <WorkflowIcon />
      </button>
      {error && (
        <span className="orch-error" data-testid="orchestrator-error" title={error}>
          {error}
        </span>
      )}
    </>
  );
}
