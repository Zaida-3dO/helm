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
import type { RuleWhen } from '../api/generated/skills/RuleWhen.ts';
import type { AgentRow, KnowledgeLayer, LayerSkill, RulePreviewRow, RulesBudget, RuleRow, SkillSource, SourceProjects } from '../settings/skills/api.ts';
import { dateLabel, localToday, ruleDates, ruleHasTrigger } from '../settings/skills/layers.ts';
import type { FakeOpHandler } from './fakeHost.ts';

function param(name: string): string | null {
  return typeof location === 'undefined' ? null : new URLSearchParams(location.search).get(name);
}

const emptyWhen = (): RuleWhen => ({
  show: null, otherwise: null, before: null,
  folders: [], hosts: [], pcs: [], tools: [], commands: [], kinds: [], people: [],
  from: null, until: null, review: null, problems: [],
});

/** Reads op input as whole trigger keys: missing lists become empty, blanks drop out. */
function normWhen(v: unknown): RuleWhen {
  const o = (v ?? {}) as Record<string, unknown>;
  const list = (k: string): string[] => (Array.isArray(o[k]) ? (o[k] as unknown[]).map((s) => String(s)).map((s) => s.trim()).filter(Boolean) : []);
  const one = (k: string): string | null => (typeof o[k] === 'string' && (o[k] as string).trim() ? String(o[k]).trim() : null);
  return {
    show: one('show'), otherwise: one('otherwise'), before: one('before'),
    folders: list('folders'), hosts: list('hosts'), pcs: list('pcs'), tools: list('tools'),
    commands: list('commands'), kinds: list('kinds'), people: list('people'),
    from: one('from'), until: one('until'), review: one('review'), problems: [],
  };
}

const normPath = (p: string): string => p.replace(/\\/g, '/').toLowerCase().replace(/\/+$/, '');
const globToRe = (pat: string): RegExp => new RegExp(`^${pat.split('*').map((s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('.*')}$`, 'i');

interface Rule {
  path: string;
  title: string;
  text: string;
  revision: number;
  always?: boolean;
  when?: RuleWhen;
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
          { path: 'rules/commits.md', title: 'Commit messages', text: 'Write commit messages in plain words, one change per commit.', revision: 1, always: true },
          { path: 'rules/careful-deletes.md', title: 'Careful deletes', text: 'Look at what a delete removes before running it.', revision: 1 },
          {
            path: 'rules/no-named-kills.md',
            title: 'No kills by name',
            text: 'Kill a process by its id, never by its name. Name kills take down every process of that image on the machine.',
            revision: 1,
            when: { ...emptyWhen(), tools: ['Bash'], commands: ['taskkill', 'pkill'], before: 'stop' },
          },
          {
            path: 'rules/tone-tablet.md',
            title: 'Plain words on shared screens',
            text: 'Anything read aloud or shown on a household screen reads as speech to a person.',
            revision: 1,
            when: { ...emptyWhen(), kinds: ['household'], otherwise: 'line' },
          },
          {
            path: 'rules/old-habit.md',
            title: 'Old habit',
            text: 'An old instruction kept to check on later.',
            revision: 1,
            when: { ...emptyWhen(), review: '2025-06-01' },
          },
        ],
        skills: [
          { name: 'review-pr', description: 'Review a pull request against the repo conventions.' },
          { name: 'release-notes', description: 'Draft release notes from the commits since the last tag.' },
        ],
        agents: [{ name: 'reviewer', description: 'Reads a diff and reports what could break.' }],
      },
    ],
    [HOME, { rules: [{ path: 'rules/short-answers.md', title: 'Short answers', text: 'Keep answers short and lead with the result.', revision: 1 }], skills: [{ name: 'weekly-plan', description: 'Plan the week from the open tasks.' }], agents: [] }],
    [SAM, {
      rules: [
        { path: 'rules/winter-note.md', title: 'Winter note', text: 'A seasonal reminder kept past its end so it can be deleted by hand.', revision: 1, when: { ...emptyWhen(), until: '2026-01-31' } },
      ],
      skills: [],
      agents: [{ name: 'tutor', description: 'Explains code step by step.' }],
    }],
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
  // The host builds the one-line trigger summary; the fake reads it back from the stored keys.
  const today = localToday();
  const summaryOf = (w: RuleWhen | undefined): string => {
    if (!ruleHasTrigger(w) || !w) return '';
    const parts: string[] = [];
    for (const key of ['folders', 'hosts', 'pcs', 'tools', 'commands', 'kinds', 'people'] as const) {
      if (w[key].length) parts.push(`${key === 'commands' ? '' : `${key} `}${w[key].join(', ')}`);
    }
    if (w.show) parts.push(`show ${w.show}`);
    if (w.otherwise) parts.push(`otherwise ${w.otherwise}`);
    if (w.before === 'stop') parts.push('stops the first matching call');
    for (const [key, iso] of [['from', w.from], ['until', w.until], ['review', w.review]] as const) {
      if (iso) parts.push(`${key} ${dateLabel(iso, today)}`);
    }
    return parts.join(' · ');
  };
  const ruleRow = (r: Rule): RuleRow => ({ path: r.path, title: r.title, size: r.text.length + r.title.length + 4, revision: String(r.revision), tooLarge: false, always: r.always === true, when: r.when ?? emptyWhen(), whenSummary: summaryOf(r.when) });
  const agentRow = (key: string, a: { name: string; description: string }): AgentRow => ({ ...a, path: `C:/Helm/knowledge/${key}/agents/${a.name}.md` });

  // Which rules a described chat would get at launch. Roughly the host's decision walk, small on
  // purpose: no folder means no kind was derived, so a plain chat reads as household.
  const previewRow = (r: Rule, layerKey: string, args: { folder?: string; kind?: string; person?: string; tool?: string; command?: string }): RulePreviewRow => {
    const w = r.when ?? emptyWhen();
    const folder = args.folder?.trim() ?? '';
    const kind = (args.kind || (folder ? 'coding' : 'household')).toLowerCase();
    let decision: RulePreviewRow['decision'] = 'full';
    let why = 'no When: every chat gets it';
    if (ruleHasTrigger(w)) {
      why = '';
      const d = ruleDates(w, today);
      if (d.expired) { decision = 'hidden'; why = `ended on ${dateLabel(w.until, today)}`; }
      else if (d.notYet) { decision = 'hidden'; why = `starts on ${dateLabel(w.from, today)}`; }
      else if (w.kinds.length && !w.kinds.some((k) => k.toLowerCase() === kind)) { decision = 'hidden'; why = `kinds says ${w.kinds.join(', ')} — this chat is ${kind}`; }
      else if (w.people.length && !w.people.some((p) => p.toLowerCase() === (args.person ?? '').toLowerCase())) { decision = 'hidden'; why = `people says ${w.people.join(', ')}`; }
      else {
        const moment = [w.folders, w.hosts, w.tools, w.commands].some((l) => l.length > 0);
        const folderFired = !!folder && w.folders.some((f) => normPath(folder) === normPath(f) || normPath(folder).startsWith(`${normPath(f)}/`));
        if (moment && !folderFired) {
          decision = w.otherwise === 'line' ? 'line' : 'hidden';
          why = `waits for ${(['folders', 'hosts', 'tools', 'commands'] as const).filter((k) => w[k].length > 0).join(' or ')}`;
        } else {
          why = moment ? 'its folder matches already' : 'its When fits this chat';
          decision = r.always === true || w.show === 'full' ? 'full' : w.show === 'line' ? 'line' : 'full';
        }
      }
    }
    const tool = args.tool?.trim() ?? '';
    const command = args.command?.trim() ?? '';
    const fires =
      (tool !== '' && w.tools.some((p) => globToRe(p).test(tool))) ||
      (command !== '' && w.commands.some((c) => command.toLowerCase().includes(c.toLowerCase())));
    return { path: r.path, layer: layerKey, title: r.title, decision, fires, why };
  };

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
    // As the host reports it: whole rules (short ones, and any marked `always`) against the 32 KiB cap.
    const budget: RulesBudget = {
      usedBytes: l.rules.filter((r) => r.always === true || r.text.length <= 800).reduce((n, r) => n + r.title.length + r.text.length + 8, 0),
      capBytes: 32 * 1024,
    };
    return {
      layer: key,
      folder: `C:/Helm/knowledge/${key}`,
      rules: l.rules.map(ruleRow),
      skills,
      agents: l.agents.map((a) => agentRow(key, a)),
      clashes,
      budget,
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
          if (a.always !== undefined) existing.always = Boolean(a.always);
          if (a.when !== undefined) existing.when = normWhen(a.when);
          changed(key);
          return { rule: ruleRow(existing) };
        }
        let path = `rules/${slug(title)}.md`;
        for (let n = 2; l.rules.some((x) => x.path === path); n++) path = `rules/${slug(title)}-${n}.md`;
        const made: Rule = { path, title, text, revision: 1, always: Boolean(a.always), ...(a.when !== undefined ? { when: normWhen(a.when) } : {}) };
        l.rules.push(made);
        changed(key);
        return { rule: ruleRow(made) };
      }
      case 'knowledge_rules_preview': {
        const key = String(a.layer);
        need(key);
        const lists: [string, Rule[]][] = key === 'everyone' ? [['everyone', need('everyone').rules]] : [['everyone', need('everyone').rules], [key, need(key).rules]];
        const described = a as { folder?: string; kind?: string; person?: string; tool?: string; command?: string };
        return { rules: lists.flatMap(([layerKey, list]) => list.map((r) => previewRow(r, layerKey, described))) };
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
      case 'knowledge_move':
      case 'knowledge_copy': {
        const copy = op === 'knowledge_copy';
        const from = need(String(a.from));
        const to = need(String(a.to));
        const kind = String(a.kind);
        const src = arrayOf(from, kind);
        const i = src.findIndex((x) => nameOf(kind, x) === a.name);
        if (i < 0) throw new ApiError('bad_args', 'There is nothing of that name to move.');
        const dest = arrayOf(to, kind);
        if (dest.some((x) => nameOf(kind, x) === a.name)) throw new ApiError('conflict', 'The other layer already has one of that name.');
        dest.push(...(copy ? [{ ...src[i] }] : src.splice(i, 1)));
        changed(String(a.from), String(a.to));
        return copy ? { copied: String(a.name) } : { moved: String(a.name) };
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
