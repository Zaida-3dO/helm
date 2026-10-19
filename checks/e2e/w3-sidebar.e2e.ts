// The sidebar end to end in the real debug app: one read-only sync from fakes/board, board groups
// in the board's order, a three-level local tree, a local rename that survives a second sync, a
// chat nested by drag, geometry, and no write ever sent to the board.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { fixtureProjects, startFakeBoard, type FakeBoard } from "../../fakes/board/server.ts";
import { api, dispose, launchApp, newChat, scratchDir, shot, tid, type App } from "./harness.ts";

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
let app: App | undefined;
let board: FakeBoard | undefined;
const page = () => app!.page;

interface Node {
  id: string;
  name: string;
  revision: number;
  kind: string;
  board: { boardProjectId: string } | null;
}

async function boardGroups(): Promise<{ id: string; title: string; depth: string; height: number }[]> {
  return page().$$eval('[data-testid="sidebar-group"][data-kind="board"]', (els) =>
    els.map((e) => ({
      id: e.getAttribute("data-project-id") ?? "",
      title: e.querySelector(".sb-title")?.textContent ?? "",
      depth: e.getAttribute("data-depth") ?? "",
      height: Math.round(e.getBoundingClientRect().height),
    })),
  );
}

/** Empty projects hide by default; the fixture's board projects hold no
 *  chats, so reveal them before counting groups. The choice is remembered for the session. */
async function showEmptyProjects(p = page()): Promise<void> {
  const t = p.locator(tid("sidebar-empty-toggle"));
  await t.waitFor({ timeout: 20_000 });
  if ((await t.getAttribute("aria-pressed")) !== "true") await t.click();
}

async function waitSync(state: string): Promise<void> {
  await page().waitForSelector(tid("board-sync", `[data-state="${state}"]`), { state: "attached", timeout: 30_000 });
}

describe("sidebar: Agent Standup sync, groups and nesting", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    board = await startFakeBoard();
    app = await launchApp(profile, { env: { HELM_TEST_STANDUP_URL: board.url, HELM_TEST_STANDUP_TOKEN: board.token } });
  });
  after(async () => {
    await dispose(app);
    await board?.close();
  });

  test("S1: one sync shows the board's projects as top-level groups, in order, read-only", async () => {
    try {
      await waitSync("ok");
      await showEmptyProjects();
      const fixtures = fixtureProjects();
      await page().waitForFunction((n) => document.querySelectorAll('[data-testid="sidebar-group"][data-kind="board"]').length >= n, fixtures.length, { timeout: 20_000 });
      const groups = await boardGroups();
      assert.deepEqual(groups.map((g) => g.title), fixtures.map((f) => f.title), "board order and titles");
      assert.ok(groups.every((g) => g.depth === "0"), "Agent Standup projects are top level");
      assert.ok(groups.every((g) => g.height === 38), `header element 38 px (40 with its gap): ${groups.map((g) => g.height)}`);
      const width = await page().$eval(".shell-sidebar", (e) => Math.round(e.getBoundingClientRect().width));
      assert.equal(width, 480, "sidebar 480 px by default");
      assert.deepEqual(board!.violations, [], "no request but GET reached the board");
      assert.ok(board!.requests.every((r) => r.authorized), "every request carried the token");
    } finally {
      await shot(page(), OUT, "S1");
    }
  });

  test("S2: local nesting makes a three-level tree; a local rename survives a second sync", async () => {
    try {
      const nodes = (await api(page(), "project_list", {})) as Node[];
      const byBoard = (id: string) => nodes.find((n) => n.board?.boardProjectId === id)!;
      const [a, b, c] = [byBoard("fb-desktop"), byBoard("fb-website"), byBoard("fb-home")];
      await api(page(), "project_update", { projectId: b.id, revision: b.revision, parentId: a.id });
      const c1 = (await api(page(), "project_update", { projectId: c.id, revision: c.revision, parentId: b.id })) as Node;
      await page().waitForSelector(`[data-testid="sidebar-group"][data-project-id="${c.id}"][data-depth="2"]`, { timeout: 15_000 });
      const depth = (id: string) => page().getAttribute(`[data-testid="sidebar-group"][data-project-id="${id}"]`, "data-depth");
      assert.deepEqual([await depth(a.id), await depth(b.id), await depth(c.id)], ["0", "1", "2"]);
      // Rename locally, then sync again: the local name stays and the board was not written.
      await api(page(), "project_update", { projectId: c.id, revision: c1.revision, name: "My home automations" });
      await page().click(`${tid("board-sync")} .sb-sync-refresh`);
      await waitSync("ok");
      await page().waitForFunction((id) => document.querySelector(`[data-testid="sidebar-group"][data-project-id="${id}"] .sb-title`)?.textContent === "My home automations", c.id, { timeout: 15_000 });
      assert.equal(await depth(c.id), "2", "nesting survives the sync");
      assert.deepEqual(board!.violations, []);
    } finally {
      await shot(page(), OUT, "S2");
    }
  });

  test("S3: chat rows are 34 px (36 with the gap); a chat dragged under another nests at depth 1", async () => {
    try {
      const folder = scratchDir("helm-e2e-chat-");
      const first = await newChat(page(), folder);
      const second = await newChat(page(), folder);
      const row = (id: string) => page().locator(tid("chat-item", `[data-chat-id="${id}"]`));
      const h = await row(first).evaluate((e) => Math.round(e.getBoundingClientRect().height));
      assert.equal(h, 34);
      const src = await row(second).boundingBox();
      const dst = await row(first).boundingBox();
      assert.ok(src && dst);
      await page().mouse.move(src.x + 40, src.y + src.height / 2);
      await page().mouse.down();
      await page().mouse.move(src.x + 40, src.y + src.height / 2 + 8, { steps: 4 });
      await page().mouse.move(dst.x + 60, dst.y + dst.height - 4, { steps: 8 });
      await page().mouse.up();
      await page().waitForSelector(tid("chat-item", `[data-chat-id="${second}"][data-depth="1"]`), { timeout: 15_000 });
      // Collapsing the parent hides it.
      await page().click(`.sb-chat-wrap[data-chat-id="${first}"] .sb-children`);
      await page().waitForSelector(tid("chat-item", `[data-chat-id="${second}"]`), { state: "detached", timeout: 10_000 });
      assert.deepEqual(await app!.cspViolations(), []);
    } finally {
      await shot(page(), OUT, "S3");
    }
  });
});
