// Settings → Skills: skills Helm loads into chats. A skill is a folder holding a SKILL.md; adding
// one copies it into Helm's own skills folder, and every chat start passes that folder to Claude
// Code, so its commands appear in the `/` menu from the chat's next start. Turning one off only
// hides it from the menu: the same hiddenSkills list Settings → Chat edits. Removing deletes
// Helm's copy; the folder you added from is never touched.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, ConfirmDialog, SettingRow, toast, type ConfirmSpec } from '../../ui/index.ts';
import { parseNameList } from '../applied.ts';
import { skillsOps, type SkillsView } from './api.ts';
import './skills.css';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

const isHidden = (hidden: string[], name: string) => hidden.some((h) => h.toLowerCase() === name.toLowerCase());

export function SkillsPane() {
  const shell = useShell();
  const ops = useMemo(() => skillsOps(shell.api), [shell.api]);
  const [view, setView] = useState<SkillsView | null>(null);
  const [hidden, setHidden] = useState<string[]>([]);
  // The list as last applied, so a failed save can put the boxes and the `/` menu back.
  const hiddenRef = useRef<string[]>([]);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);

  const load = useCallback(
    () =>
      ops
        .list()
        .then(setView)
        .catch((e: unknown) => setError(errorText(e))),
    [ops],
  );

  useEffect(() => {
    void load();
    return shell.bus.on('skills-changed', () => void load());
  }, [shell, load]);

  // hiddenSkills, read directly so the boxes here match the list the `/` menu uses.
  useEffect(() => {
    const read = () =>
      shell
        .api<unknown>('preference_get', { key: 'hiddenSkills' })
        .then((v) => {
          hiddenRef.current = parseNameList(v);
          setHidden(hiddenRef.current);
        })
        .catch(() => undefined);
    void read();
    return shell.bus.on('preferences', (e) => {
      if ('hiddenSkills' in ((e as { changed?: object }).changed ?? {})) void read();
    });
  }, [shell]);

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

  const addFromFolder = () =>
    run(async () => {
      const picked = await shell.api<{ path: string | null }>('folder_pick', { title: 'Folder with the skill to add' });
      if (!picked.path) return;
      const r = await ops.add(picked.path);
      setNote(`Added ${r.added}. Chats pick it up from their next start.`);
      await load();
    });

  const askRemove = (name: string) =>
    setConfirm({
      title: 'Remove the skill?',
      body: `Helm deletes its copy of ${name}. The folder you added it from is left alone.`,
      confirm: 'Remove',
      danger: true,
      onConfirm: async () => {
        try {
          await ops.remove(name);
          setNote(`Removed ${name}.`);
          await load();
        } catch (e) {
          setError(errorText(e));
        }
      },
    });

  // On means the command shows in the `/` menu: keeping a skill is done by taking its name off
  // the hiddenSkills list. The list is saved whole, as in Settings → Chat.
  const setHiddenFor = (name: string, on: boolean) => {
    const before = hiddenRef.current;
    const next = on ? before.filter((h) => h.toLowerCase() !== name.toLowerCase()) : [...before, name];
    hiddenRef.current = next;
    setHidden(next);
    shell.bus.dispatch({ kind: 'preferences', changed: { hiddenSkills: next } });
    void shell.api('preference', { key: 'hiddenSkills', value: next }).catch((err: unknown) => {
      hiddenRef.current = before;
      setHidden(before);
      shell.bus.dispatch({ kind: 'preferences', changed: { hiddenSkills: before } });
      toast.show({ kind: 'error', text: `Could not save: ${errorText(err)}` });
    });
  };

  return (
    <div className="skills-pane" data-testid="settings-skills">
      <SettingRow
        testId="skills-folder"
        title="Helm's skills folder"
        description="A skill is a folder holding a SKILL.md: instructions Claude Code can load as a command. Helm passes this folder to every chat start, so its skills join the `/` menu from the chat's next start. Adding copies a folder in; the folder you picked is never changed."
      >
        <div className="skills-actions">
          <Button variant="primary" data-testid="skills-add" disabled={busy} onClick={() => void addFromFolder()}>
            Add from folder
          </Button>
          <Button variant="secondary" data-testid="skills-open" disabled={busy} onClick={() => void run(() => ops.reveal().then(() => undefined))}>
            Open folder
          </Button>
        </div>
        {view && view.skills.length === 0 && (
          <div className="setting-desc" data-testid="skills-empty">
            No skills yet. Add a folder with a SKILL.md and Helm starts passing it to chats.
          </div>
        )}
      </SettingRow>
      {view?.skills.map((s) => (
        <div className="skills-row" key={s.name} data-testid={`skill-row-${s.name}`}>
          <Checkbox
            checked={!isHidden(hidden, s.name)}
            aria-label={`Offer ${s.name} in chats`}
            data-testid={`skill-offer-${s.name}`}
            onChange={(on) => setHiddenFor(s.name, on)}
          />
          <div className="skills-what">
            <div className="skills-name">{s.name}</div>
            {s.description !== '' && <div className="skills-desc">{s.description}</div>}
          </div>
          <Button
            variant="text"
            data-testid={`skill-open-${s.name}`}
            disabled={busy}
            onClick={() => void run(() => ops.reveal(s.name).then(() => undefined))}
          >
            Open
          </Button>
          <Button variant="text" danger data-testid={`skill-remove-${s.name}`} disabled={busy} onClick={() => askRemove(s.name)}>
            Remove
          </Button>
        </div>
      ))}
      {note && (
        <div className="setting-desc" role="status" data-testid="skills-note">
          {note}
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="skills-error">
          {error}
        </div>
      )}
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}
