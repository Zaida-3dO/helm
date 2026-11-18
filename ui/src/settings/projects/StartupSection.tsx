// Settings → Projects → Startup: what every new chat on a project is given first. The
// instructions are done in order before any other work; the context files are listed in the first
// message and read when the work needs them; the MCP servers are the ones Helm's own MCP tools
// find in the project's chats (none ticked leaves every server available, as for a project that
// names none). The default modes are the ones a new chat starts with already on.

import { useEffect, useMemo, useState } from 'react';
import { mcpOps, type McpServer } from '../../api/mcp.ts';
import { projectOps, type ProjectNode } from '../../api/projects.ts';
import { modeOps } from '../../modes/api.ts';
import { useShell } from '../../shell/context.ts';
import { Button } from '../../ui/index.ts';
import { changes, formOf, problem, STARTUP_MAX, type StartupForm } from './startup.ts';

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

export function StartupSection({ projects, onSaved }: { projects: ProjectNode[]; onSaved: () => void }) {
  const shell = useShell();
  const ops = useMemo(() => projectOps(shell.api), [shell.api]);
  const mcp = useMemo(() => mcpOps(shell.api), [shell.api]);
  const choices = useMemo(
    () => [...projects.filter((p) => p.kind === 'board'), ...projects.filter((p) => p.kind !== 'board')].filter((p) => !p.closed && !p.archived),
    [projects],
  );
  const [id, setId] = useState('');
  const [servers, setServers] = useState<McpServer[]>([]);
  const [modeNames, setModeNames] = useState<string[]>([]);
  const [form, setForm] = useState<StartupForm>({ instructions: '', files: '', servers: [], modes: [] });
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const node = choices.find((p) => p.id === id) ?? null;

  useEffect(() => {
    let live = true;
    mcp
      .get()
      .then((s) => live && setServers(s.servers))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [mcp]);

  // The modes a chat could be given: the ones the home person's chats get, from every list.
  useEffect(() => {
    let live = true;
    const read = () =>
      modeOps(shell.api)
        .list()
        .then((r) => live && setModeNames(r.modes.map((m) => m.name)))
        .catch(() => undefined);
    void read();
    const off = shell.bus.on('knowledge-changed', () => void read());
    return () => {
      live = false;
      off();
    };
  }, [shell]);

  // The form follows the project chosen, and its saved state after a save.
  useEffect(() => {
    if (node) setForm(formOf(node));
  }, [node?.id, node?.revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const wrong = problem(form);
  const update = node ? changes(node, form) : null;

  const save = async () => {
    if (!update) return;
    setError(null);
    setSaved(false);
    try {
      await ops.update(update);
      setSaved(true);
      onSaved();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const toggleMode = (name: string) =>
    setForm((f) => ({ ...f, modes: f.modes.includes(name) ? f.modes.filter((m) => m !== name) : [...f.modes, name] }));
  // A saved default whose file is gone stays listed (ticked), so it can be unticked.
  const shownModes = [...modeNames, ...form.modes.filter((m) => !modeNames.includes(m))];

  const toggle = (serverId: string) =>
    setForm((f) => ({ ...f, servers: f.servers.includes(serverId) ? f.servers.filter((s) => s !== serverId) : [...f.servers, serverId] }));

  return (
    <section className="sp-section sp-startup" aria-labelledby="sp-startup" data-testid="settings-startup">
      <h3 id="sp-startup">Startup</h3>
      <p className="sp-help">
        What every new chat on a project is given first. A chat started on a board uses the board's startup, or its folder's when the
        board has none. They apply to chats started after you save.
      </p>
      <label className="sp-row">
        <span>Project</span>
        <select data-testid="startup-project" value={id} onChange={(e) => (setId(e.target.value), setSaved(false), setError(null))}>
          <option value="">Choose a project…</option>
          {choices.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
              {p.kind === 'board' ? '' : ' (folder)'}
            </option>
          ))}
        </select>
        <span />
      </label>
      {node && (
        <>
          <label className="sp-field">
            <span>Instructions</span>
            <textarea
              data-testid="startup-instructions"
              rows={6}
              maxLength={STARTUP_MAX}
              value={form.instructions}
              placeholder="Make a worktree and branch, start the local servers, send me the links…"
              onChange={(e) => setForm({ ...form, instructions: e.target.value })}
            />
          </label>
          <label className="sp-field">
            <span>Context files</span>
            <textarea
              data-testid="startup-files"
              rows={3}
              spellCheck={false}
              value={form.files}
              placeholder="One absolute path per line"
              onChange={(e) => setForm({ ...form, files: e.target.value })}
            />
          </label>
          <fieldset className="sp-field" data-testid="startup-servers">
            <legend>MCP servers</legend>
            {servers.length === 0 && <p className="sp-help">No MCP servers are set up in Settings → MCP.</p>}
            {servers.map((s) => (
              <label key={s.id} className="sp-check">
                <input type="checkbox" data-testid={`startup-server-${s.id}`} checked={form.servers.includes(s.id)} onChange={() => toggle(s.id)} />
                <span>{s.name || s.id}</span>
                {!s.enabled && <span className="sp-meta"> · off</span>}
              </label>
            ))}
            {form.servers.length > 0 && <p className="sp-help">Helm's own MCP tools find only the ticked servers in this project's chats. Servers that Claude Code loads by itself are not narrowed.</p>}
          </fieldset>
          <fieldset className="sp-field" data-testid="startup-modes">
            <legend>Default modes</legend>
            {shownModes.length === 0 && <p className="sp-help">No modes are set up yet. Add them in Settings → Skills &amp; rules.</p>}
            {shownModes.map((name) => (
              <label key={name} className="sp-check">
                <input type="checkbox" data-testid={`startup-mode-${name}`} checked={form.modes.includes(name)} onChange={() => toggleMode(name)} />
                <span>{name}</span>
                {!modeNames.includes(name) && <span className="sp-meta"> · no such mode</span>}
              </label>
            ))}
            {shownModes.length > 0 && <p className="sp-help">New chats on this project start with the ticked modes on; a chat can turn them off.</p>}
          </fieldset>
          <div className="sp-actions">
            <Button disabled={!update || wrong !== null} onClick={() => void save()} data-testid="startup-save">
              Save
            </Button>
            <Button disabled={!update} onClick={() => node && setForm(formOf(node))} data-testid="startup-revert">
              Revert
            </Button>
          </div>
          {(wrong ?? error) && (
            <p className="sp-error" data-testid="startup-error">
              {wrong ?? error}
            </p>
          )}
          {saved && !update && <p className="sp-help">Saved.</p>}
        </>
      )}
    </section>
  );
}
