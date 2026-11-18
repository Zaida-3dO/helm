// Command dispatch for one worker: reads host commands from stdin, gates prompts behind the
// get_state handshake, and guarantees one response per id-bearing command.

import fs from "node:fs";
import path from "node:path";
import type { Readable } from "node:stream";
import { HostChannel, type ResponseBody } from "./channel.ts";
import { describeFault, LineReader, MAX_RECORD_BYTES } from "./lines.ts";
import { launchMarks, markLaunch, markPrompt, promptMarks } from "./timing.ts";
import type { Json, UiResponse } from "./types.ts";

export interface Driver {
  /** Starts the harness; resolves with the get_state data once it is ready. */
  ready(): Promise<Json>;
  prompt(cmd: Json): Promise<ResponseBody>;
  abort(cmd: Json): Promise<ResponseBody>;
  setModel(cmd: Json): Promise<ResponseBody>;
  setThinkingLevel(cmd: Json): Promise<ResponseBody>;
  uiResponse(resp: UiResponse): void;
  /** Called once a prompt's response has been written, so later events can follow it. */
  promptAnswered?(submissionId: unknown): void;
  deliverNow(cmd: Json): Promise<ResponseBody>;
  withdraw(cmd: Json): Promise<ResponseBody>;
  stopTool(cmd: Json): Promise<ResponseBody>;
  compact(cmd: Json): Promise<ResponseBody>;
  /** Ends the harness. Resolves when the child is gone. */
  shutdown(reason: string): Promise<void>;
  /** A host line for a worker-side service (`mcp_config`); false when nothing took it. */
  hostLine?(cmd: Json): boolean;
}

/** Commands that are answered only once the harness is ready (as `prompt` is). */
const AFTER_READY: Record<string, keyof Pick<Driver, "deliverNow" | "withdraw" | "stopTool" | "compact">> = {
  deliver_now: "deliverNow",
  withdraw: "withdraw",
  stop_tool: "stopTool",
  compact: "compact",
};

/**
 * A line in the host's diagnostics folder saying this worker lost its host. It tells an
 * outside kill apart from a quit, seen from the workers' side. Best-effort: a failed note
 * is only a stderr line.
 */
function noteHostGone(diag: (msg: string) => void): void {
  const root = process.env.HELM_DATA_ROOT;
  if (!root) return;
  try {
    const dir = path.join(root, "diagnostics");
    fs.mkdirSync(dir, { recursive: true });
    const p = path.join(dir, "host-gone.log");
    // One rolling file, like the host's own logs.
    if (fs.existsSync(p) && fs.statSync(p).size > 512 * 1024) {
      fs.renameSync(p, path.join(dir, "host-gone.old.log"));
    }
    fs.appendFileSync(
      p,
      `${new Date().toISOString()} host went away: this worker's stdin closed (worker pid ${process.pid}, chat ${process.env.HELM_CHAT_ID ?? "?"})\n`,
    );
  } catch (e) {
    diag(`host-gone note not written: ${(e as Error).message}`);
  }
}

export interface ServeOptions {
  input: Readable;
  channel: HostChannel;
  driver: Driver;
  diag: (msg: string) => void;
  exit: (code: number) => void;
  maxRecordBytes?: number;
}

export function serve(opts: ServeOptions): void {
  const { channel, driver, diag } = opts;
  let readyAnswered = false;
  let ending = false;

  const finish = async (code: number, reason: string) => {
    if (ending) return;
    ending = true;
    try {
      await driver.shutdown(reason);
    } catch (e) {
      diag(`shutdown error: ${(e as Error).message}`);
    }
    await channel.flush();
    opts.exit(code);
  };

  const answer = (id: string, command: string, p: Promise<ResponseBody>) => {
    p.then(
      (body) => channel.respond(id, command, body),
      (e: unknown) => channel.respond(id, command, { success: false, error: String((e as Error)?.message ?? e) }),
    );
  };

  const onRecord = (cmd: Json) => {
    if (ending) return;
    const type = typeof cmd.type === "string" ? cmd.type : undefined;
    const id = typeof cmd.id === "string" ? cmd.id : undefined;
    if (!type) {
      if (id) channel.respond(id, "unknown", { success: false, error: "command has no type" });
      else diag("ignoring a command with no type");
      return;
    }
    switch (type) {
      case "get_state":
        if (!id) return diag("get_state without id ignored");
        answer(
          id,
          type,
          driver.ready().then((data) => {
            readyAnswered = true;
            markLaunch("ready");
            return { success: true, data, timing: launchMarks() };
          }),
        );
        return;
      case "prompt":
      case "steer": {
        if (!id) return diag(`${type} without id ignored`);
        if (!readyAnswered) {
          channel.respond(id, type, { success: false, error: "worker not ready: get_state has not completed" });
          return;
        }
        const c = type === "steer" ? { ...cmd, streamingBehavior: "steer" } : cmd;
        markPrompt(c.submissionId, "received");
        driver.prompt(c).then(
          (body) => {
            channel.respond(id, type, { ...body, timing: promptMarks(c.submissionId) });
            driver.promptAnswered?.(c.submissionId);
          },
          (e: unknown) => channel.respond(id, type, { success: false, error: String((e as Error)?.message ?? e) }),
        );
        return;
      }
      case "abort":
        if (id) answer(id, type, driver.abort(cmd));
        else void driver.abort(cmd);
        return;
      case "set_model":
        if (!id) return diag("set_model without id ignored");
        answer(id, type, driver.setModel(cmd));
        return;
      case "set_thinking_level":
        if (!id) return diag("set_thinking_level without id ignored");
        answer(id, type, driver.setThinkingLevel(cmd));
        return;
      case "extension_ui_response":
        if (!id) return diag("extension_ui_response without id ignored");
        driver.uiResponse(cmd as unknown as UiResponse);
        return;
      case "shutdown":
        void finish(0, "shutdown requested");
        return;
      case "mcp_config":
        // The MCP client's configuration; no response is expected.
        if (!driver.hostLine?.(cmd)) diag("mcp_config ignored: no MCP client in this worker");
        return;
      case "mcp_oauth_header":
        // The answer to a header ask for a browser-signed-in server; no response is expected. The
        // line carries a credential, so nothing of it is logged.
        if (!driver.hostLine?.(cmd)) diag("mcp_oauth_header ignored: no MCP client in this worker");
        return;
      case "mcp_owner_grant":
        // The person approved one browser step: the grant retries it. No response is expected,
        // and the line carries the grant, so nothing of it is logged.
        if (!driver.hostLine?.(cmd)) diag("mcp_owner_grant ignored: no MCP client in this worker");
        return;
      case "set_permission_mode":
        // The composer's permission-mode pick; applied before the next prompt, no response.
        if (!driver.hostLine?.(cmd)) diag("set_permission_mode ignored: this worker has no permission modes");
        return;
      case "guard_verdict":
        // The landing guard's answer to a guard_check (drivers/claude/guard.ts); no response.
        if (!driver.hostLine?.(cmd)) diag("guard_verdict ignored: nothing in this worker asked");
        return;
      case "deliver_now":
      case "withdraw":
      case "stop_tool":
      case "compact": {
        if (!id) return diag(`${type} without id ignored`);
        if (!readyAnswered) {
          channel.respond(id, type, { success: false, error: "worker not ready: get_state has not completed" });
          return;
        }
        const method = AFTER_READY[type];
        answer(id, type, driver[method](cmd));
        return;
      }
      default:
        if (id) {
          channel.respond(id, type, { success: false, error: `unknown command: ${type}` });
        } else {
          diag(`ignoring unknown command ${type}`);
        }
    }
  };

  const reader = new LineReader(
    {
      onRecord,
      onFault: (f) => {
        channel.emit({ type: "worker_error", error: `bad command from host: ${describeFault(f)}` });
        void finish(1, "bad host record");
      },
      onEnd: () => {
        // The host is gone (killed or crashed) unless the quit sequence already said so;
        // `ending` guards the orderly case inside finish.
        if (!ending) noteHostGone(diag);
        void finish(0, "host closed stdin");
      },
    },
    opts.maxRecordBytes ?? MAX_RECORD_BYTES,
  );
  opts.input.on("data", (c: Buffer) => reader.push(c));
  opts.input.on("end", () => reader.end());
}
