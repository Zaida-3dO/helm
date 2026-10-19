// The unified model and effort picker: one compact trigger
// reading "Model · Effort", a searchable list of models and an effort row in its footer. Up/Down
// move through the models, Left/Right step the effort, Enter commits both; a click on a model or
// an effort stop commits it at once. Ctrl+Shift+M opens it.
// A change applies from the next turn (the host acknowledges within 10 s); a failed save shows an
// error beside the trigger.
//
// Built from the kit's picker pieces (ui-picker-* styles, the layer stack, placeBelow) because the
// kit Picker has no footer slot yet; the effort row needs one.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { ChatOps } from '../api/ops.ts';
import type { ChatRecord, ModelCatalogue } from '../api/types.ts';
import { useBinding, useLayer } from '../ui/hooks.ts';
import { IconCheck, IconChevronDown } from '../ui/icons.tsx';
import { layoutSize, nextIndex, pageZoom, placeBelow, unzoom, viewportAt } from '../ui/place.ts';
import { commitPatch, effortLabel, FALLBACK_EFFORTS, filterRows, modelRows, stepEffort, triggerLabel } from './modelEffort.ts';

let cached: Promise<ModelCatalogue> | null = null;

export function ModelPicker({ chat, ops }: { chat: ChatRecord; ops: ChatOps }) {
  const [catalogue, setCatalogue] = useState<ModelCatalogue | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState('');
  const [active, setActive] = useState(-1);
  const [effortDraft, setEffortDraft] = useState('');
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);

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
  const rows = useMemo(() => modelRows(catalogue, model || null), [catalogue, model]);
  const shown = useMemo(() => filterRows(rows, q), [rows, q]);
  const current = rows.find((r) => r.id === model);
  const label = triggerLabel(current?.title ?? (model || 'Default'), effort);

  const close = (focus = true) => {
    setOpen(false);
    setQ('');
    setPos(null);
    if (focus) trigger.current?.focus();
  };
  const openPicker = () => {
    setEffortDraft(effort);
    setOpen(true);
  };
  useLayer('popover', open, () => close());
  useLayer('search', open, () => setQ(''), () => q.length > 0);

  useLayoutEffect(() => {
    if (!open) return;
    const a = trigger.current?.getBoundingClientRect();
    const z = pageZoom();
    if (a && pop.current) setPos(placeBelow(unzoom(a, z), layoutSize(pop.current), viewportAt(z), 6));
    setActive(Math.max(0, shown.findIndex((o) => o.id === model)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
  // The popover is hidden until placed, and a hidden field cannot take focus: focus it once shown.
  useEffect(() => {
    if (open && pos) search.current?.focus();
  }, [open, pos]);

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
    <span className="model-picker">
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
        title={`${label} · Claude Code · Ctrl+Shift+M`}
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
      {open &&
        createPortal(
          <>
            <div className="ui-dismiss clear" onMouseDown={() => close(false)} />
            <div
              ref={pop}
              className="ui-picker-pop searchable model-picker-pop"
              data-testid="model-picker-pop"
              style={{ left: pos?.left ?? 0, top: pos?.top ?? 0, visibility: pos ? 'visible' : 'hidden' }}
              onKeyDown={(e) => {
                if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                  e.preventDefault();
                  setActive(nextIndex(active, e.key === 'ArrowDown' ? 1 : -1, shown.map((o) => o.disabled)));
                } else if ((e.key === 'ArrowLeft' || e.key === 'ArrowRight') && !e.altKey) {
                  e.preventDefault();
                  setEffortDraft((x) => stepEffort(efforts, x || effort, e.key === 'ArrowRight' ? 1 : -1));
                } else if (e.key === 'Enter') {
                  e.preventDefault();
                  commit(shown[active]?.id);
                }
              }}
            >
              <div className="ui-picker-heading model-picker-heading">
                <span>Model and effort</span>
                <kbd>esc</kbd>
              </div>
              <input
                ref={search}
                className="ui-picker-search"
                placeholder="Find a model…"
                aria-label="Find a model"
                value={q}
                onChange={(e) => {
                  setQ(e.target.value);
                  setActive(0);
                }}
              />
              <div className="ui-picker-list" role="listbox" aria-label="Model">
                {shown.length === 0 && <div className="ui-picker-empty">No matching models</div>}
                {shown.map((o, i) => (
                  <div
                    key={o.id}
                    role="option"
                    aria-selected={o.id === model}
                    aria-disabled={o.disabled || undefined}
                    className={`ui-picker-option${i === active ? ' active' : ''}`}
                    data-value={o.id}
                    onMouseEnter={() => setActive(i)}
                    onClick={() => !o.disabled && commit(o.id)}
                  >
                    <div className="ui-picker-text">
                      <div className="ui-picker-title">{o.title}</div>
                      <div className="ui-picker-sub">{o.detail}</div>
                    </div>
                    {o.id === model && <IconCheck />}
                  </div>
                ))}
              </div>
              <div className="model-picker-effort" data-testid="effort-picker">
                <span className="model-picker-caption">
                  Effort <kbd>←</kbd> <kbd>→</kbd>
                </span>
                <span className="model-picker-stops" role="radiogroup" aria-label="Effort">
                  {efforts.map((x, i) => {
                    const at = efforts.indexOf(effortDraft || effort);
                    return (
                      <button
                        key={x}
                        type="button"
                        role="radio"
                        aria-checked={i === at}
                        aria-label={effortLabel(x)}
                        title={effortLabel(x)}
                        className={`model-picker-stop${i <= at ? ' lit' : ''}${i === at ? ' on' : ''}`}
                        onClick={() => commit(model, x)}
                      >
                        <span />
                      </button>
                    );
                  })}
                </span>
                <span className="model-picker-effort-value">{effortLabel(effortDraft || effort)}</span>
              </div>
              <div className="model-picker-hints">
                <span>↑ ↓ navigate</span>
                <span>↵ select</span>
              </div>
            </div>
          </>,
          document.body,
        )}
    </span>
  );
}

function PickerShortcut({ run }: { run: () => void }) {
  useBinding({ id: 'composer.modelPicker', title: 'Open the model and effort picker', defaultKeys: 'Ctrl+Shift+M', run });
  return null;
}
