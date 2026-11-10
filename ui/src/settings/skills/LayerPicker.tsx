// The layer picker shared by Settings → Skills & rules and Settings → Agents: a segmented choice
// between Everyone and each profile, opening on the active profile. Also the "Move to" menu.

import { useMemo, useState } from 'react';
import { useProfiles } from '../../profiles/hooks.ts';
import { useShell } from '../../shell/context.ts';
import { Picker } from '../../ui/index.ts';
import { currentLayer, defaultLayer, layerOptions, moveTargets, type LayerOption } from './layers.ts';

export interface KnowledgeLayerState {
  options: LayerOption[];
  /** The layer being shown. */
  layer: string;
  setLayer: (key: string) => void;
}

/** The layers that exist (Everyone and every profile) and the one being shown. */
export function useKnowledgeLayer(): KnowledgeLayerState {
  const shell = useShell();
  const { snap } = useProfiles(shell.api, shell.bus);
  const view = snap.view;
  const options = useMemo(() => layerOptions(view?.profiles ?? []), [view]);
  const [picked, setPicked] = useState<string | null>(null);
  const fallback = defaultLayer(options, view?.activeId ?? null, view?.homeId ?? null);
  return { options, layer: currentLayer(options, picked, fallback), setLayer: setPicked };
}

export function LayerPicker({ state }: { state: KnowledgeLayerState }) {
  return (
    <div className="kn-layers" role="radiogroup" aria-label="Whose list to show" data-testid="knowledge-layers">
      {state.options.map((o) => (
        <button
          key={o.key}
          type="button"
          role="radio"
          aria-checked={state.layer === o.key}
          className={`kn-layer${state.layer === o.key ? ' on' : ''}`}
          data-testid={`knowledge-layer-${o.testSuffix}`}
          onClick={() => state.setLayer(o.key)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** "Move to ▸": every other layer. Nothing is overwritten; a taken name is refused by the host. */
export function MoveTo({ options, from, what, testId, disabled, onMove }: {
  options: LayerOption[];
  from: string;
  /** What is being moved, for the control's name. */
  what: string;
  testId: string;
  disabled?: boolean;
  onMove: (to: string) => void;
}) {
  const targets = moveTargets(options, from);
  return (
    <Picker<string>
      settings
      label={`Move ${what} to`}
      heading="Move to"
      testId={testId}
      value={null}
      display="Move to…"
      options={targets.map((t) => ({ value: t.key, title: t.label }))}
      disabled={disabled || targets.length === 0}
      onChange={(to) => onMove(to)}
    />
  );
}
