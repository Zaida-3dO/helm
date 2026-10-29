// The unified model and effort picker: one compact trigger
// reading "Model · Effort", a searchable list of models and an effort row in its footer. Up/Down
// move through the models, Left/Right step the effort, Enter commits both; a click on a model or
// an effort stop commits it at once. Ctrl+Shift+M opens it.
// A change applies from the next turn (the host acknowledges within 10 s); a failed save shows an
// error beside the trigger.
//
// Built on the kit's shared picker popover, with the effort row in its footer slot.
//
// Claude's models come first, then the local models under their own heading, each with its server
// and context size. A server that did not answer greys its rows. After the chat's first turn its
// model is fixed: rows that would cross between Claude and local, or between servers, are disabled
// with a tooltip saying so. A local model has no effort setting, so the same stops are labelled as
// thinking.

import { useEffect, useMemo, useRef, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { ChatRecord, ModelCatalogue } from '../api/types.ts';
import { useBinding } from '../ui/hooks.ts';
import { IconChevronDown } from '../ui/icons.tsx';
import { PickerPopover, type PickerOption } from '../ui/Picker.tsx';
import { commitPatch, effortLabel, FALLBACK_EFFORTS, isLocalModel, modelRows, stepEffort, thinkingLabel, triggerLabel } from './modelEffort.ts';

let cached: Promise<ModelCatalogue> | null = null;

/** `started`: the chat has had its first turn, so its model can no longer move between Claude and local, or between servers. */
export function ModelPicker({ chat, ops, started = false }: { chat: ChatRecord; ops: ChatOps; started?: boolean }) {
  const [catalogue, setCatalogue] = useState<ModelCatalogue | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [effortDraft, setEffortDraft] = useState('');
  const trigger = useRef<HTMLButtonElement>(null);

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

  const efforts = catalogue?.efforts?.length ? catalogue.efforts : FALLBACK_EFFORTS;
  const model = chat.model ?? catalogue?.models[0]?.id ?? '';
  const effort = chat.effort ?? catalogue?.defaultEffort ?? 'xhigh';
  const rows = useMemo(() => modelRows(catalogue, model || null, started), [catalogue, model, started]);
  const options: PickerOption<string>[] = useMemo(
    () => rows.map((r) => ({ value: r.id, title: r.title, text: `${r.title} ${r.id} ${r.detail}`, subtitle: r.detail, disabled: r.disabled, group: r.group, hint: r.hint })),
    [rows],
  );
  const current = rows.find((r) => r.id === model);
  // The chat's effort is a thinking level when the model is a local one.
  const local = isLocalModel(model);
  const stopLabel = local ? thinkingLabel : effortLabel;
  const label = triggerLabel(current?.title ?? (model || 'Default'), effort, local);

  const close = (focus = true) => {
    setOpen(false);
    if (focus) trigger.current?.focus();
  };
  const openPicker = () => {
    setEffortDraft(effort);
    setOpen(true);
    // A local server that did not answer earlier may be up now; asking again is the user's own action.
    if (catalogue?.local?.some((m) => !m.reachable)) {
      cached = ops.catalogue(true);
      cached.then(setCatalogue).catch(() => {
        cached = null;
      });
    }
  };

  const commit = (id: string | undefined, chosenEffort = effortDraft) => {
    const row = rows.find((r) => r.id === id);
    const next = { model: row && !row.disabled ? row.id : model, effort: chosenEffort || effort };
    close();
    const patch = commitPatch({ model, effort }, next);
    if (!patch) return;
    setError(null);
    ops.setModel(chat.id, patch.model, patch.effort).catch((e: unknown) => setError(String((e as Error).message ?? e)));
  };

  return (
    <span
      className="model-picker"
      onKeyDown={(e) => {
        // Left and Right step the effort while the popover is open (its own keys cover the list).
        if (open && (e.key === 'ArrowLeft' || e.key === 'ArrowRight') && !e.altKey) {
          e.preventDefault();
          setEffortDraft((x) => stepEffort(efforts, x || effort, e.key === 'ArrowRight' ? 1 : -1));
        }
      }}
    >
      {/* One registration per binding id: only the main chat's composer takes the shortcut, so a
          side chat closing cannot remove it. */}
      {!chat.sideSourceId && <PickerShortcut run={() => (open ? close() : openPicker())} />}
      <button
        ref={trigger}
        type="button"
        className="ui-picker-trigger compact"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`Model and effort: ${label}`}
        title={`${label} · ${local ? 'Local model' : 'Claude Code'} · Ctrl+Shift+M`}
        data-testid="model-picker"
        onClick={() => (open ? close() : openPicker())}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            openPicker();
          }
        }}
      >
        <span className="ui-picker-value">{label}</span>
        <span className={`ui-picker-chevron${open ? ' open' : ''}`}>
          <IconChevronDown size={12} />
        </span>
      </button>
      {error && (
        <span className="model-picker-error" role="alert">
          {error}
        </span>
      )}
      {open && (
        <PickerPopover
          anchor={trigger}
          value={model}
          options={options}
          label="Model"
          heading="Model and effort"
          searchable
          searchPlaceholder="Find a model…"
          className="model-picker-pop"
          testId="model-picker-pop"
          onClose={() => close()}
          onPick={(id) => commit(id)}
          extraHint={<span>← → {local ? 'thinking' : 'effort'}</span>}
          footer={
            <div className="model-picker-effort" data-testid="effort-picker">
              <span className="model-picker-caption">
                {local ? 'Thinking' : 'Effort'} <kbd>←</kbd> <kbd>→</kbd>
              </span>
              <span className="model-picker-stops" role="radiogroup" aria-label={local ? 'Thinking' : 'Effort'}>
                {efforts.map((x, i) => {
                  const at = efforts.indexOf(effortDraft || effort);
                  return (
                    <button
                      key={x}
                      type="button"
                      role="radio"
                      aria-checked={i === at}
                      aria-label={stopLabel(x)}
                      title={stopLabel(x)}
                      className={`model-picker-stop${i <= at ? ' lit' : ''}${i === at ? ' on' : ''}`}
                      onClick={() => commit(model, x)}
                    >
                      <span />
                    </button>
                  );
                })}
              </span>
              <span className="model-picker-effort-value">{stopLabel(effortDraft || effort)}</span>
            </div>
          }
        />
      )}
    </span>
  );
}

function PickerShortcut({ run }: { run: () => void }) {
  useBinding({ id: 'composer.modelPicker', title: 'Open the model and effort picker', defaultKeys: 'Ctrl+Shift+M', run });
  return null;
}
