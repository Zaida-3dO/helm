// A local chat pins every model slot to its local model server, so a sub-agent that asks for a
// Claude model would quietly run on the local one instead. This refuses that call and names the way
// out; it is registered only for local chats.

import fs from "node:fs";
import path from "node:path";

export const SUBAGENT_MODEL_HOOK_ID = "helm-subagent-model";
export const SUBAGENT_MODEL_MATCHER = "Agent|Task";

/** True when a model name can only mean a Claude model: `opus`, `sonnet` or `haiku` by themselves,
 * or a full `claude-…` id. `inherit` and anything a local server would answer are not. */
export function isClaudeModelRef(model: string): boolean {
  const m = model.trim().toLowerCase();
  return /^(opus|sonnet|haiku)$/.test(m) || m.startsWith("claude-");
}

/** The refusal, exported so a test pins its wording. */
export function subagentModelDenyReason(name: string, model: string): string {
  return `The sub-agent '${name}' asks for ${model}, but this chat runs on a local model server, so it would run on the local model instead. To run it on ${model}, start it as its own chat with app_create_chat (model "${model}"): give it the same brief, and ask it to report back to this chat with app_send_message. Or call a sub-agent with no model of its own.`;
}

export interface SubagentModelOptions {
  /** The launch's own agent definitions file (JSON, one entry per agent name). */
  agentsFile?: string;
  /** The chat's folder: its `.claude/agents/*.md` definitions. */
  cwd?: string;
  /** The user's home: `~/.claude/agents/*.md`. */
  home?: string;
}

/** The model each named agent's definition asks for. Read once per launch and cached; unreadable
 * files are skipped quietly. Sources in order of precedence: the launch's file, the project's
 * markdown files, the home folder's. */
export class SubagentModelGuard {
  private models: Map<string, string> | null = null;
  private readonly o: SubagentModelOptions;

  constructor(o: SubagentModelOptions) {
    this.o = o;
  }

  /** The deny reason for one Agent/Task call, or null to let it through. */
  decide(input: JsonInput): string | null {
    const type = typeof input.subagent_type === "string" ? input.subagent_type.trim() : "";
    const asked = typeof input.model === "string" ? input.model.trim() : "";
    const model = asked !== "" ? asked : type !== "" ? this.definitionModels().get(type) ?? "" : "";
    if (model === "" || model.toLowerCase() === "inherit") return null;
    if (!isClaudeModelRef(model)) return null;
    return subagentModelDenyReason(type !== "" ? type : "default", model);
  }

  private definitionModels(): Map<string, string> {
    if (this.models) return this.models;
    const out = new Map<string, string>();
    // First definition of a name wins: the launch's file, then the project, then the home folder.
    const add = (name: string, model: string) => {
      if (name !== "" && model !== "" && !out.has(name)) out.set(name, model);
    };
    if (this.o.agentsFile) {
      try {
        const parsed = JSON.parse(fs.readFileSync(this.o.agentsFile, "utf8"));
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          for (const [name, def] of Object.entries(parsed as Record<string, unknown>)) {
            const model = def && typeof def === "object" ? (def as Record<string, unknown>).model : undefined;
            add(name, typeof model === "string" ? model.trim() : "");
          }
        }
      } catch {
        // An unreadable or odd file changes nothing.
      }
    }
    for (const dir of [this.o.cwd && path.join(this.o.cwd, ".claude", "agents"), this.o.home && path.join(this.o.home, ".claude", "agents")]) {
      if (!dir) continue;
      let files: string[];
      try {
        files = fs.readdirSync(dir).filter((f) => f.endsWith(".md"));
      } catch {
        continue;
      }
      for (const file of files) {
        try {
          const fm = frontmatter(fs.readFileSync(path.join(dir, file), "utf8").slice(0, 4000));
          add(fm.name !== "" ? fm.name : file.slice(0, -3), fm.model);
        } catch {
          // As above: quiet.
        }
      }
    }
    this.models = out;
    return out;
  }
}

type JsonInput = Record<string, unknown>;

/** The two lines that matter in a markdown agent's frontmatter — `name:` and `model:` — read
 * without a YAML library. No frontmatter: both empty. */
function frontmatter(text: string): { name: string; model: string } {
  const out = { name: "", model: "" };
  const lines = text.split(/\r?\n/);
  if ((lines[0] ?? "").trim() !== "---") return out;
  const clean = (v: string) => v.trim().replace(/^["']|["']$/g, "");
  for (let i = 1; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === "---") break;
    const name = /^name:\s*(.+)$/.exec(line);
    if (name && out.name === "") out.name = clean(name[1]);
    const model = /^model:\s*(.+)$/.exec(line);
    if (model && out.model === "") out.model = clean(model[1]);
  }
  return out;
}
