// Stdio servers spawn real executables. A `.cmd`/`.bat` is refused unless
// it is an npm-style shim, which is rewritten to node.exe and the shim's target script.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { planSpawn, shimTarget } from "../src/mcp/stdio.ts";
import { stdioServer } from "./mcp-helpers.ts";

function tmp(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "helm-mcp-spawn-"));
}

test("a package-bin shim is rewritten to node and its target", () => {
  const d = tmp();
  try {
    fs.mkdirSync(path.join(d, "node_modules", "pkg", "bin"), { recursive: true });
    const target = path.join(d, "node_modules", "pkg", "bin", "x.js");
    fs.writeFileSync(target, "");
    const shim = path.join(d, "x.cmd");
    fs.writeFileSync(shim, '@ECHO off\r\nSET dp0=%~dp0\r\n"%_prog%"  "%dp0%\\node_modules\\pkg\\bin\\x.js" %*\r\n');
    assert.equal(shimTarget(shim), target);
    const plan = planSpawn(stdioServer("s", {}, { command: shim, args: ["--a"] }), process.env, "win32", "C:\\node\\node.exe");
    assert.deepEqual(plan, { command: "C:\\node\\node.exe", args: [target, "--a"] });
  } finally {
    fs.rmSync(d, { recursive: true, force: true });
  }
});

test("npm's own npx.cmd runs npx-cli.js beside it", () => {
  const d = tmp();
  try {
    fs.mkdirSync(path.join(d, "node_modules", "npm", "bin"), { recursive: true });
    const cli = path.join(d, "node_modules", "npm", "bin", "npx-cli.js");
    fs.writeFileSync(cli, "");
    const shim = path.join(d, "npx.cmd");
    fs.writeFileSync(shim, ':: Created by npm\r\nSET "NPX_CLI_JS=%~dp0\\node_modules\\npm\\bin\\npx-cli.js"\r\n');
    const plan = planSpawn(stdioServer("s", {}, { command: "npx", args: ["-y", "pkg"] }), { PATH: d }, "win32", "node.exe");
    assert.deepEqual(plan, { command: "node.exe", args: [cli, "-y", "pkg"] });
  } finally {
    fs.rmSync(d, { recursive: true, force: true });
  }
});

test("any other .cmd or .bat is refused, and a target outside the shim's folder is not followed", () => {
  const d = tmp();
  try {
    const bat = path.join(d, "run.bat");
    fs.writeFileSync(bat, "@echo off\r\nnode server.js\r\n");
    assert.throws(() => planSpawn(stdioServer("s", {}, { command: bat }), process.env, "win32"), /\.cmd or \.bat/);
    const sneaky = path.join(d, "s.cmd");
    fs.writeFileSync(sneaky, '"%_prog%" "%dp0%\\..\\outside.js" %*\r\n');
    fs.writeFileSync(path.join(path.dirname(d), "outside.js"), "");
    assert.equal(shimTarget(sneaky), undefined);
    fs.rmSync(path.join(path.dirname(d), "outside.js"));
  } finally {
    fs.rmSync(d, { recursive: true, force: true });
  }
});

test("an .exe and non-Windows commands pass through unchanged", () => {
  const s = stdioServer("s", {}, { command: process.execPath, args: ["a"] });
  assert.deepEqual(planSpawn(s, process.env, "win32"), { command: process.execPath, args: ["a"] });
  assert.deepEqual(planSpawn(stdioServer("s", {}, { command: "x.cmd", args: [] }), process.env, "linux"), { command: "x.cmd", args: [] });
});
