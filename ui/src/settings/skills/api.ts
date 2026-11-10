// The skills and knowledge ops (host/crates/skills) that Settings → Skills & rules and Settings →
// Agents use. They only ever touch Helm's own knowledge folders and the linked folders the person
// added; a picked folder is copied in, never moved or changed.
//
// A layer is `everyone` or `profile:<profileId>`. The older skills and agents ops take it as an
// optional `layer` (default: everyone).

import type { Api } from '../../api/client.ts';

/** One skill as listed: its folder name, the description from its SKILL.md, and its path. */
export interface SkillRow {
  name: string;
  description: string;
  path: string;
}

export interface SkillsView {
  skills: SkillRow[];
  /** The skills folder itself, for the "Open folder" button. */
  folder: string;
  /** Whether there is at least one skill, so chats pick the folder up. */
  loadedIntoChats: boolean;
}

export function skillsOps(api: Api) {
  return {
    list: (layer?: string) => api<SkillsView>('helm_skills_list', layer ? { layer } : {}),
    add: (source: string, layer?: string) =>
      api<{ skills: SkillRow[]; added: string }>('helm_skills_add', layer ? { source, layer } : { source }),
    remove: (name: string, layer?: string) =>
      api<{ skills: SkillRow[] }>('helm_skills_remove', layer ? { name, layer } : { name }),
    reveal: (name?: string, layer?: string) =>
      api<{ revealed: boolean }>('helm_skills_reveal', { ...(name ? { name } : {}), ...(layer ? { layer } : {}) }),
  };
}

/** One agent as listed: its file name without `.md`, the description from its front matter, and its path. */
export interface AgentRow {
  name: string;
  description: string;
  path: string;
}

export interface AgentsView {
  agents: AgentRow[];
  /** The agents folder itself, for the "Open folder" button. */
  folder: string;
}

export function agentsOps(api: Api) {
  return {
    list: (layer?: string) => api<AgentsView>('helm_agents_list', layer ? { layer } : {}),
    /** A single agent file, or a folder of them. */
    add: (source: string, layer?: string) =>
      api<{ agents: AgentRow[]; added: string[]; skipped: string[] }>('helm_agents_add', layer ? { source, layer } : { source }),
    remove: (name: string, layer?: string) =>
      api<{ agents: AgentRow[] }>('helm_agents_remove', layer ? { name, layer } : { name }),
    reveal: (layer?: string) => api<{ revealed: boolean }>('helm_agents_reveal', layer ? { layer } : {}),
  };
}

/** One rule: a Markdown file under the layer's `rules/` folder. */
export interface RuleRow {
  /** `rules/<file>.md`: what the ops take back. */
  path: string;
  title: string;
  size: number;
  /** A hash of the file's text, sent back on save so an edit never overwrites a newer one. */
  revision: string;
  /** Over the size limit: listed so it can be fixed, but never loaded into a chat. */
  tooLarge: boolean;
}

/** `all`, or the ids of the projects a linked folder's skills load in. */
export type SourceProjects = 'all' | string[];

/** One skill in a layer: from the layer's own folder (`folder`) or a linked folder (`linked:<id>`). */
export interface LayerSkill {
  name: string;
  description: string;
  path: string;
  source: string;
  projects: SourceProjects;
}

/** A skill name two sources both gave the same set of projects: `kept` loads, `dropped` does not. */
export interface Clash {
  plugin: string;
  name: string;
  kept: string;
  dropped: string;
}

/** One layer as the knowledge screen draws it. */
export interface KnowledgeLayer {
  layer: string;
  folder: string;
  rules: RuleRow[];
  skills: LayerSkill[];
  agents: AgentRow[];
  clashes: Clash[];
}

export type KnowledgeKind = 'rule' | 'skill' | 'agent';

export interface SkillSource {
  /** Made by the host when a source is first saved without one. */
  id: string;
  path: string;
  layer: string;
  projects: SourceProjects;
  enabled: boolean;
}

export interface SourcesSnapshot {
  sources: SkillSource[];
  revision: string;
}

/** One shared memory the hub holds, as the Memories list shows it. */
export interface MemoryRow {
  id: string;
  title: string;
  /** The person it belongs to; empty for a memory everyone sees. */
  owner: string;
  /** `everyone` or `profile:<id>` when the host could map the owner to a profile here. */
  layer?: string;
}

export function knowledgeOps(api: Api) {
  return {
    get: (layer: string) => api<KnowledgeLayer>('knowledge_get', { layer }),
    ruleRead: (layer: string, path: string) =>
      api<{ rule: RuleRow; text: string }>('knowledge_rule_read', { layer, path }),
    /** A new rule has no `path`; a changed one sends the `revision` it was read at (a stale one is a conflict). */
    ruleWrite: (a: { layer: string; path?: string; title: string; text: string; revision?: string }) =>
      api<{ rule: RuleRow }>('knowledge_rule_write', a),
    remove: (layer: string, kind: KnowledgeKind, name: string) =>
      api<KnowledgeLayer>('knowledge_delete', { layer, kind, name }),
    move: (kind: KnowledgeKind, name: string, from: string, to: string) =>
      api<{ moved: string }>('knowledge_move', { kind, name, from, to }),
    reveal: (layer: string, kind?: KnowledgeKind) =>
      api<{ revealed: boolean }>('knowledge_reveal', kind ? { layer, kind } : { layer }),
    /**
     * A skill's SKILL.md text. A skill in Helm's own folder is read by the host (`knowledge_skill_read`:
     * the file viewer refuses Helm's data folder); one in a linked folder is an ordinary file.
     */
    skillSource: async (layer: string, skill: LayerSkill): Promise<string> => {
      if (skill.source === 'folder') return (await api<{ text: string }>('knowledge_skill_read', { layer, name: skill.name })).text;
      const sep = skill.path.includes('/') ? '/' : String.fromCharCode(92);
      const view = await api<{ text?: string }>('file_read', { path: `${skill.path.replace(/[\\/]+$/, '')}${sep}SKILL.md` });
      return view.text ?? '';
    },
    sources: () => api<SourcesSnapshot>('skill_sources_get', {}),
    saveSource: (source: Omit<SkillSource, 'id'> & { id?: string }, revision: string) =>
      api<SourcesSnapshot>('skill_sources_save', { source, revision }),
    removeSource: (id: string, revision: string) => api<SourcesSnapshot>('skill_sources_remove', { id, revision }),
    /** Shared memories for a layer, from the hub. Answers `unavailable` while no hub is connected. */
    memories: (layer: string) => api<{ memories: MemoryRow[] }>('knowledge_memories_list', { layer }).then((r) => r.memories ?? []),
    moveMemory: (id: string, to: string) => api<unknown>('knowledge_memory_move', { id, to }),
  };
}

export type KnowledgeOps = ReturnType<typeof knowledgeOps>;
