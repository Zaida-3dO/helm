// A deliberately misbehaving "worker" used to prove the fake host's protocol checks can fail.
// argv: <launch.json>. Reads {"mode": ...} from launch.json.
import fs from "node:fs";

const launch = JSON.parse(fs.readFileSync(process.argv[2], "utf8")) as { mode: string };
let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (c: string) => {
  buf += c;
  let nl: number;
  while ((nl = buf.indexOf("\n")) !== -1) {
    const cmd = JSON.parse(buf.slice(0, nl)) as { type: string; id?: string };
    buf = buf.slice(nl + 1);
    if (cmd.type === "shutdown") process.exit(0);
    const resp = JSON.stringify({ type: "response", id: cmd.id, command: cmd.type, success: true });
    if (launch.mode === "double") process.stdout.write(resp + "\n" + resp + "\n");
    else if (launch.mode === "garbage") process.stdout.write("hello there\n" + resp + "\n");
    else if (launch.mode === "wrong-command") process.stdout.write(JSON.stringify({ type: "response", id: cmd.id, command: "other", success: true }) + "\n");
    else process.stdout.write(resp + "\n");
  }
});
