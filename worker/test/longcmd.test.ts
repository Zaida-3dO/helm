import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { classifyBashCall, DENY_TEXT, KILL_TEXT, LongCommandPolicy } from "../src/drivers/claude/longcmd.ts";

// [command, expected: true = classified as long, false = not]
const TABLE: [string, boolean][] = [
  // build and test runs
  ["npm test", true],
  ["npm t", true],
  ["npm run build", true],
  ["npm run test:unit -- --grep foo", true],
  ["npm run typecheck", true],
  ["pnpm test", true],
  ["yarn build", true],
  ["bun run check", true],
  ["npx vitest run", true],
  ["npx tsc --noEmit", true],
  ["cargo build --release", true],
  ["cargo test -p helm-core", true],
  ["cargo run", true],
  ["cargo +nightly check", true],
  ["go test ./...", true],
  ["python -m pytest -q", true],
  ["pytest -v tests/", true],
  ["make", true],
  // servers and watchers
  ["npm run dev", true],
  ["npm start", true],
  ["yarn dev", true],
  ["vite", true],
  ["node server.js --watch", true],
  ["cargo watch -x test", true],
  ["python -m http.server 8000", true],
  // sleeps
  ["sleep 30", true],
  ["sleep 2m", true],
  ["sleep 5", false],
  // compound and nested forms (Git Bash)
  ["cd worker && npm test", true],
  ["git status; npm run build", true],
  ["(cd host && cargo test)", true],
  ["echo $(npm test)", true],
  ["FOO=1 npm test", true],
  ['bash -c "npm run build"', true],
  ["timeout 600 cargo test", true],
  ["for d in a b; do npm test; done", true],
  ["if true; then cargo build; fi", true],
  ["! npm test", true],
  ["npm --prefix worker test", true],
  ["npm -w worker run build", true],
  ["sudo -u builder npm test", true],
  ["timeout -s KILL 30 npm test", true],
  ["for f in *.ts; do echo $f; done", false],
  // PowerShell and Windows forms
  ["npm.cmd test", true],
  ["& npm test", true],
  ['"C:\\tools\\nodejs\\npm.exe" run build', true],
  ["/c/tools/nodejs/npm.exe run build", true],
  ['powershell -Command "npm test"', true],
  ["Start-Sleep -Seconds 60", true],
  ["Start-Sleep -Seconds 3", false],
  ["timeout /t 30", true],
  ['cmd /c "cargo build"', true],
  // exempt: already in the background, version or help, text tools
  ["npm test &", false],
  ["nohup npm run dev", false],
  ["npm test --help", false],
  ["cargo --version", false],
  ["grep --watch foo", false],
  ["git log --oneline", false],
  ["echo npm test", false],
  ["ls -la", false],
  ["cat package.json", false],
  ["npm install", false],
  ["npm run lint:fix-imports-not-long", true],
  ["npm run format", false],
  ["cargo fmt --all", false],
  ["node -e \"setTimeout(()=>{},90000)\"", false],
  // heredoc bodies and comments are skipped
  ["cat <<EOF > notes.txt\nnpm test\nEOF\necho done", false],
  ["# npm test\nls", false],
  // a parse failure never blocks
  ['echo "unterminated', false],
  ["npm test 'oops", false],
];

describe("long-command classifier", () => {
  test(`table of ${TABLE.length} commands`, () => {
    assert.ok(TABLE.length >= 30);
    const wrong = TABLE.filter(([cmd, long]) => (classifyBashCall({ command: cmd }) !== undefined) !== long).map(([cmd, long]) => `${long ? "missed" : "false hit"}: ${cmd}`);
    assert.deepEqual(wrong, []);
  });

  test("the background flag exempts a call", () => {
    assert.equal(classifyBashCall({ command: "npm test", run_in_background: true }), undefined);
  });

  test("a tool timeout over 2 minutes counts; exactly 2 minutes does not", () => {
    assert.ok(classifyBashCall({ command: "node build.js", timeout: 120_001 }));
    assert.equal(classifyBashCall({ command: "node build.js", timeout: 120_000 }), undefined);
  });

  test("nesting deeper than 16 levels is a parse failure, not a block", () => {
    let cmd = "npm test";
    for (let i = 0; i < 20; i++) cmd = `echo $(${cmd})`;
    assert.equal(classifyBashCall({ command: cmd }), undefined);
    let shallow = "npm test";
    for (let i = 0; i < 3; i++) shallow = `echo $(${shallow})`;
    assert.ok(classifyBashCall({ command: shallow }));
  });

  test("the reason names the class", () => {
    assert.match(classifyBashCall({ command: "sleep 45" })!.reason, /sleep of 45 s/);
    assert.match(classifyBashCall({ command: "npm run dev" })!.reason, /server or watcher/);
  });
});

// [command, true = refused as ending Node or Claude Code by name]
const KILLS: [string, boolean][] = [
  ["taskkill /F /IM node.exe", true],
  ["taskkill //F //IM node.exe", true],
  ["taskkill /im NODE.EXE /f /t", true],
  ["taskkill -f -im node*", true],
  ["taskkill /F /IM claude.exe", true],
  ['taskkill /F /FI "IMAGENAME eq node.exe"', true],
  ['cmd /c "taskkill /F /IM node.exe"', true],
  ["Stop-Process -Name node -Force", true],
  ["Stop-Process -ProcessName node,chrome", true],
  ['powershell -Command "Stop-Process -Name node"', true],
  ["kill -Name claude", true],
  ["Get-Process node | Stop-Process -Force", true],
  ["Get-Process -Name node | Stop-Process", true],
  ["gps node | kill", true],
  ["pkill node", true],
  ["pkill -9 -f node", true],
  ["killall node.exe", true],
  ['wmic process where name="node.exe" delete', true],
  ["cd x && taskkill /F /IM node.exe", true],
  ["taskkill /F /IM node.exe &", true],
  // ending one process, or a process that is not shared
  ["taskkill /F /PID 4242", false],
  ["taskkill /F /T /PID 4242", false],
  ["taskkill /F /IM vite.exe", false],
  ["Stop-Process -Id 4242", false],
  ["kill 4242", false],
  ["kill -9 4242", false],
  ["pkill -f 'node server.js'", false],
  ["Get-Process node", false],
  ["Get-Process node | Select-Object Id", false],
  ["gps node; kill 4242", false],
  ["echo taskkill /IM node.exe", false],
  ["grep -n 'taskkill /IM node.exe' notes.md", false],
];

describe("kill by name guard", () => {
  test(`table of ${KILLS.length} commands`, () => {
    const wrong = KILLS.filter(([cmd, kill]) => (classifyBashCall({ command: cmd })?.kill === true) !== kill).map(([cmd, kill]) => `${kill ? "missed" : "false hit"}: ${cmd}`);
    assert.deepEqual(wrong, []);
  });

  test("refused every time at either level, in the background too, and never escaped; a kill wins over a long command", () => {
    for (const level of ["warn", "redirect"] as const) {
      const p = new LongCommandPolicy(level);
      for (let i = 0; i < 3; i++) {
        const d = p.decide({ command: "taskkill /F /IM node.exe" });
        assert.equal(d.kind, "deny", `${level} #${i}`);
        assert.equal((d as { text: string }).text, KILL_TEXT.replace("REASON", (d as { reason: string }).reason));
      }
      assert.equal(p.decide({ command: "pkill node", run_in_background: true }).kind, "deny");
    }
    assert.match(classifyBashCall({ command: "npm test; taskkill /IM node.exe /F" })!.reason, /ends every node process by name/);
    assert.match(KILL_TEXT, /taskkill \/PID <pid> \/F/);
  });
});

describe("long-command policy", () => {
  test("redirect denies once, then lets the exact repeat through as the very next root call", () => {
    const p = new LongCommandPolicy("redirect");
    const first = p.decide({ command: "npm test" });
    assert.equal(first.kind, "deny");
    assert.equal((first as { text: string }).text, DENY_TEXT.replace("REASON", (first as { reason: string }).reason));
    assert.equal(p.decide({ command: "npm test" }).kind, "escape");
    assert.equal(p.decide({ command: "npm test" }).kind, "deny", "the escape is used once");
  });

  test("any other root Bash call in between clears the escape", () => {
    const p = new LongCommandPolicy("redirect");
    assert.equal(p.decide({ command: "cargo build" }).kind, "deny");
    assert.equal(p.decide({ command: "git status" }).kind, "allow");
    assert.equal(p.decide({ command: "cargo build" }).kind, "deny");
  });

  test("a different long command is denied, not escaped", () => {
    const p = new LongCommandPolicy("redirect");
    p.decide({ command: "npm test" });
    assert.equal(p.decide({ command: "npm test -- --bail" }).kind, "deny");
  });

  test("the end of the turn clears the escape", () => {
    const p = new LongCommandPolicy("redirect");
    p.decide({ command: "npm test" });
    p.endTurn();
    assert.equal(p.decide({ command: "npm test" }).kind, "deny");
  });

  test("warn reminds and never denies; off allows everything", () => {
    const w = new LongCommandPolicy("warn");
    assert.equal(w.decide({ command: "npm test" }).kind, "remind");
    assert.equal(w.decide({ command: "npm test" }).kind, "remind");
    assert.equal(new LongCommandPolicy("off").decide({ command: "npm test" }).kind, "allow");
  });
});
