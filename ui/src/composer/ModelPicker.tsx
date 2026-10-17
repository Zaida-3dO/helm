// The classic model and effort picker, Claude only. A change applies from the
// next turn (the host acknowledges within 10 s); a failed save reverts with an error.

import { useEffect, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { ChatRecord, ModelCatalogue } from '../api/types.ts';

let cached: Promise<ModelCatalogue> | null = null;

export function ModelPicker({ chat, ops }: { chat: ChatRecord; ops: ChatOps }) {
  const [catalogue, setCatalogue] = useState<ModelCatalogue | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    cached ??= ops.catalogue();
    cached.then((c) => live && setCatalogue(c)).catch(() => {
      cached = null;
    });
    return () => {
      live = false;
    };
  }, [ops]);

  const set = (patch: { model?: string; effort?: string }) => {
    setError(null);
    ops.setModel(chat.id, patch.model, patch.effort).catch((e: unknown) => setError(String((e as Error).message ?? e)));
  };

  const models = catalogue?.models ?? [];
  const model = chat.model ?? models[0]?.id ?? '';
  const effort = chat.effort ?? catalogue?.defaultEffort ?? 'xhigh';
  const known = models.some((m) => m.id === model);

  return (
    <span className="picker">
      <select aria-label="Model" data-testid="model-picker" value={model} onChange={(e) => set({ model: e.target.value })}>
        {!known && model && (
          <option value={model} disabled>
            {model} (not in the current model list)
          </option>
        )}
        {models.map((m) => (
          <option key={m.id} value={m.id} title={m.description}>
            {m.name}
          </option>
        ))}
      </select>
      <select aria-label="Effort" data-testid="effort-picker" value={effort} onChange={(e) => set({ effort: e.target.value })}>
        {(catalogue?.efforts ?? ['low', 'medium', 'high', 'xhigh', 'max']).map((x) => (
          <option key={x} value={x}>
            {x}
          </option>
        ))}
      </select>
      {error && <span className="error small-text">{error}</span>}
    </span>
  );
}
