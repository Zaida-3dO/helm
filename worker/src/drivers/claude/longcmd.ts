// The long-command rule (the `longCommands` launch setting). A classifier that decides whether a
// root Bash call is a build or test run, a server or watcher, or a long sleep, and a per-turn
// policy that denies such a call once (`redirect`) or adds a reminder (`warn`).
//
// The parse is deliberately forgiving: anything it cannot follow is "not classified", so a parse
// failure never blocks a tool.

export type LongCommandLevel = "off" | "warn" | "redirect";

export function isLongCommandLevel(v: unknown): v is LongCommandLevel {
  return v === "off" || v === "warn" || v === "redirect";
}

export interface Classification {
  /** Short reason, shown to the agent and in the `long_command` event. */
  reason: string;
}

const MAX_DEPTH = 16;
const TOOL_TIMEOUT_LIMIT_MS = 2 * 60 * 1000;
const LONG_SLEEP_S = 10;

// ---- tokenising -------------------------------------------------------------------------

interface Word {
  text: string;
  /** Command substitutions found inside the word ($(...) or backticks), parsed separately. */
  subs: string[];
}

type Tok = { kind: "word"; word: Word } | { kind: "op"; op: string };

class ParseError extends Error {}

/** Splits a command line into words and operators, following quotes, escapes and substitutions. */
function tokenize(src: string): Tok[] {
  const toks: Tok[] = [];
  let i = 0;
  let cur: Word | undefined;
  const pendingHeredocs: { delim: string; strip: boolean }[] = [];
  const push = () => {
    if (cur) toks.push({ kind: "word", word: cur });
    cur = undefined;
  };
  const word = () => (cur ??= { text: "", subs: [] });

  const readSubst = (open: string, close: string): string => {
    // i points just after the opening; returns the inner text with nesting handled.
    let depth = 1;
    const start = i;
    while (i < src.length) {
      const c = src[i];
      if (c === "\\") {
        i += 2;
        continue;
      }
      if (c === "'" && open === "(") {
        const end = src.indexOf("'", i + 1);
        if (end === -1) throw new ParseError("unterminated quote");
        i = end + 1;
        continue;
      }
      if (open === "(" && c === "(") depth++;
      else if (c === close) {
        depth--;
        if (depth === 0) {
          const inner = src.slice(start, i);
          i++;
          return inner;
        }
      }
      i++;
    }
    throw new ParseError("unterminated substitution");
  };

  while (i < src.length) {
    const c = src[i];
    if (c === "\n") {
      push();
      toks.push({ kind: "op", op: ";" });
      i++;
      // Heredoc bodies follow the line that opened them: skip to each delimiter line.
      for (const h of pendingHeredocs.splice(0)) {
        for (;;) {
          const nl = src.indexOf("\n", i);
          const line = src.slice(i, nl === -1 ? src.length : nl);
          i = nl === -1 ? src.length : nl + 1;
          if ((h.strip ? line.replace(/^\t+/, "") : line).trim() === h.delim) break;
          if (nl === -1) break;
        }
      }
      continue;
    }
    if (c === " " || c === "\t" || c === "\r") {
      push();
      i++;
      continue;
    }
    if (c === "#" && !cur) {
      while (i < src.length && src[i] !== "\n") i++;
      continue;
    }
    if (c === "\\") {
      if (i + 1 < src.length && src[i + 1] !== "\n") word().text += src[i + 1];
      i += 2;
      continue;
    }
    if (c === "'") {
      const end = src.indexOf("'", i + 1);
      if (end === -1) throw new ParseError("unterminated quote");
      word().text += src.slice(i + 1, end);
      i = end + 1;
      continue;
    }
    if (c === '"') {
      const w = word();
      i++;
      for (;;) {
        if (i >= src.length) throw new ParseError("unterminated quote");
        const d = src[i];
        if (d === '"') {
          i++;
          break;
        }
        if (d === "\\" && i + 1 < src.length) {
          // Inside double quotes a backslash only escapes $ ` " \ and a newline.
          const n = src[i + 1];
          w.text += '$`"\\\n'.includes(n) ? n : d + n;
          i += 2;
          continue;
        }
        if (d === "$" && src[i + 1] === "(") {
          i += 2;
          w.subs.push(readSubst("(", ")"));
          continue;
        }
        if (d === "`") {
          i++;
          w.subs.push(readSubst("`", "`"));
          continue;
        }
        w.text += d;
        i++;
      }
      continue;
    }
    if (c === "$" && src[i + 1] === "(") {
      i += 2;
      word().subs.push(readSubst("(", ")"));
      continue;
    }
    if (c === "`") {
      i++;
      word().subs.push(readSubst("`", "`"));
      continue;
    }
    if (c === "<" && src[i + 1] === "<" && src[i + 2] !== "<") {
      push();
      i += 2;
      const strip = src[i] === "-";
      if (strip) i++;
      while (src[i] === " ") i++;
      const m = /^(['"]?)([A-Za-z0-9_\-.]+)\1/.exec(src.slice(i));
      if (m) {
        pendingHeredocs.push({ delim: m[2], strip });
        i += m[0].length;
      }
      continue;
    }
    const two = src.slice(i, i + 2);
    if (two === "&&" || two === "||" || two === ";;") {
      push();
      toks.push({ kind: "op", op: two });
      i += 2;
      continue;
    }
    if (c === "&" && (src[i + 1] === ">" || src[i - 1] === ">")) {
      // &> and >& are redirections, not backgrounding.
      word().text += c;
      i++;
      continue;
    }
    if (c === ";" || c === "|" || c === "&" || c === "(" || c === ")" || c === "{" || c === "}") {
      if ((c === "{" || c === "}") && cur) {
        word().text += c;
        i++;
        continue;
      }
      push();
      toks.push({ kind: "op", op: c });
      i++;
      continue;
    }
    word().text += c;
    i++;
  }
  push();
  return toks;
}

interface SimpleCommand {
  words: string[];
  background: boolean;
}

function splitCommands(toks: Tok[], subs: string[]): SimpleCommand[] {
  const out: SimpleCommand[] = [];
  let words: string[] = [];
  const end = (background: boolean) => {
    if (words.length) out.push({ words, background });
    words = [];
  };
  for (const t of toks) {
    if (t.kind === "word") {
      words.push(t.word.text);
      subs.push(...t.word.subs);
    } else if (t.op === "&") end(true);
    else end(false);
  }
  end(false);
  return out;
}

// ---- classifying ------------------------------------------------------------------------

const TEXT_TOOLS = new Set(["grep", "rg", "egrep", "fgrep", "echo", "printf", "git", "sed", "awk", "cat", "less", "head", "tail", "find", "ls", "select-string", "write-host", "write-output"]);
const WRAPPERS = new Set(["sudo", "env", "time", "command", "exec", "nice", "call", "&", "."]);
const SHELLS_WITH_C = new Set(["bash", "sh", "zsh", "dash", "cmd", "powershell", "pwsh"]);
const SCRIPT_RUNNERS = new Set(["npm", "pnpm", "yarn", "bun"]);
const BUILD_SCRIPT = /^(build|test|tests|typecheck|type-check|check|lint)([:\-_.].*)?$/i;
const SERVE_SCRIPT = /^(dev|start|serve|watch|preview)([:\-_.].*)?$/i;
const NPM_BUILTIN_TEST = new Set(["test", "t", "tst"]);
const NPM_BUILTIN_START = new Set(["start"]);
const BUILD_TOOLS = new Set(["tsc", "jest", "vitest", "mocha", "ava", "pytest", "tox", "nextest", "make", "gradle", "gradlew", "mvn", "msbuild", "ctest", "webpack", "rollup", "esbuild", "playwright", "cypress"]);
const SERVER_TOOLS = new Set(["nodemon", "http-server", "live-server", "serve", "uvicorn", "flask", "gunicorn"]);
const CARGO_LONG = new Set(["build", "b", "check", "c", "test", "t", "run", "r", "nextest", "clippy", "bench"]);

/** The command name without path, extension or case: `C:\x\npm.cmd` → `npm`. */
function baseName(w: string): string {
  const last = w.split(/[\\/]/).pop() ?? w;
  return last.replace(/\.(exe|cmd|bat|ps1|sh)$/i, "").toLowerCase();
}

function isVersionOrHelp(args: string[]): boolean {
  return args.some((a) => /^(--version|-V|--help|-h|-\?|\/\?)$/.test(a));
}

function parseSleepSeconds(arg: string | undefined): number | undefined {
  if (!arg) return undefined;
  const m = /^(\d+(?:\.\d+)?)([smhd]?)$/i.exec(arg);
  if (!m) return undefined;
  const n = Number(m[1]);
  const mult = { "": 1, s: 1, m: 60, h: 3600, d: 86400 }[m[2].toLowerCase() as "" | "s" | "m" | "h" | "d"];
  return n * mult;
}

function classifyWords(words: string[], depth: number): Classification | undefined {
  let i = 0;
  // Skip environment assignments and wrappers to reach the command-position word.
  while (i < words.length) {
    const w = words[i];
    if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(w)) {
      i++;
      continue;
    }
    if (WRAPPERS.has(baseName(w)) || w === "&") {
      i++;
      // `env -i` style flags
      while (i < words.length && words[i].startsWith("-") && baseName(words[i - 1]) === "env") i++;
      continue;
    }
    break;
  }
  if (i >= words.length) return undefined;
  const cmd = baseName(words[i]);
  const args = words.slice(i + 1);
  if (cmd === "nohup" || cmd === "start-job" || cmd === "start-process") return undefined; // already detached
  if (isVersionOrHelp(args)) return undefined;

  // A shell given a command string: classify what it runs.
  if (SHELLS_WITH_C.has(cmd)) {
    const k = args.findIndex((a) => /^(-c|\/c|\/k|-command|-c?ommand|-encodedcommand)$/i.test(a));
    if (k !== -1 && args[k + 1] !== undefined && !/^-encodedcommand$/i.test(args[k])) return classifyLine(args.slice(k + 1).join(" "), depth + 1);
    return undefined;
  }

  // `timeout 30 cmd` (coreutils) runs cmd; Windows `timeout /t 30` is a sleep.
  if (cmd === "timeout") {
    const t = args.findIndex((a) => a.toLowerCase() === "/t");
    if (t !== -1) {
      const secs = Number(args[t + 1]);
      return secs >= LONG_SLEEP_S ? { reason: `sleep of ${secs} s` } : undefined;
    }
    const rest = args.filter((a) => !a.startsWith("-"));
    return parseSleepSeconds(rest[0]) !== undefined && rest.length > 1 ? classifyWords(rest.slice(1), depth) : undefined;
  }

  // sleep N (and PowerShell's Start-Sleep).
  if (cmd === "sleep") {
    const secs = parseSleepSeconds(args.find((a) => !a.startsWith("-")));
    if (secs !== undefined && secs >= LONG_SLEEP_S) return { reason: `sleep of ${secs} s` };
    return undefined;
  }
  if (cmd === "start-sleep") {
    let secs: number | undefined;
    for (let k = 0; k < args.length; k++) {
      const a = args[k].toLowerCase();
      if (a === "-seconds" || a === "-s") secs = Number(args[k + 1]);
      else if (a === "-milliseconds" || a === "-m") secs = Number(args[k + 1]) / 1000;
      else if (!a.startsWith("-") && secs === undefined && k === 0) secs = Number(a);
    }
    if (secs !== undefined && secs >= LONG_SLEEP_S) return { reason: `sleep of ${secs} s` };
    return undefined;
  }

  // A --watch flag, except on text tools.
  if (!TEXT_TOOLS.has(cmd) && args.some((a) => a === "--watch" || a.startsWith("--watch="))) return { reason: "a watcher (--watch)" };
  if (TEXT_TOOLS.has(cmd)) return undefined;

  // Package scripts.
  if (SCRIPT_RUNNERS.has(cmd) || cmd === "npx" || cmd === "pnpx" || cmd === "bunx") {
    let rest = args.filter((a) => !a.startsWith("-"));
    if (cmd === "npx" || cmd === "pnpx" || cmd === "bunx" || rest[0] === "exec" || rest[0] === "dlx" || rest[0] === "x") {
      if (rest[0] === "exec" || rest[0] === "dlx" || rest[0] === "x") rest = rest.slice(1);
      return rest.length ? classifyWords(rest, depth) : undefined;
    }
    const sub = rest[0]?.toLowerCase();
    if (!sub) return undefined;
    if (sub === "run" || sub === "run-script") {
      const script = rest[1];
      if (!script) return undefined;
      if (BUILD_SCRIPT.test(script)) return { reason: `a build or test script (${cmd} run ${script})` };
      if (SERVE_SCRIPT.test(script)) return { reason: `a server or watcher script (${cmd} run ${script})` };
      return undefined;
    }
    if (NPM_BUILTIN_TEST.has(sub)) return { reason: `a test run (${cmd} ${sub})` };
    if (NPM_BUILTIN_START.has(sub)) return { reason: `a server script (${cmd} start)` };
    // yarn/pnpm/bun run scripts by bare name.
    if (cmd !== "npm" && BUILD_SCRIPT.test(sub)) return { reason: `a build or test script (${cmd} ${sub})` };
    if (cmd !== "npm" && SERVE_SCRIPT.test(sub)) return { reason: `a server or watcher script (${cmd} ${sub})` };
    return undefined;
  }

  if (cmd === "cargo") {
    const sub = args.find((a) => !a.startsWith("-") && !a.startsWith("+"));
    if (sub === "watch") return { reason: "a watcher (cargo watch)" };
    if (sub && CARGO_LONG.has(sub)) return { reason: `a cargo ${sub}` };
    return undefined;
  }
  if (cmd === "go" || cmd === "dotnet" || cmd === "deno" || cmd === "swift") {
    const sub = args.find((a) => !a.startsWith("-"));
    if (sub === "build" || sub === "test" || sub === "run" || sub === "vet") return { reason: `a ${cmd} ${sub}` };
    if (cmd === "dotnet" && sub === "watch") return { reason: "a watcher (dotnet watch)" };
    return undefined;
  }
  if (cmd === "vite" || cmd === "next" || cmd === "nuxt" || cmd === "astro" || cmd === "tauri") {
    const sub = args.find((a) => !a.startsWith("-"));
    if (sub === "build") return { reason: `a build (${cmd} build)` };
    if (sub === undefined || sub === "dev" || sub === "serve" || sub === "preview" || sub === "start") return { reason: `a dev server (${cmd}${sub ? " " + sub : ""})` };
    return undefined;
  }
  if (cmd === "python" || cmd === "python3" || cmd === "py") {
    const m = args.indexOf("-m");
    const mod = m !== -1 ? args[m + 1] : undefined;
    if (mod === "pytest" || mod === "unittest" || mod === "tox") return { reason: `a test run (python -m ${mod})` };
    if (mod === "http.server" || mod === "uvicorn" || mod === "flask") return { reason: `a server (python -m ${mod})` };
    return undefined;
  }
  if (BUILD_TOOLS.has(cmd)) return { reason: `a build or test run (${cmd})` };
  if (SERVER_TOOLS.has(cmd)) return { reason: `a server (${cmd})` };
  return undefined;
}

function classifyLine(line: string, depth: number): Classification | undefined {
  if (depth > MAX_DEPTH) throw new ParseError("nested too deeply");
  const subs: string[] = [];
  const commands = splitCommands(tokenize(line), subs);
  for (const c of commands) {
    if (c.background) continue; // `cmd &` already runs in the background
    const hit = classifyWords(c.words, depth);
    if (hit) return hit;
  }
  for (const s of subs) {
    const hit = classifyLine(s, depth + 1);
    if (hit) return hit;
  }
  return undefined;
}

/**
 * Classifies one Bash tool call's input. Returns undefined for anything that is not a long
 * command, is already in the background, or cannot be parsed.
 */
export function classifyBashCall(input: { command?: unknown; timeout?: unknown; run_in_background?: unknown }): Classification | undefined {
  if (input.run_in_background === true) return undefined;
  const command = typeof input.command === "string" ? input.command : "";
  try {
    const hit = command ? classifyLine(command, 0) : undefined;
    if (hit) return hit;
  } catch {
    return undefined; // a parse failure never blocks a tool
  }
  if (typeof input.timeout === "number" && input.timeout > TOOL_TIMEOUT_LIMIT_MS) return { reason: `a tool timeout over 2 minutes (${Math.round(input.timeout / 1000)} s)` };
  return undefined;
}

// ---- the per-turn policy ----------------------------------------------------------------

export const DENY_TEXT =
  "Helm: this looks like a long-running command (REASON). Run it in the background instead: call Bash again with run_in_background set to true, then check its output when it finishes. If the user explicitly asked for it to run in the foreground, repeat exactly the same command as your next Bash call and it will be allowed.";

export const REMIND_TEXT =
  "Helm reminder: this looks like a long-running command (REASON). Prefer running builds, tests, servers and watchers in the background (run_in_background: true) so the conversation is not blocked.";

/** Appended to the main chat's instructions when the rule is on. */
export const PROMPT_RULE =
  "Long commands: run builds, test suites, dev servers, watchers and long sleeps in the background (Bash with run_in_background: true) and check on them later, so the user can keep talking to you while they run. Only run them in the foreground when the user asks for that.";

export type HookDecision =
  | { kind: "allow" }
  | { kind: "deny"; reason: string; text: string }
  | { kind: "remind"; reason: string; text: string }
  | { kind: "escape"; reason: string };

export class LongCommandPolicy {
  private readonly level: LongCommandLevel;
  /** The command denied last, which may be repeated once as the turn's very next root Bash call. */
  private escape: string | undefined;

  constructor(level: LongCommandLevel) {
    this.level = level;
  }

  get enabled(): boolean {
    return this.level !== "off";
  }

  /** Decides one root Bash call. Subagent calls must not be passed here (they are exempt). */
  decide(input: { command?: unknown; timeout?: unknown; run_in_background?: unknown }): HookDecision {
    if (this.level === "off") return { kind: "allow" };
    const command = typeof input.command === "string" ? input.command : "";
    const escape = this.escape;
    this.escape = undefined; // any root Bash call clears the escape
    const hit = classifyBashCall(input);
    if (!hit) return { kind: "allow" };
    if (this.level === "warn") return { kind: "remind", reason: hit.reason, text: REMIND_TEXT.replace("REASON", hit.reason) };
    if (escape !== undefined && escape === command) return { kind: "escape", reason: hit.reason };
    this.escape = command;
    return { kind: "deny", reason: hit.reason, text: DENY_TEXT.replace("REASON", hit.reason) };
  }

  /** The turn ended: the escape does not carry into the next turn. */
  endTurn(): void {
    this.escape = undefined;
  }
}
