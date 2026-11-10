// The dev fake host's knowledge layers: rules, skills and agents for Everyone and for each fake
// profile, linked skill folders and hub memories (Settings → Skills & rules, Settings → Agents in a
// plain browser). Dev builds only.
//
// The page URL picks the state:
//   (nothing)           a few rules, skills and agents per list; one linked folder whose skill
//                       clashes with one of Helm's own; shared memory connected
//   ?memories=off       shared memory is not connected: the Memories section says so
//   ?memories=none      the host has no memories op: the section is left out

import { ApiError } from '../api/client.ts';
import type { AgentRow, KnowledgeLayer, LayerSkill, RuleRow, SkillSource, SourceProjects } from '../settings/skills/api.ts';
import type { FakeOpHandler } from './fakeHost.ts';

function param(name: string): string | null {
  return typeof location === 'undefined' ? null : new URLSearchParams(location.search).get(name);
}

interface Rule {
  path: string;
  title: string;
  text: string;
  revision: number;
}

interface Layer {
  rules: Rule[];
  skills: { name: string; description: string }[];
  agents: { name: string; description: string }[];
}

interface Memory {
  id: string;
  title: string;
  layer: string;
}

const HOME = 'profile:prof-home';
const SAM = 'profile:prof-sam';

export function knowledgeFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const memoriesMode = param('memories');
  const layers = new Map<string, Layer>([
    [
      'everyone',
      {
        rules: [
          { path: 'rules/commits.md', title: 'Commit messages', text: 'Write commit messages in plain words, one change per commit.', revision: 1 },
          { path: 'rules/careful-deletes.md', title: 'Careful deletes', text: 'Look at what a delete removes before running it.', revision: 1 },
        ],
        skills: [
          { name: 'review-pr', description: 'Review a pull request against the repo conventions.' },
          { name: 'release-notes', description: 'Draft release notes from the commits since the last tag.' },
        ],
        agents: [{ name: 'reviewer', description: 'Reads a diff and reports what could break.' }],
      },
    ],
    [HOME, { rules: [{ path: 'rules/short-answers.md', title: 'Short answers', text: 'Keep answers short and lead with the result.', revision: 1 }], skills: [{ name: 'weekly-plan', description: 'Plan the week from the open tasks.' }], agents: [] }],
    [SAM, { rules: [], skills: [], agents: [{ name: 'tutor', description: 'Explains code step by step.' }] }],
  ]);
  const linkedSkills: Record<string, { name: string; description: string }[]> = {
    src1: [
      { name: 'review-pr', description: 'The team copy of the pull request review.' },
      { name: 'deploy-checklist', description: 'Steps before a deploy.' },
    ],
  };
  let sources: SkillSource[] = [{ id: 'src1', path: 'C:/team/shared-skills', layer: 'everyone', projects: 'all', enabled: true }];
  let sourcesRevision = 1;
  const memories: Memory[] = [
    { id: 'm1', title: 'The shop uses metric sizes', layer: 'everyone' },
    { id: 'm2', title: 'Prefers tabs in the notes repo', layer: HOME },
    { id: 'm3', title: 'Learning git rebase', layer: SAM },
  ];

  const need = (key: string): Layer => {
    const l = layers.get(key);
    if (!l) throw new ApiError('bad_args', 'The layer is `everyone` or `profile:<id>`.');
    return l;
  };
  const ruleRow = (r: Rule): RuleRow => ({ path: r.path, title: r.title, size: r.text.length + r.title.length + 4, revision: String(r.revision), tooLarge: false });
  const agentRow = (key: string, a: { name: string; description: string }): AgentRow => ({ ...a, path: `C:/Helm/knowledge/${key}/agents/${a.name}.md` });

  const view = (key: string): KnowledgeLayer => {
    const l = need(key);
    const skills: LayerSkill[] = l.skills.map((s) => ({ ...s, path: `C:/Helm/knowledge/${key}/skills/${s.name}`, source: 'folder', projects: 'all' as SourceProjects }));
    for (const src of sources.filter((s) => s.enabled && s.layer === key)) {
      for (const s of linkedSkills[src.id] ?? []) skills.push({ ...s, path: `${src.path}/${s.name}`, source: `linked:${src.id}`, projects: src.projects });
    }
    const clashes = skills.flatMap((later, i) => {
      const first = skills.slice(0, i).find((e) => e.name === later.name);
      return first ? [{ plugin: '', name: later.name, kept: first.source, dropped: later.source }] : [];
    });
    return {
      layer: key,
      folder: `C:/Helm/knowledge/${key}`,
      rules: l.rules.map(ruleRow),
      skills,
      agents: l.agents.map((a) => agentRow(key, a)),
      clashes,
    };
  };

  const slug = (title: string) => title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'rule';
  // The list a kind lives in, seen only through the field that names its items.
  const arrayOf = (l: Layer, kind: string): { path?: string; name?: string }[] =>
    (kind === 'rule' ? l.rules : kind === 'skill' ? l.skills : l.agents) as { path?: string; name?: string }[];
  const nameOf = (kind: string, item: { path?: string; name?: string }) => (kind === 'rule' ? item.path : item.name);

  const handler: FakeOpHandler = (op, a, ctx) => {
    const changed = (...keys: string[]) => {
      for (const layer of keys) ctx.emit({ kind: 'knowledge-changed', layer });
    };
    switch (op) {
      case 'knowledge_get':
        return view(String(a.layer));
      case 'knowledge_rule_read': {
        const r = need(String(a.layer)).rules.find((x) => x.path === a.path);
        if (!r) throw new ApiError('bad_args', 'No rule of that name is in the folder.');
        return { rule: ruleRow(r), text: r.text };
      }
      case 'knowledge_rule_write': {
        const key = String(a.layer);
        const l = need(key);
        const title = String(a.title ?? '').trim();
        const text = String(a.text ?? '');
        if (!title || !text.trim()) throw new ApiError('bad_args', 'A rule needs a title and some text.');
        const existing = typeof a.path === 'string' ? l.rules.find((x) => x.path === a.path) : undefined;
        if (existing) {
          if (a.revision !== undefined && String(a.revision) !== String(existing.revision)) throw new ApiError('conflict', 'That rule changed since it was read.');
          Object.assign(existing, { title, text, revision: existing.revision + 1 });
          changed(key);
          return { rule: ruleRow(existing) };
        }
        let path = `rules/${slug(title)}.md`;
        for (let n = 2; l.rules.some((x) => x.path === path); n++) path = `rules/${slug(title)}-${n}.md`;
        const made: Rule = { path, title, text, revision: 1 };
        l.rules.push(made);
        changed(key);
        return { rule: ruleRow(made) };
      }
      case 'knowledge_delete': {
        const key = String(a.layer);
        const l = need(key);
        const list = arrayOf(l, String(a.kind));
        const i = list.findIndex((x) => nameOf(String(a.kind), x) === a.name);
        if (i < 0) throw new ApiError('bad_args', 'No item of that name is in the folder.');
        list.splice(i, 1);
        changed(key);
        return view(key);
      }
      case 'knowledge_move': {
        const from = need(String(a.from));
        const to = need(String(a.to));
        const kind = String(a.kind);
        const src = arrayOf(from, kind);
        const i = src.findIndex((x) => nameOf(kind, x) === a.name);
        if (i < 0) throw new ApiError('bad_args', 'There is nothing of that name to move.');
        const dest = arrayOf(to, kind);
        if (dest.some((x) => nameOf(kind, x) === a.name)) throw new ApiError('conflict', 'The other layer already has one of that name.');
        dest.push(...src.splice(i, 1));
        changed(String(a.from), String(a.to));
        return { moved: String(a.name) };
      }
      case 'knowledge_reveal':
      case 'helm_skills_reveal':
      case 'helm_agents_reveal':
        return { revealed: true };
      case 'knowledge_skill_read': {
        const s = need(String(a.layer)).skills.find((x) => x.name === a.name);
        if (!s) throw new ApiError('bad_args', 'No skill of that name is in the folder.');
        return { text: `---\nname: ${s.name}\ndescription: ${s.description}\n---\n\nSteps for ${s.name} go here.\n` };
      }
      case 'knowledge_memories_list': {
        if (memoriesMode === 'none') throw new ApiError('unknown_op', op);
        if (memoriesMode === 'off') throw new ApiError('unavailable', 'Shared memory is not connected.');
        const key = String(a.layer);
        need(key);
        return {
          memories: memories
            .filter((m) => m.layer === key)
            .map((m) => ({ id: m.id, title: m.title, owner: m.layer === 'everyone' ? '' : m.layer === HOME ? 'Alex' : 'Sam', layer: m.layer })),
        };
      }
      case 'knowledge_memory_move': {
        const m = memories.find((x) => x.id === a.id);
        if (!m) throw new ApiError('bad_args', 'No memory of that id.');
        need(String(a.to));
        const from = m.layer;
        m.layer = String(a.to);
        changed(from, m.layer);
        return { moved: m.id };
      }
      case 'skill_sources_get':
        return { sources, revision: String(sourcesRevision) };
      case 'skill_sources_save': {
        if (a.revision !== undefined && String(a.revision) !== String(sourcesRevision)) throw new ApiError('conflict', 'The linked folders changed since they were read.');
        const s = a.source as Omit<SkillSource, 'id'> & { id?: string };
        if (!s.path || !String(s.path).trim()) throw new ApiError('bad_args', 'Pick a folder first.');
        need(s.layer);
        const id = s.id || `src${sourcesRevision + 1}`;
        const saved: SkillSource = { ...s, id };
        sources = sources.some((x) => x.id === id) ? sources.map((x) => (x.id === id ? saved : x)) : [...sources, saved];
        sourcesRevision++;
        changed(saved.layer);
        return { sources, revision: String(sourcesRevision) };
      }
      case 'skill_sources_remove': {
        if (a.revision !== undefined && String(a.revision) !== String(sourcesRevision)) throw new ApiError('conflict', 'The linked folders changed since they were read.');
        const gone = sources.find((x) => x.id === a.id);
        sources = sources.filter((x) => x.id !== a.id);
        sourcesRevision++;
        changed(gone?.layer ?? 'everyone');
        return { sources, revision: String(sourcesRevision) };
      }
      case 'helm_skills_list': {
        const key = String(a.layer ?? 'everyone');
        const skills = need(key).skills.map((s) => ({ ...s, path: `C:/Helm/knowledge/${key}/skills/${s.name}` }));
        return { skills, folder: `C:/Helm/knowledge/${key}/skills`, loadedIntoChats: skills.length > 0 };
      }
      case 'helm_skills_add': {
        const key = String(a.layer ?? 'everyone');
        const name = String(a.source).split(/[\\/]/).filter(Boolean).pop()?.toLowerCase() ?? 'new-skill';
        const l = need(key);
        if (!l.skills.some((s) => s.name === name)) l.skills.push({ name, description: 'Added from a folder.' });
        changed(key);
        return { skills: l.skills.map((s) => ({ ...s, path: `C:/Helm/knowledge/${key}/skills/${s.name}` })), added: name };
      }
      case 'helm_skills_remove': {
        const key = String(a.layer ?? 'everyone');
        const l = need(key);
        l.skills = l.skills.filter((s) => s.name !== a.name);
        changed(key);
        return { skills: l.skills.map((s) => ({ ...s, path: `C:/Helm/knowledge/${key}/skills/${s.name}` })) };
      }
      case 'helm_agents_list': {
        const key = String(a.layer ?? 'everyone');
        return { agents: need(key).agents.map((x) => agentRow(key, x)), folder: `C:/Helm/knowledge/${key}/agents` };
      }
      case 'helm_agents_add': {
        const key = String(a.layer ?? 'everyone');
        const name = String(a.source).split(/[\\/]/).filter(Boolean).pop()?.replace(/\.md$/, '').toLowerCase() ?? 'new-agent';
        const l = need(key);
        if (!l.agents.some((x) => x.name === name)) l.agents.push({ name, description: 'Added from a folder.' });
        changed(key);
        return { agents: l.agents.map((x) => agentRow(key, x)), added: [name], skipped: [] };
      }
      case 'helm_agents_remove': {
        const key = String(a.layer ?? 'everyone');
        const l = need(key);
        l.agents = l.agents.filter((x) => x.name !== a.name);
        changed(key);
        return { agents: l.agents.map((x) => agentRow(key, x)) };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['knowledge_', 'skill_sources_', 'helm_skills_', 'helm_agents_'], handler };
}
