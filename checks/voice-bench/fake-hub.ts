// A stand-in for the hub's relay line: one WebSocket endpoint at /relay that Helm's debug build
// connects to (HELM_TEST_RELAY_URL), recording every frame Helm sends and sending the frames a
// real hub would. Node has no WebSocket server, so this speaks just enough RFC 6455 by hand:
// text frames (also split into fragments), ping, pong and close, from one client at a time.
// It listens on the loopback address only and holds no state beyond the frames it saw.

import { createHash } from "node:crypto";
import http from "node:http";
import type { Duplex } from "node:stream";

const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/** A real hub pings every 30 s; Helm drops a line that has been silent for 100 s. */
const PING_MS = 30_000;

export type Frame = Record<string, any>;

export interface FakeHub {
  /** `ws://127.0.0.1:<port>/relay` */
  url: string;
  /** Every frame Helm sent, in order. */
  frames: Frame[];
  /** When a frame arrived, in `performance.now()` milliseconds. */
  arrivedAt(frame: Frame): number;
  connected(): boolean;
  waitConnected(ms: number): Promise<void>;
  /** Sends one frame down the line; throws when Helm is not connected. */
  send(frame: Frame): void;
  /** The first frame at index `from` or later that `pred` accepts; throws after `ms`. */
  waitFor(pred: (f: Frame) => boolean, ms: number, from?: number): Promise<Frame>;
  close(): Promise<void>;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Splits a byte stream into complete messages: `(opcode, payload)`. Control frames are never fragmented. */
export function decoder(onMessage: (opcode: number, payload: Buffer) => void): (chunk: Buffer) => void {
  let buf = Buffer.alloc(0);
  let fragments: Buffer[] = [];
  let first = 0;
  return (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    for (;;) {
      if (buf.length < 2) return;
      const fin = (buf[0] & 0x80) !== 0;
      const op = buf[0] & 0x0f;
      const masked = (buf[1] & 0x80) !== 0;
      let len = buf[1] & 0x7f;
      let at = 2;
      if (len === 126) {
        if (buf.length < 4) return;
        len = buf.readUInt16BE(2);
        at = 4;
      } else if (len === 127) {
        if (buf.length < 10) return;
        len = Number(buf.readBigUInt64BE(2));
        at = 10;
      }
      const end = at + (masked ? 4 : 0) + len;
      if (buf.length < end) return;
      let payload = Buffer.from(buf.subarray(at + (masked ? 4 : 0), end));
      if (masked) {
        const key = buf.subarray(at, at + 4);
        for (let i = 0; i < payload.length; i++) payload[i] ^= key[i % 4];
      }
      buf = buf.subarray(end);
      if (op >= 8) {
        onMessage(op, payload);
        continue;
      }
      if (op !== 0) {
        first = op;
        fragments = [];
      }
      fragments.push(payload);
      if (fin) {
        onMessage(first, Buffer.concat(fragments));
        fragments = [];
      }
    }
  };
}

/** One unmasked frame, as a server sends it. */
export function encode(opcode: number, payload: Buffer): Buffer {
  const n = payload.length;
  let head: Buffer;
  if (n < 126) head = Buffer.from([0x80 | opcode, n]);
  else if (n < 65_536) head = Buffer.from([0x80 | opcode, 126, n >> 8, n & 0xff]);
  else {
    head = Buffer.alloc(10);
    head[0] = 0x80 | opcode;
    head[1] = 127;
    head.writeBigUInt64BE(BigInt(n), 2);
  }
  return Buffer.concat([head, payload]);
}

/** Starts the fake hub. With a `token`, a connection must send it as `Authorization: Bearer <token>`. */
export async function startFakeHub(opts: { token?: string } = {}): Promise<FakeHub> {
  const frames: Frame[] = [];
  const stamps = new WeakMap<Frame, number>();
  let line: Duplex | undefined;
  const server = http.createServer((_req, res) => {
    res.statusCode = 404;
    res.end();
  });
  server.on("upgrade", (req, socket: Duplex, head: Buffer) => {
    const refuse = (status: string) => socket.end(`HTTP/1.1 ${status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`);
    if (req.url?.split("?")[0] !== "/relay") return void refuse("404 Not Found");
    if (opts.token && req.headers.authorization !== `Bearer ${opts.token}`) return void refuse("401 Unauthorized");
    const key = req.headers["sec-websocket-key"];
    if (typeof key !== "string") return void refuse("400 Bad Request");
    const accept = createHash("sha1")
      .update(key + GUID)
      .digest("base64");
    socket.write(["HTTP/1.1 101 Switching Protocols", "Upgrade: websocket", "Connection: Upgrade", `Sec-WebSocket-Accept: ${accept}`, "", ""].join("\r\n"));
    line?.destroy();
    line = socket;
    const feed = decoder((op, payload) => {
      if (op === 1) {
        try {
          const frame = JSON.parse(payload.toString("utf8")) as Frame;
          stamps.set(frame, performance.now());
          frames.push(frame);
        } catch {
          // Not JSON: a real hub would drop it too.
        }
      } else if (op === 9) socket.write(encode(10, payload));
      else if (op === 8) socket.end(encode(8, Buffer.alloc(0)));
    });
    socket.on("data", feed);
    socket.on("error", () => undefined);
    socket.on("close", () => {
      if (line === socket) line = undefined;
    });
    if (head.length) feed(head);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as import("node:net").AddressInfo).port;
  const pinger = setInterval(() => line?.write(encode(9, Buffer.alloc(0))), PING_MS);
  pinger.unref();

  const hub: FakeHub = {
    url: `ws://127.0.0.1:${port}/relay`,
    frames,
    arrivedAt: (frame) => stamps.get(frame) ?? Number.NaN,
    connected: () => line !== undefined,
    async waitConnected(ms) {
      const end = Date.now() + ms;
      while (!line) {
        if (Date.now() > end) throw new Error("Helm never connected to the fake hub");
        await sleep(50);
      }
    },
    send(frame) {
      if (!line) throw new Error("Helm is not connected to the fake hub");
      line.write(encode(1, Buffer.from(JSON.stringify(frame), "utf8")));
    },
    async waitFor(pred, ms, from = 0) {
      const end = Date.now() + ms;
      for (;;) {
        const found = frames.slice(from).find(pred);
        if (found) return found;
        if (Date.now() > end) throw new Error(`timed out after ${ms} ms waiting for a frame`);
        await sleep(50);
      }
    },
    async close() {
      clearInterval(pinger);
      line?.destroy();
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
  return hub;
}
