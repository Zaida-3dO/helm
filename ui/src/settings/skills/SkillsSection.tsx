// Skills: folders holding a SKILL.md that Claude Code can load as commands. A layer's skills are the
// ones copied into its own folder plus the ones its linked folders give it. Turning one off only
// hides it from the `/` menu (the hiddenSkills list Settings → Chat edits too); removing deletes
// Helm's copy and never the folder it was added from.

import { useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, type ConfirmSpec } from '../../ui/index.ts';
import { skillsOps, type Clash, type KnowledgeOps, type LayerSkill, type SkillSource } from './api.ts';
import { errorText } from './common.ts';
import { MoveTo } from './LayerPicker.tsx';
import { projectsLabel, sourceLabel, type LayerOption } from './layers.ts';
import type { RunAction } from './RulesSection.tsx';

const isHidden = (hidden: string[], name: string) => hidden.some((h) => h.toLowerCase() === name.toLowerCase());

export function SkillsSection({ ops, layer, options, busy, run, confirm, skills, clashes, sources, projectNames, hidden, onOffer }: {
  ops: KnowledgeOps;
  layer: string;
  options: LayerOption[];
  busy: boolean;
  run: RunAction;
  confirm: (spec: ConfirmSpec) => void;
  skills: LayerSkill[];
  clashes: Clash[];
  sources: SkillSource[];
  projectNames: ReadonlyMap<string, string>;
  hidden: string[];
  /** Show or hide a skill in the chat `/` menu. */
  onOffer: (name: string, on: boolean) => void;
}) {
  const shell = useShell();
  const skillOps = useMemo(() => skillsOps(shell.api), [shell.api]);
  // The skill whose SKILL.md is open under its row, and what was read (or why not).
  const [viewing, setViewing] = useState<{ name: string; text: string | null; error: string | null } | null>(null);

  const add = () =>
    run(async () => {
      const picked = await shell.api<{ path: string | null }>('folder_pick', { title: 'Folder with the skill to add' });
      if (!picked.path) return;
      const r = await skillOps.add(picked.path, layer);
      return `Added ${r.added}. Chats pick it up from their next start.`;
    });

  const askRemove = (name: string) =>
    confirm({
      title: 'Remove the skill?',
      body: `Helm deletes its copy of ${name}. The folder you added it from is left alone.`,
      confirm: 'Remove',
      danger: true,
      onConfirm: () =>
        run(async () => {
          await ops.remove(layer, 'skill', name);
          return `Removed ${name}.`;
        }),
    });

  const viewSource = (s: LayerSkill) => {
    if (viewing?.name === s.name) {
      setViewing(null);
      return;
    }
    setViewing({ name: s.name, text: null, error: null });
    ops
      .skillSource(layer, s)
      .then((text) => setViewing((v) => (v?.name === s.name ? { name: s.name, text, error: null } : v)))
      .catch((e: unknown) =>
        setViewing((v) => (v?.name === s.name ? { name: s.name, text: null, error: `${errorText(e)} Use Open to read it in Explorer.` } : v)),
      );
  };

  return (
    <section className="kn-section" data-testid="knowledge-skills">
      <div className="kn-head">
        <h3 className="hub-heading">Skills</h3>
        <div className="kn-actions">
          <Button variant="primary" disabled={busy} data-testid="knowledge-skill-add" onClick={() => void add()}>
            Add folder…
          </Button>
          <Button variant="secondary" disabled={busy} data-testid="knowledge-skills-open" onClick={() => void run(() => ops.reveal(layer, 'skill').then(() => undefined))}>
            Open folder
          </Button>
        </div>
      </div>
      <div className="setting-desc">
        A skill is a folder holding a SKILL.md. Adding one copies it into this list; the folder you picked is never changed. Chats pick it up from their next start.
      </div>
      {clashes.length > 0 && (
        <div className="kn-clash" role="alert" data-testid="knowledge-clash">
          {clashes.map((c) => (
            <div key={`${c.name}-${c.dropped}`}>
              Two skills are called {c.name}. The one from {sourceLabel(c.kept, sources)} loads and the one from {sourceLabel(c.dropped, sources)} is left out.
            </div>
          ))}
        </div>
      )}
      {skills.length === 0 && (
        <div className="setting-desc" data-testid="knowledge-skills-empty">
          No skills in this list yet.
        </div>
      )}
      {skills.map((s) => {
        const own = s.source === 'folder';
        return (
          <div key={`${s.source}/${s.name}`}>
            <div className="skills-row" data-testid={`skill-row-${s.name}`}>
              <Checkbox
                checked={!isHidden(hidden, s.name)}
                aria-label={`Offer ${s.name} in chats`}
                data-testid={`skill-offer-${s.name}`}
                onChange={(on) => onOffer(s.name, on)}
              />
              <div className="skills-what">
                <div className="skills-name">{s.name}</div>
                {s.description !== '' && <div className="skills-desc">{s.description}</div>}
                <div className="skills-desc">
                  {sourceLabel(s.source, sources)}
                  {!own && ` · ${projectsLabel(s.projects, projectNames)}`}
                </div>
              </div>
              <Button variant="text" disabled={busy} data-testid={`skill-source-view-${s.name}`} onClick={() => viewSource(s)}>
                {viewing?.name === s.name ? 'Hide source' : 'View source'}
              </Button>
              {own && (
                <>
                  <Button variant="text" disabled={busy} data-testid={`skill-open-${s.name}`} onClick={() => void run(() => skillOps.reveal(s.name, layer).then(() => undefined))}>
                    Open
                  </Button>
                  <MoveTo
                    options={options}
                    from={layer}
                    what={s.name}
                    testId={`skill-move-${s.name}`}
                    disabled={busy}
                    onMove={(to) =>
                      void run(async () => {
                        await ops.move('skill', s.name, layer, to);
                        return `Moved ${s.name}.`;
                      })
                    }
                  />
                  <Button variant="text" danger disabled={busy} data-testid={`skill-remove-${s.name}`} onClick={() => askRemove(s.name)}>
                    Remove
                  </Button>
                </>
              )}
            </div>
            {viewing?.name === s.name && (
              <pre className="kn-source" data-testid="knowledge-skill-source">
                {viewing.error ?? viewing.text ?? 'Reading…'}
              </pre>
            )}
          </div>
        );
      })}
    </section>
  );
}
