// Command dispatch for one worker: reads host commands from stdin, gates prompts behind the
// get_state handshake, and guarantees one response per id-bearing command.

import type { Readable } from "node:stream";
import { HostChannel, type ResponseBody } from "./channel.ts";
import { describeFault, LineReader, MAX_RECORD_BYTES } from "./lines.ts";
import type { Json, UiResponse } from "./types.ts";

export interface Driver {
  /** Starts the harness; resolves with the get_state data once it is ready. */
  ready(): Promise<Json>;
  prompt(cmd: Json): Promise<ResponseBody>;
  abort(cmd: Json): Promise<ResponseBody>;
  setModel(cmd: Json): Promise<ResponseBody>;
  setThinkingLevel(cmd: Json): Promise<ResponseBody>;
  uiResponse(resp: UiResponse): void;
  /** Ends the harness. Resolves when the child is gone. */
  shutdown(reason: string): Promise<void>;
}

/** Commands the protocol names but this worker does not implement yet (they answer with a refusal). */
const NOT_YET = new Set(["compact", "deliver_now", "stop_tool", "withdraw"]);

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
            return { success: true, data };
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
        answer(id, type, driver.prompt(c));
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
      default:
        if (id) {
          const error = NOT_YET.has(type) ? `${type} is not supported by this worker yet` : `unknown command: ${type}`;
          channel.respond(id, type, { success: false, error });
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
      onEnd: () => void finish(0, "host closed stdin"),
    },
    opts.maxRecordBytes ?? MAX_RECORD_BYTES,
  );
  opts.input.on("data", (c: Buffer) => reader.push(c));
  opts.input.on("end", () => reader.end());
}
