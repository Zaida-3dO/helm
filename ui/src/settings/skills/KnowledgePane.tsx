// Settings → Skills & rules: what Helm loads into chats, one list at a time. There are two layers:
// Everyone (every chat) and one list per profile (that profile's chats, on any device). A project's
// own CLAUDE.md and .claude/skills load as well, straight from the chat's folder. Each list has its
// rules, its skills, the memories the hub keeps for it and the folders it links skills from.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { projectOps } from '../../api/projects.ts';
import { useShell } from '../../shell/context.ts';
import { ConfirmDialog, SettingRow, toast, type ConfirmSpec } from '../../ui/index.ts';
import { parseNameList } from '../applied.ts';
import { knowledgeOps, type KnowledgeLayer, type SourcesSnapshot } from './api.ts';
import { errorText, type Notice } from './common.ts';
import { LayerPicker, useKnowledgeLayer } from './LayerPicker.tsx';
import { layerLabel, EVERYONE } from './layers.ts';
import { MemoriesSection } from './MemoriesSection.tsx';
import { RulesSection, type RunAction } from './RulesSection.tsx';
import { SkillsSection } from './SkillsSection.tsx';
import { SourcesSection, type ProjectChoice } from './SourcesSection.tsx';
import './skills.css';

export function KnowledgePane() {
  const shell = useShell();
  const ops = useMemo(() => knowledgeOps(shell.api), [shell.api]);
  const state = useKnowledgeLayer();
  const { layer, options } = state;
  const [data, setData] = useState<KnowledgeLayer | null>(null);
  const [sources, setSources] = useState<SourcesSnapshot | null>(null);
  const [projects, setProjects] = useState<ProjectChoice[]>([]);
  const [hidden, setHidden] = useState<string[]>([]);
  // The list as last applied, so a failed save can put the boxes and the `/` menu back.
  const hiddenRef = useRef<string[]>([]);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const seq = useRef(0);

  // The shown layer and the linked folders. A read that a newer one overtook is dropped.
  const load = useCallback(() => {
    const mine = ++seq.current;
    return Promise.all([
      ops.get(layer),
      // An unreadable folder list must not hide the rules and skills; its section stays off.
      ops.sources().catch((e: unknown) => {
        if (mine === seq.current) setNotice({ kind: 'error', text: errorText(e) });
        return null;
      }),
    ])
      .then(([d, s]) => {
        if (mine !== seq.current) return;
        setData(d);
        setSources(s);
      })
      .catch((e: unknown) => {
        if (mine === seq.current) setNotice({ kind: 'error', text: errorText(e) });
      });
  }, [ops, layer]);

  useEffect(() => {
    setData(null);
    void load();
    return shell.bus.on('knowledge-changed', () => void load());
  }, [shell, load]);

  // The projects a linked folder can be limited to.
  useEffect(() => {
    projectOps(shell.api)
      .list()
      .then((list) => setProjects(list.filter((p) => !p.archived).map((p) => ({ id: p.id, name: p.name }))))
      .catch(() => undefined);
  }, [shell]);

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

  /** Run one action: clear the old message, show what it returns, or the reason it failed. */
  const run: RunAction = async (action) => {
    setBusy(true);
    setNotice(null);
    try {
      const text = await action();
      if (typeof text === 'string') setNotice({ kind: 'note', text });
      await load();
    } catch (e) {
      setNotice({ kind: 'error', text: errorText(e) });
    } finally {
      setBusy(false);
    }
  };

  // On means the command shows in the `/` menu: keeping a skill is done by taking its name off the
  // hiddenSkills list. The list is saved whole, as in Settings → Chat.
  const setOffered = (name: string, on: boolean) => {
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

  const projectNames = useMemo(() => new Map(projects.map((p) => [p.id, p.name])), [projects]);
  const shared = { ops, layer, options, busy, run, confirm: setConfirm };
  const who = layerLabel(options, layer);

  return (
    <div className="skills-pane" data-testid="settings-knowledge">
      <SettingRow
        title="Skills & rules"
        description="Rules are instructions added to every chat's starting prompt; skills are commands Claude Code can load. Pick whose list to look at. Everyone's list reaches every chat; a person's list reaches chats on their profile, on any device. A project's own CLAUDE.md and .claude/skills load as well."
      >
        <LayerPicker state={state} />
      </SettingRow>
      <div className="setting-desc" data-testid="knowledge-layer-note">
        {layer === EVERYONE ? 'Loaded into every chat on this Helm.' : `Loaded into ${who}'s chats only, on any device.`}
      </div>
      {notice && (
        <div
          className={notice.kind === 'error' ? 'ui-field-error' : 'setting-desc'}
          role={notice.kind === 'error' ? 'alert' : 'status'}
          data-testid={notice.kind === 'error' ? 'knowledge-error' : 'knowledge-note'}
        >
          {notice.text}
        </div>
      )}
      {data && (
        <>
          <RulesSection {...shared} rules={data.rules} />
          <SkillsSection
            {...shared}
            skills={data.skills}
            clashes={data.clashes}
            sources={sources?.sources ?? []}
            projectNames={projectNames}
            hidden={hidden}
            onOffer={setOffered}
          />
          <MemoriesSection ops={ops} layer={layer} options={options} busy={busy} run={run} />
          <SourcesSection {...shared} snapshot={sources} onSnapshot={setSources} skills={data.skills} projects={projects} />
        </>
      )}
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}
