// Settings → Agents: the agents Helm offers every chat. An agent is one Markdown file: a front
// matter block with a name, a description and optionally the tools and model it uses, then its
// instructions. Chats get the whole folder at their next start, by plain name (the names the
// Orchestrator's stages point at). Adding copies files in; removing deletes Helm's copy only.
// Like skills and rules, agents come in layers: Everyone's, and one list per profile (a profile's
// agent wins over Everyone's when both have the same name).

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, ConfirmDialog, SettingRow, type ConfirmSpec } from '../../ui/index.ts';
import { agentsOps, type AgentsView } from './api.ts';
import { LayerPicker, MoveTo, useKnowledgeLayer } from './LayerPicker.tsx';
import './skills.css';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export function AgentsPane() {
  const shell = useShell();
  const ops = useMemo(() => agentsOps(shell.api), [shell.api]);
  const state = useKnowledgeLayer();
  const { layer } = state;
  const [view, setView] = useState<AgentsView | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);

  const load = useCallback(
    () =>
      ops
        .list(layer)
        .then(setView)
        .catch((e: unknown) => setError(errorText(e))),
    [ops, layer],
  );

  useEffect(() => {
    setView(null);
    void load();
    const offs = [shell.bus.on('agents-changed', () => void load()), shell.bus.on('knowledge-changed', () => void load())];
    return () => offs.forEach((off) => off());
  }, [shell, load]);

  /** Run one action: clear the old messages, show its error if it fails. */
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  // The picker chooses folders only, so a folder of agent files is what gets added (the op also
  // takes the path of a single file).
  const addFromFolder = () =>
    run(async () => {
      const picked = await shell.api<{ path: string | null }>('folder_pick', { title: 'Folder with the agent files to add' });
      if (!picked.path) return;
      const r = await ops.add(picked.path, layer);
      const skipped = r.skipped.length > 0 ? ` Left out: ${r.skipped.join('; ')}.` : '';
      setNote(`Added ${r.added.join(', ')}. Chats pick them up from their next start.${skipped}`);
      await load();
    });

  const askRemove = (name: string) =>
    setConfirm({
      title: 'Remove the agent?',
      body: `Helm deletes its copy of ${name}. The file you added it from is left alone. A stage in Settings → Orchestrator that names it will have no agent to start.`,
      confirm: 'Remove',
      danger: true,
      onConfirm: async () => {
        try {
          await ops.remove(name, layer);
          setNote(`Removed ${name}.`);
          await load();
        } catch (e) {
          setError(errorText(e));
        }
      },
    });

  return (
    <div className="skills-pane" data-testid="settings-agents">
      <SettingRow title="Whose agents" description="Pick whose list to look at.">
        <LayerPicker state={state} />
      </SettingRow>
      <SettingRow
        testId="agents-folder"
        title="Helm's agents folder"
        description="An agent is a Markdown file: front matter with a name and a description (and optionally tools and model), then its instructions. Helm hands this folder to every chat start, so the agents are known by their plain names, which is how the Orchestrator's stages find them. The examples are yours to edit or remove; a removed example does not come back."
      >
        <div className="skills-actions">
          <Button variant="primary" data-testid="agents-add" disabled={busy} onClick={() => void addFromFolder()}>
            Add from folder
          </Button>
          <Button variant="secondary" data-testid="agents-open" disabled={busy} onClick={() => void run(() => ops.reveal(layer).then(() => undefined))}>
            Open folder
          </Button>
        </div>
        {view && view.agents.length === 0 && (
          <div className="setting-desc" data-testid="agents-empty">
            No agents yet. Add a folder of agent files and Helm starts passing them to chats.
          </div>
        )}
      </SettingRow>
      {view?.agents.map((a) => (
        <div className="skills-row" key={a.name} data-testid={`agent-row-${a.name}`}>
          <div className="skills-what">
            <div className="skills-name">{a.name}</div>
            {a.description !== '' && <div className="skills-desc">{a.description}</div>}
          </div>
          <MoveTo
            options={state.options}
            from={layer}
            what={a.name}
            testId={`agent-move-${a.name}`}
            disabled={busy}
            onMove={(to) =>
              void run(async () => {
                await shell.api('knowledge_move', { kind: 'agent', name: a.name, from: layer, to });
                setNote(`Moved ${a.name}.`);
                await load();
              })
            }
          />
          <Button variant="text" danger data-testid={`agent-remove-${a.name}`} disabled={busy} onClick={() => askRemove(a.name)}>
            Remove
          </Button>
        </div>
      ))}
      {note && (
        <div className="setting-desc" role="status" data-testid="agents-note">
          {note}
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="agents-error">
          {error}
        </div>
      )}
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}
