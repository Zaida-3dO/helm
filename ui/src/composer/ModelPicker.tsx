// The unified model and effort picker: one compact trigger
// reading "Model · Effort", a searchable list of models and an effort row in its footer. Up/Down
// move through the models, Left/Right step the effort, Enter commits both; a click on a model or
// an effort stop commits it at once. Ctrl+Shift+M opens it.
// A change applies from the next turn (the host acknowledges within 10 s); a failed save shows an
// error beside the trigger.
//
// Built on the kit's shared picker popover, with the effort row in its footer slot. Under it sits
// a reply-length row (a chat's own length; it applies when the chat's agent next starts) and, in a
// chat, "Set as default for new chats". Settings → Runtime uses the same picker for the new-chat
// default, without that last action and with "Claude Code's default" as a model, so nothing else
// chooses model, effort or reply length.
//
// Claude's models come first, then the local models under their own heading, each with its server
// and context size. A server that did not answer greys its rows. A chat can change its model at
// any time, between Claude and a local server in either direction: the next turn runs on the new
// model with the conversation carried over. Where the host says the conversation is larger than
// the chosen model's window, the error comes with a "Roll over into it" offer: a fresh chat on
// that model starting from a hand-off summary. A local model has no effort setting, so the same
// stops are labelled as thinking.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { ChatRecord, ModelCatalogue } from '../api/types.ts';
import { DEFAULT_MODEL, VERBOSITY_OPTIONS, type RuntimeSettings } from '../settings/runtime/model.ts';
import { useShell } from '../shell/context.ts';
import { rollOverChat } from '../sidebar/actions.ts';
import { ConfirmDialog, Segmented, type ConfirmSpec } from '../ui/index.ts';
import { useBinding } from '../ui/hooks.ts';
import { IconChevronDown, IconStar } from '../ui/icons.tsx';
import { PickerPopover, type PickerOption } from '../ui/Picker.tsx';
import { commitPatch, effortLabel, FALLBACK_EFFORTS, isLocalModel, localHint, modelRows, stepEffort, thinkingLabel, triggerLabel, withDefaultRow } from './modelEffort.ts';

let cached: Promise<ModelCatalogue> | null = null;

/** The reply lengths, in order, for stepping with Alt+←/→. */
const LEVELS = VERBOSITY_OPTIONS.map((o) => o.value);

/** Where a picker reads its choice from and writes it to: a chat, or the new-chat default. */
export interface PickerSource {
  model: string | null;
  effort: string | null;
  /** Apply what changed; a rejection shows beside the trigger. A `model` of `DEFAULT_MODEL` is
   *  "Claude Code's default" (only offered with `allowDefaultModel`). */
  commit: (patch: { model?: string; effort?: string }) => Promise<unknown>;
  /** Offer "Claude Code's default" as a model; a null `model` then reads as that choice. */
  allowDefaultModel?: boolean;
  /** The reply length in force (`low`, `medium` or `high`), null while it is not known yet. */
  verbosity: string | null;
  /** True when the chat has its own reply length rather than following the default. */
  verbosityOwn?: boolean;
  setVerbosity: (level: string) => Promise<unknown>;
  /** Drops the chat's own reply length so it follows the default again (only in a chat). */
  followDefault?: () => Promise<unknown>;
  /** Said under the reply-length row. */
  verbosityNote?: string;
  /** "Set as default for new chats": only in a chat. `isDefault` greys it. */
  makeDefault?: { isDefault: boolean; save: () => Promise<unknown> };
}

/** The title of a reply length: "Short", "Medium" or "Full". */
function verbosityTitle(level: string | null): string {
  return VERBOSITY_OPTIONS.find((o) => o.value === level)?.title ?? '';
}

/** The chat's model and effort picker. A chat can change its model at any time; where the host
 *  says the conversation will not fit the chosen model's window, the offer beside the error rolls
 *  the chat over into it. */
export function ModelPicker({ chat, ops, loading = false }: { chat: ChatRecord; ops: ChatOps; loading?: boolean }) {
  const shell = useShell();
  const [verbosity, setVerbosity] = useState<{ level: string; own: boolean } | null>(null);
  const [saved, setSaved] = useState<RuntimeSettings | null>(null);
  useEffect(() => {
    let live = true;
    shell
      .api<{ level: string; own: boolean }>('chat_verbosity_get', { chatId: chat.id })
      .then((v) => live && setVerbosity(v))
      .catch(() => undefined);
    shell
      .api<RuntimeSettings>('runtime_settings_get', {})
      .then((s) => live && setSaved(s))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [shell, chat.id]);
  const isDefault =
    !!saved &&
    !!chat.model &&
    saved.model === chat.model &&
    saved.effort === (chat.effort ?? saved.effort) &&
    (!verbosity || saved.responseVerbosity === verbosity.level);
  const source: PickerSource = {
    model: chat.model ?? null,
    effort: chat.effort ?? null,
    commit: (patch) => ops.setModel(chat.id, patch.model, patch.effort),
    verbosity: verbosity?.level ?? null,
    verbosityOwn: verbosity?.own,
    setVerbosity: (level) => shell.api<{ level: string; own: boolean }>('chat_verbosity_set', { chatId: chat.id, level }).then(setVerbosity),
    followDefault: () => shell.api<{ level: string; own: boolean }>('chat_verbosity_set', { chatId: chat.id, level: null }).then(setVerbosity),
    verbosityNote: 'Applies when this chat’s agent next starts.',
    makeDefault: chat.model
      ? {
          isDefault,
          save: () =>
            shell
              .api<RuntimeSettings>('runtime_settings_save', {
                model: chat.model,
                ...(chat.effort ? { effort: chat.effort } : {}),
                ...(verbosity ? { responseVerbosity: verbosity.level } : {}),
              })
              .then(setSaved),
        }
      : undefined,
  };
  return <ModelEffortPicker chat={chat} ops={ops} source={source} shortcut={!chat.sideSourceId} loading={loading} />;
}

/** Settings → Runtime's "Model and effort for new chats": the same picker, saving the default. */
export function DefaultModelPicker({
  ops,
  settings,
  save,
}: {
  ops: ChatOps;
  settings: RuntimeSettings | null;
  save: (patch: { model?: string; effort?: string; responseVerbosity?: string }) => Promise<unknown>;
}) {
  const source: PickerSource = {
    model: settings?.model ?? null,
    effort: settings?.effort ?? null,
    commit: (patch) => save(patch),
    allowDefaultModel: true,
    verbosity: settings?.responseVerbosity ?? null,
    verbosityOwn: true,
    setVerbosity: (level) => save({ responseVerbosity: level }),
    verbosityNote: 'The reply length a new chat starts with.',
  };
  return <ModelEffortPicker ops={ops} source={source} settingsRow />;
}

function ModelEffortPicker({
  chat,
  ops,
  source,
  shortcut = false,
  settingsRow = false,
  loading = false,
}: {
  chat?: ChatRecord;
  ops: ChatOps;
  source: PickerSource;
  /** Only the main chat's composer takes Ctrl+Shift+M. */
  shortcut?: boolean;
  /** Drawn as a Settings row's control rather than a composer chip. */
  settingsRow?: boolean;
  /** The app is still checking its runtimes: the chip says so, muted, instead of a model name. */
  loading?: boolean;
}) {
  const [catalogue, setCatalogue] = useState<ModelCatalogue | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A model change the host refused as too large for the target's window: the id it refused, so
  // the offer can roll the chat over into that model.
  const [tooLarge, setTooLarge] = useState<string | null>(null);
  const [rollSpec, setRollSpec] = useState<ConfirmSpec | null>(null);
  const [open, setOpen] = useState(false);
  const [checking, setChecking] = useState(false);
  const [checked, setChecked] = useState(false);
  const [notes, setNotes] = useState<string[]>([]);
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
  // A chat with no model of its own shows the catalogue's first; the new-chat default with none
  // is "Claude Code's default".
  const model = source.model ?? (source.allowDefaultModel ? DEFAULT_MODEL : catalogue?.models[0]?.id) ?? '';
  const effort = source.effort ?? catalogue?.defaultEffort ?? 'xhigh';
  const rows = useMemo(() => {
    const listed = modelRows(catalogue, model === DEFAULT_MODEL ? null : model || null);
    return source.allowDefaultModel ? withDefaultRow(listed, DEFAULT_MODEL) : listed;
  }, [catalogue, model, source.allowDefaultModel]);
  const options: PickerOption<string>[] = useMemo(
    () => rows.map((r) => ({ value: r.id, title: r.title, text: `${r.title} ${r.id} ${r.detail}`, subtitle: r.detail, disabled: r.disabled, group: r.group, hint: r.hint })),
    [rows],
  );
  const current = rows.find((r) => r.id === model);
  // The chat's effort is a thinking level when the model is a local one.
  const local = isLocalModel(model);
  const stopLabel = local ? thinkingLabel : effortLabel;
  // While the app is still checking its runtimes the chip says so, muted, instead of a model name.
  const label = loading ? 'Loading…' : triggerLabel(current?.title ?? (model || 'Default'), effort, local);
  // A chat with its own reply length says so after the effort.
  const shown = !loading && source.verbosityOwn && !settingsRow && source.verbosity ? `${label} · ${verbosityTitle(source.verbosity)}` : label;

  const close = (focus = true) => {
    setOpen(false);
    if (focus) trigger.current?.focus();
  };
  const openPicker = () => {
    setEffortDraft(effort);
    setOpen(true);
  };
  const failed = useCallback((e: unknown) => setError(String((e as Error).message ?? e)), []);
  const setLength = (level: string) => {
    setError(null);
    source.setVerbosity(level).catch(failed);
  };

  /**
   * The only thing here that contacts the local servers, once per click: opening the picker never
   * does. The list shows what they said the last time they were asked.
   */
  const checkServers = () => {
    setChecking(true);
    setError(null);
    ops
      .refreshLocalModels()
      .then(({ models, notes: seen }) => {
        setChecked(true);
        setNotes(seen ?? []);
        cached = (cached ?? ops.catalogue()).then((c) => ({ ...c, local: models }));
        return cached.then(setCatalogue);
      })
      .catch(failed)
      .finally(() => setChecking(false));
  };

  const commit = (id: string | undefined, chosenEffort = effortDraft) => {
    const row = rows.find((r) => r.id === id);
    const next = { model: row && !row.disabled ? row.id : model, effort: chosenEffort || effort };
    close();
    const patch = commitPatch({ model, effort }, next);
    if (!patch) return;
    setError(null);
    setTooLarge(null);
    source.commit(patch).catch((e: unknown) => {
      failed(e);
      // The host measured the conversation against the new model's window and refused: offer
      // the roll over, which starts a fresh chat on that model from a hand-off summary.
      if ((e as { code?: unknown })?.code === 'too_large' && patch.model) setTooLarge(patch.model);
    });
  };

  /** The roll over this chat's too-large refusal points at, confirmed before anything runs. */
  const offerRollOver = () => {
    const target = tooLarge;
    if (!target || !chat) return;
    const name = rows.find((r) => r.id === target)?.title ?? target;
    setRollSpec({
      title: 'Roll over into this model?',
      body:
        `The conversation is larger than ${name} can take into its window. The agent in “${chat.title || 'Untitled chat'}” ` +
        'writes a short hand-off, a new chat on the chosen model starts from it, and this chat is archived. ' +
        'Running work in this chat is not carried over.',
      confirm: 'Roll over',
      onConfirm: () =>
        rollOverChat(ops, chat.id, (m) => setError(m), target).then((ok) => {
          if (ok) setTooLarge(null);
        }),
    });
  };

  return (
    <span
      className="model-picker"
      onKeyDown={(e) => {
        // Left and Right step the effort while the popover is open (its own keys cover the list);
        // with Alt they step the reply length. A key the reply-length control already took is not
        // taken twice.
        if (open && !e.defaultPrevented && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
          e.preventDefault();
          const dir = e.key === 'ArrowRight' ? 1 : -1;
          if (e.altKey) {
            const next = stepEffort(LEVELS, source.verbosity ?? 'low', dir);
            if (next !== source.verbosity) setLength(next);
          } else {
            setEffortDraft((x) => stepEffort(efforts, x || effort, dir));
          }
        }
      }}
    >
      {/* One registration per binding id: only the main chat's composer takes the shortcut, so a
          side chat closing cannot remove it. */}
      {shortcut && <PickerShortcut run={() => (open ? close() : openPicker())} />}
      <button
        ref={trigger}
        type="button"
        className={settingsRow ? 'ui-picker-trigger settings' : `ui-picker-trigger compact${loading ? ' loading' : ''}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`Model and effort: ${shown}`}
        title={`${shown} · ${local ? 'Local model' : 'Claude Code'}${shortcut ? ' · Ctrl+Shift+M' : ''}`}
        data-testid={settingsRow ? 'runtime-model' : 'model-picker'}
        onClick={() => (open ? close() : openPicker())}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            openPicker();
          }
        }}
      >
        <span className="ui-picker-value">{shown}</span>
        <span className={`ui-picker-chevron${open ? ' open' : ''}`}>
          <IconChevronDown size={12} />
        </span>
      </button>
      {error && (
        <span className="model-picker-error" role="alert">
          {error}
        </span>
      )}
      {tooLarge && chat && (
        <button type="button" className="model-picker-link" data-testid="model-roll-over-offer" onClick={offerRollOver}>
          Roll over into it
        </button>
      )}
      {rollSpec && <ConfirmDialog spec={rollSpec} onClose={() => setRollSpec(null)} />}
      {open && (
        <PickerPopover
          // Placed once when it mounts: a list that grows after a check (more rows) must be placed again.
          key={options.length}
          anchor={trigger}
          value={model}
          options={options}
          label="Model"
          heading="Model and effort"
          searchable
          searchPlaceholder="Find a model…"
          className="model-picker-pop"
          alignEnd={settingsRow}
          testId="model-picker-pop"
          onClose={() => close()}
          onPick={(id) => commit(id)}
          extraHint={
            <span>
              ← → {local ? 'thinking' : 'effort'} · Alt ← → replies
            </span>
          }
          footer={
            <>
              <div className="model-picker-local" data-testid="local-check">
                <span className="model-picker-local-hint" data-testid="local-hint">
                  {localHint(catalogue, checked, notes)}
                </span>
                <button type="button" className="model-picker-link" data-testid="check-servers" disabled={checking} onClick={checkServers}>
                  {checking ? 'Checking…' : 'Check servers'}
                </button>
              </div>
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
              <div className="model-picker-verbosity" data-testid="verbosity-picker">
                <span className="model-picker-caption">Replies</span>
                <Segmented<string>
                  label="Reply length"
                  testId="verbosity-segments"
                  value={source.verbosity ?? 'low'}
                  options={VERBOSITY_OPTIONS.map((o) => ({ value: o.value, label: o.title }))}
                  onChange={setLength}
                />
                {source.followDefault && source.verbosityOwn && (
                  <button
                    type="button"
                    className="model-picker-link"
                    data-testid="verbosity-follow-default"
                    onClick={() => {
                      setError(null);
                      source.followDefault?.().catch(failed);
                    }}
                  >
                    Follow default
                  </button>
                )}
              </div>
              {source.verbosityNote && <div className="model-picker-note">{source.verbosityNote}</div>}
              {source.makeDefault && (
                <div className="model-picker-default">
                  <button
                    type="button"
                    className="model-picker-link"
                    data-testid="make-default"
                    aria-pressed={source.makeDefault.isDefault}
                    disabled={source.makeDefault.isDefault}
                    onClick={() => {
                      setError(null);
                      source.makeDefault?.save().catch(failed);
                    }}
                  >
                    <IconStar size={12} filled={source.makeDefault.isDefault} />{' '}
                    {source.makeDefault.isDefault ? 'This is the default for new chats' : 'Set as default for new chats'}
                  </button>
                </div>
              )}
            </>
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
