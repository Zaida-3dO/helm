// What the chosen claude CLI can accept for `--agents`. Newer CLIs take the path of a file of
// agent definitions (with --print); older ones only take the JSON itself; the oldest have no
// such flag. The version is read once per binary path.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import { compareVersions } from "./resolve.ts";

/** First version that took the JSON itself on `--agents`. */
export const AGENTS_INLINE_MIN = "1.0.60";
/**
 * First version we rely on to take a file path on `--agents`. The installed 2.1.289 documents it
 * ("JSON object ... or with --print the path to a file that holds one"); the exact release it
 * landed in is not recorded in any help text, so this is a cautious floor: anything older is
 * handed the JSON inline, which every CLI that has the flag understands.
 */
export const AGENTS_FILE_MIN = "2.1.0";

/**
 * First version we rely on to take a file path on `--append-system-prompt-file`. The installed
 * 2.1.289 documents it in its help ("--append-system-prompt-file <file>"); the release it landed
 * in is not recorded anywhere, so like AGENTS_FILE_MIN this is a cautious floor: an older CLI is
 * handed the text inline instead.
 */
export const APPEND_FILE_MIN = "2.1.0";

export type AppendMode = "file" | "inline";

/** How to carry a chat's appended prompt for a CLI of this version. An unknown version is trusted with the file. */
export function appendMode(version: string | undefined): AppendMode {
  if (!version) return "file";
  return compareVersions(version, APPEND_FILE_MIN) < 0 ? "inline" : "file";
}

export type AgentsMode = "file" | "inline" | "unsupported";

/** The first `major.minor.patch` in a `claude --version` line ("2.1.289 (Claude Code)"), or undefined. */
export function parseCliVersion(out: string): string | undefined {
  return /(\d+)\.(\d+)\.(\d+)/.exec(out)?.[0];
}

/** How to pass the agents to a CLI of this version. An unknown version is trusted with the file. */
export function agentsMode(version: string | undefined): AgentsMode {
  if (!version) return "file";
  if (compareVersions(version, AGENTS_INLINE_MIN) < 0) return "unsupported";
  return compareVersions(version, AGENTS_FILE_MIN) < 0 ? "inline" : "file";
}

/**
 * The value for `--agents`: the file path, the file's JSON, or an error naming the CLI's version
 * and path when it has no such flag.
 */
export function agentsArg(file: string, version: string | undefined, cliPath: string, read: (p: string) => string = (p) => fs.readFileSync(p, "utf8")): string {
  const mode = agentsMode(version);
  if (mode === "file") return file;
  if (mode === "unsupported") {
    throw new Error(`the claude CLI at ${cliPath} is version ${version}, too old to take custom agents (needs ${AGENTS_INLINE_MIN} or newer): update claude`);
  }
  return JSON.stringify(JSON.parse(read(file)));
}

const cache = new Map<string, string | undefined>();

/** `claude --version` for this binary, cached by path; undefined when it cannot be read. */
export function cliVersion(cliPath: string, run: (p: string) => string = (p) => execFileSync(p, ["--version"], { encoding: "utf8", timeout: 10_000, windowsHide: true })): string | undefined {
  if (cache.has(cliPath)) return cache.get(cliPath);
  let v: string | undefined;
  try {
    v = parseCliVersion(run(cliPath));
  } catch {
    v = undefined;
  }
  cache.set(cliPath, v);
  return v;
}
