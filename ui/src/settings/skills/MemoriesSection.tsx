// Memories: the facts and lessons the hub's shared memory holds for a layer. Moving one changes who
// can see it (Everyone, or one person). With no hub connected the host answers `unavailable` and
// the section says so; a host that has no such op leaves the section out.

import { useCallback, useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import type { KnowledgeOps, MemoryRow } from './api.ts';
import { errorCode, errorText } from './common.ts';
import { MoveTo } from './LayerPicker.tsx';
import type { LayerOption } from './layers.ts';
import type { RunAction } from './RulesSection.tsx';

type State = { kind: 'loading' } | { kind: 'rows'; rows: MemoryRow[] } | { kind: 'unavailable' } | { kind: 'absent' } | { kind: 'error'; text: string };

export function MemoriesSection({ ops, layer, options, busy, run }: {
  ops: KnowledgeOps;
  layer: string;
  options: LayerOption[];
  busy: boolean;
  run: RunAction;
}) {
  const shell = useShell();
  const [state, setState] = useState<State>({ kind: 'loading' });

  const load = useCallback(
    () =>
      ops
        .memories(layer)
        .then((rows) => setState({ kind: 'rows', rows }))
        .catch((e: unknown) => {
          const code = errorCode(e);
          if (code === 'unavailable') setState({ kind: 'unavailable' });
          else if (code === 'unknown_op') setState({ kind: 'absent' });
          else setState({ kind: 'error', text: errorText(e) });
        }),
    [ops, layer],
  );

  useEffect(() => {
    setState({ kind: 'loading' });
    void load();
    return shell.bus.on('knowledge-changed', () => void load());
  }, [shell, load]);

  if (state.kind === 'absent') return null;
  return (
    <section className="kn-section" data-testid="knowledge-memories">
      <div className="kn-head">
        <h3 className="hub-heading">Memories</h3>
      </div>
      <div className="setting-desc">Facts and lessons kept in shared memory on your hub, for this list.</div>
      {state.kind === 'unavailable' && (
        <div className="setting-desc" data-testid="knowledge-memories-empty">
          Shared memory isn't connected.
        </div>
      )}
      {state.kind === 'error' && (
        <div className="ui-field-error" role="alert" data-testid="knowledge-memories-error">
          {state.text}
        </div>
      )}
      {state.kind === 'rows' && state.rows.length === 0 && (
        <div className="setting-desc" data-testid="knowledge-memories-empty">
          No memories in this list.
        </div>
      )}
      {state.kind === 'rows' &&
        state.rows.map((m) => (
          <div className="skills-row" key={m.id} data-testid={`knowledge-memory-${m.id}`}>
            <div className="skills-what">
              <div className="skills-name">{m.title}</div>
              <div className="skills-desc">{m.owner ? `Kept for ${m.owner}` : 'Everyone can see this'}</div>
            </div>
            <MoveTo
              options={options}
              from={layer}
              what={m.title}
              testId={`knowledge-memory-move-${m.id}`}
              disabled={busy}
              onMove={(to) =>
                void run(async () => {
                  await ops.moveMemory(m.id, to);
                  await load();
                  return 'Moved the memory.';
                })
              }
            />
          </div>
        ))}
    </section>
  );
}
