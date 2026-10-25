// Settings â†’ Keyboard: every command in the key registry with its
// chord. Change records the next chord pressed; Clear unbinds; Reset returns to the default. Each
// change saves the user's map at once through the generic `preference` op.

import { useState, useSyncExternalStore } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, chordOf, isAssignable, keys, KEYBINDINGS_PREF, TextInput, toast, useLayer } from '../../ui/index.ts';
import { IconSearch, IconWarning } from '../../ui/icons.tsx';
import { registerSettingsPane } from '../registry.ts';
import { conflictedIds, groupKeyboardRows, keyboardRows } from './model.ts';
import './keyboard.css';

function KeyboardPane() {
  const shell = useShell();
  useSyncExternalStore(keys.subscribe, keys.getVersion);
  const [query, setQuery] = useState('');
  const [capturing, setCapturing] = useState<string | null>(null);
  const [problem, setProblem] = useState<{ id: string; text: string } | null>(null);
  // Escape while listening cancels the capture, not the Settings dialog (the Escape router runs
  // before the field sees the key, so the capture holds a layer above the dialog's).
  useLayer('popover', capturing !== null, () => setCapturing(null));
  const rows = keyboardRows(keys.list(), query);
  const clashing = conflictedIds(keys.conflicts());

  const save = (map: Record<string, string | null>) =>
    shell.api('preference', { key: KEYBINDINGS_PREF, value: map }).catch((e: unknown) => {
      toast.show({ kind: 'error', text: `Could not save the shortcut: ${String((e as Error).message ?? e)}` });
    });
  const set = (id: string, chord: string | null) => {
    const r = keys.setUser(id, chord);
    if (!r.ok) {
      setProblem({ id, text: r.reason });
      return;
    }
    setProblem(null);
    void save(r.map);
  };

  return (
    <div className="kb-pane" data-testid="settings-keyboard">
      <div className="kb-search">
        <IconSearch />
        <TextInput placeholder="Find a shortcut" aria-label="Find a shortcut" value={query} onChange={(e) => setQuery(e.target.value)} />
      </div>
      <div className="kb-list">
        {!query.trim() && <p className="kb-hint">Click a shortcut to change it.</p>}
        {groupKeyboardRows(rows).map((g) => (
          <section key={g.section} className="kb-section" aria-label={g.section}>
            <h4 className="kb-section-title">{g.section}</h4>
            <div role="list">
              {g.rows.map((b) => (
                <div key={b.id} className="kb-row" role="listitem" data-testid="kb-row" data-command={b.id}>
                  <div className="kb-title">
                    <span>{b.title}</span>
                    {clashing.has(b.id) && (
                      <span className="kb-clash" title="Another command uses the same shortcut">
                        <IconWarning size={12} />
                      </span>
                    )}
                    {problem?.id === b.id && (
                      <span className="kb-problem" role="alert">
                        {problem.text}
                      </span>
                    )}
                  </div>
                  <div className="kb-actions">
                    {b.chord && (
                      <Button variant="text" onClick={() => set(b.id, null)}>
                        Clear
                      </Button>
                    )}
                    {b.custom && (
                      <Button
                        variant="text"
                        onClick={() => {
                          setProblem(null);
                          void save(keys.reset(b.id));
                        }}
                      >
                        Reset
                      </Button>
                    )}
                  </div>
                  {capturing === b.id ? (
                    <button
                      type="button"
                      className="kb-capture"
                      autoFocus
                      onBlur={() => setCapturing(null)}
                      onKeyDown={(e) => {
                        // This field owns every key while it listens: nothing else may act on it.
                        e.preventDefault();
                        e.stopPropagation();
                        const chord = chordOf(e);
                        if (!chord) return;
                        if (!isAssignable(chord)) {
                          setProblem({ id: b.id, text: `${chord}: use Ctrl or Alt with a key, or a function key.` });
                          return;
                        }
                        setCapturing(null);
                        set(b.id, chord);
                      }}
                    >
                      Press the new shortcut…
                    </button>
                  ) : (
                    <button
                      type="button"
                      className={`kb-chord${b.chord ? '' : ' unset'}`}
                      data-testid="kb-change"
                      title="Click to change"
                      aria-label={`${b.title}: ${b.chord ?? 'not set'}. Change`}
                      onClick={() => {
                        setProblem(null);
                        setCapturing(b.id);
                      }}
                    >
                      {b.chord ?? 'Not set'}
                    </button>
                  )}
                </div>
              ))}
            </div>
          </section>
        ))}
        {rows.length === 0 && <p className="kb-empty">No shortcut matches.</p>}
      </div>
    </div>
  );
}

registerSettingsPane({ id: 'keyboard', title: 'Keyboard', order: 10, render: () => <KeyboardPane /> });
