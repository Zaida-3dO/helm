// The skills ops (host/crates/skills) that Settings → Skills uses. They only ever touch Helm's
// own skills folder; the picked folder is copied in, never moved or changed.

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
    list: () => api<SkillsView>('helm_skills_list', {}),
    add: (source: string) => api<{ skills: SkillRow[]; added: string }>('helm_skills_add', { source }),
    remove: (name: string) => api<{ skills: SkillRow[] }>('helm_skills_remove', { name }),
    reveal: (name?: string) => api<{ revealed: boolean }>('helm_skills_reveal', name ? { name } : {}),
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
    list: () => api<AgentsView>('helm_agents_list', {}),
    /** A single agent file, or a folder of them. */
    add: (source: string) => api<{ agents: AgentRow[]; added: string[]; skipped: string[] }>('helm_agents_add', { source }),
    remove: (name: string) => api<{ agents: AgentRow[] }>('helm_agents_remove', { name }),
    reveal: () => api<{ revealed: boolean }>('helm_agents_reveal', {}),
  };
}
