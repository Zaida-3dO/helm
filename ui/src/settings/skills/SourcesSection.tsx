// Skill folders: folders elsewhere on this PC whose subfolders each hold a SKILL.md. They are read
// in place at every chat start, never copied into Helm and never synced to other devices. Like the
// MCP server list: a list, one inline editor, and a revision sent with every save so a stale list
// is refused instead of overwriting a newer one.

import { useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import type { KnowledgeOps, LayerSkill, SkillSource, SourceProjects, SourcesSnapshot } from './api.ts';
import { errorCode } from './common.ts';
import { layerLabel, projectsLabel, skillsFromSource, toggleProject, type LayerOption } from './layers.ts';
import type { RunAction } from './RulesSection.tsx';

const countLabel = (n: number) => `${n} ${n === 1 ? 'skill' : 'skills'}`;

export interface ProjectChoice {
  id: string;
  name: string;
}

function SourceEditor({ ops, options, layer, source, snapshot, projects, busy, run, onSaved, onClose }: {
  ops: KnowledgeOps;
  options: LayerOption[];
  /** The layer a new folder joins. */
  layer: string;
  /** The folder being edited; `null` for a new one. */
  source: SkillSource | null;
  snapshot: SourcesSnapshot;
  projects: ProjectChoice[];
  busy: boolean;
  run: RunAction;
  onSaved: (s: SourcesSnapshot) => void;
  onClose: () => void;
}) {
  const shell = useShell();
  const [path, setPath] = useState(source?.path ?? '');
  const [to, setTo] = useState(source?.layer ?? layer);
  const [all, setAll] = useState(source ? source.projects === 'all' : true);
  const [ids, setIds] = useState<string[]>(source && source.projects !== 'all' ? source.projects : []);
  const [enabled, setEnabled] = useState(source?.enabled ?? true);
  const [stale, setStale] = useState(false);

  const browse = () =>
    run(async () => {
      const picked = await shell.api<{ path: string | null }>('folder_pick', { title: 'Folder whose subfolders are skills' });
      if (picked.path) setPath(picked.path);
    });

  const save = () =>
    run(async () => {
      const reach: SourceProjects = all ? 'all' : ids;
      try {
        const next = await ops.saveSource({ ...(source ? { id: source.id } : {}), path: path.trim(), layer: to, projects: reach, enabled }, snapshot.revision);
        onSaved(next);
      } catch (e) {
        if (errorCode(e) === 'conflict') {
          setStale(true);
          onSaved(await ops.sources());
          return;
        }
        throw e;
      }
      onClose();
      return source ? 'Saved the skill folder.' : 'Linked the folder. Chats read its skills from their next start.';
    });

  return (
    <div className="kn-editor" data-testid="skill-source-editor">
      {stale && (
        <div className="ui-field-error" role="alert" data-testid="skill-source-stale">
          The list of skill folders changed since you opened this, so nothing was saved. Check the fields and save again.
        </div>
      )}
      <div className="kn-field">
        <TextInput
          aria-label="Folder path"
          placeholder="Folder whose subfolders each hold a SKILL.md"
          spellCheck={false}
          value={path}
          onChange={(e) => setPath(e.target.value)}
          data-testid="skill-source-path"
        />
        <Button variant="secondary" disabled={busy} data-testid="skill-source-browse" onClick={() => void browse()}>
          Browse…
        </Button>
      </div>
      <div className="kn-field" role="group" aria-label="Whose list the skills join">
        {options.map((o) => (
          <label key={o.key} className="kn-choice">
            <input type="radio" name="skill-source-layer" checked={to === o.key} onChange={() => setTo(o.key)} data-testid={`skill-source-layer-${o.testSuffix}`} />
            {o.label}
          </label>
        ))}
      </div>
      <div className="kn-field" role="group" aria-label="Projects the skills load in">
        <Checkbox label="All projects" checked={all} onChange={setAll} data-testid="skill-source-all" />
        {!all &&
          projects.map((p) => (
            <Checkbox key={p.id} label={p.name} checked={ids.includes(p.id)} onChange={() => setIds((cur) => toggleProject(cur, p.id))} data-testid={`skill-source-project-${p.id}`} />
          ))}
      </div>
      <Checkbox label="Load these skills in chats" checked={enabled} onChange={setEnabled} data-testid="skill-source-enabled" />
      <div className="kn-actions">
        <Button variant="primary" disabled={busy || !path.trim() || (!all && ids.length === 0)} data-testid="skill-source-save" onClick={() => void save()}>
          Save
        </Button>
        <Button variant="secondary" disabled={busy} data-testid="skill-source-cancel" onClick={onClose}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

export function SourcesSection({ ops, options, layer, snapshot, onSnapshot, skills, projects, busy, run, confirm }: {
  ops: KnowledgeOps;
  options: LayerOption[];
  layer: string;
  snapshot: SourcesSnapshot | null;
  onSnapshot: (s: SourcesSnapshot) => void;
  /** The shown layer's skills, to count what each folder gave it. */
  skills: LayerSkill[];
  projects: ProjectChoice[];
  busy: boolean;
  run: RunAction;
  confirm: (spec: ConfirmSpec) => void;
}) {
  // `null`: closed; `'new'`: a blank editor; otherwise the id of the folder being edited.
  const [open, setOpen] = useState<string | null>(null);
  const names = new Map(projects.map((p) => [p.id, p.name]));
  const mine = (snapshot?.sources ?? []).filter((s) => s.layer === layer);
  const editing = open && open !== 'new' ? (mine.find((s) => s.id === open) ?? null) : null;

  const toggleEnabled = (s: SkillSource, on: boolean) =>
    snapshot &&
    run(async () => {
      onSnapshot(await ops.saveSource({ ...s, enabled: on }, snapshot.revision));
    });

  const askRemove = (s: SkillSource) =>
    snapshot &&
    confirm({
      title: 'Stop using this folder?',
      body: `Its skills stop loading in chats. The folder itself is not touched.`,
      confirm: 'Remove',
      danger: true,
      onConfirm: () =>
        run(async () => {
          onSnapshot(await ops.removeSource(s.id, snapshot.revision));
          return 'Stopped using the folder.';
        }),
    });

  return (
    <section className="kn-section" data-testid="knowledge-sources">
      <div className="kn-head">
        <h3 className="hub-heading">Skill folders</h3>
        <div className="kn-actions">
          <Button variant="primary" disabled={busy || !snapshot} data-testid="skill-source-add" onClick={() => setOpen('new')}>
            Add folder
          </Button>
        </div>
      </div>
      <div className="setting-desc">
        Folders elsewhere on this PC whose subfolders each hold a skill. They are read where they are, so editing a skill there changes it here. They are not shared with your other devices.
      </div>
      {mine.length === 0 && open !== 'new' && (
        <div className="setting-desc" data-testid="skill-sources-empty">
          No folders linked to {layerLabel(options, layer)}.
        </div>
      )}
      {snapshot &&
        mine.map((s) =>
          open === s.id ? (
            <SourceEditor key={s.id} {...{ ops, options, layer, snapshot, projects, busy, run }} source={editing} onSaved={onSnapshot} onClose={() => setOpen(null)} />
          ) : (
            <div className="skills-row" key={s.id} data-testid={`skill-source-${s.id}`}>
              <Checkbox
                checked={s.enabled}
                aria-label={`Load skills from ${s.path}`}
                disabled={busy}
                data-testid={`skill-source-switch-${s.id}`}
                onChange={(on) => void toggleEnabled(s, on)}
              />
              <div className="skills-what">
                <div className="skills-name">{s.path}</div>
                <div className="skills-desc">
                  {projectsLabel(s.projects, names)} · {countLabel(skillsFromSource(skills, s.id))}
                </div>
              </div>
              <Button variant="text" disabled={busy} data-testid={`skill-source-edit-${s.id}`} onClick={() => setOpen(s.id)}>
                Edit
              </Button>
              <Button variant="text" danger disabled={busy} data-testid={`skill-source-remove-${s.id}`} onClick={() => askRemove(s)}>
                Remove
              </Button>
            </div>
          ),
        )}
      {snapshot && open === 'new' && (
        <SourceEditor {...{ ops, options, layer, snapshot, projects, busy, run }} source={null} onSaved={onSnapshot} onClose={() => setOpen(null)} />
      )}
    </section>
  );
}
