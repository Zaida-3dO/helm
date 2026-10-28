// The sidebar end to end in the real debug app: one read-only sync from fakes/board, board groups
// in the board's order, a three-level local tree, a local rename that survives a second sync, a
// chat nested by drag, geometry, the read-only Tasks tab (columns, a card, its drawer, Ctrl+K and a
// new chat for a task), and no write ever sent to the board.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { fixtureProjects, startFakeBoard, type FakeBoard } from "../../fakes/board/server.ts";
import { api, dispose, launchApp, newChat, scratchDir, seedBoardChats, shot, tid, type App } from "./harness.ts";

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
      await seedBoardChats(page());
      const fixtures = fixtureProjects();
      await page().waitForFunction((n) => document.querySelectorAll('[data-testid="sidebar-group"][data-kind="board"]').length >= n, fixtures.length, { timeout: 20_000 });
      const groups = await boardGroups();
      assert.deepEqual(groups.map((g) => g.title), fixtures.map((f) => f.title), "board order and titles");
      assert.ok(groups.every((g) => g.depth === "0"), "Agent Standup projects are top level");
      assert.ok(groups.every((g) => g.height === 40), `header slot 40 px (the 38 px header and its gap): ${groups.map((g) => g.height)}`);
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

  test("S3: chat rows fill a 36 px slot; a chat dragged under another nests at depth 1", async () => {
    try {
      const folder = scratchDir("helm-e2e-chat-");
      const first = await newChat(page(), folder);
      const second = await newChat(page(), folder);
      const row = (id: string) => page().locator(tid("chat-item", `[data-chat-id="${id}"]`));
      // The newer chat is listed first and fills a whole 36 px slot; the older one is the last of its
      // block, whose slot is 4 px shorter (the gap to the next block).
      const h = await row(second).evaluate((e) => Math.round(e.getBoundingClientRect().height));
      assert.equal(h, 36);
      const last = await row(first).evaluate((e) => Math.round(e.getBoundingClientRect().height));
      assert.equal(last, 32);
      // Drag the lower (older) chat onto the lower half of the one above it, so nothing above the
      // target moves while it is dragged.
      const src = await row(first).boundingBox();
      const dst = await row(second).boundingBox();
      assert.ok(src && dst);
      await page().mouse.move(src.x + 40, src.y + src.height / 2);
      await page().mouse.down();
      await page().mouse.move(src.x + 40, src.y + src.height / 2 + 8, { steps: 4 });
      await page().mouse.move(dst.x + 60, dst.y + dst.height - 4, { steps: 8 });
      await page().mouse.up();
      await page().waitForSelector(tid("chat-item", `[data-chat-id="${first}"][data-depth="1"]`), { timeout: 15_000 });
      // Collapsing the parent hides it.
      await page().click(`.sb-chat-wrap[data-chat-id="${second}"] .sb-children`);
      await page().waitForSelector(tid("chat-item", `[data-chat-id="${first}"]`), { state: "detached", timeout: 10_000 });
      assert.deepEqual(await app!.cspViolations(), []);
    } finally {
      await shot(page(), OUT, "S3");
    }
  });

  test("S4: Tasks reads the board in the side panel; a card opens its drawer; Ctrl+K finds the task; a new chat for it starts with its details", async () => {
    const MERGE = "5c9e2a41-0f0b-4d6e-9d1f-0a7c1e3b9a10";
    try {
      await page().click(tid("nav-tasks"));
      await page().waitForSelector(tid("tasks-view", '[data-loaded="true"]'), { timeout: 30_000 });
      const counts = () => page().$$eval(tid("tasks-column-count"), (els) => els.map((e) => e.textContent));
      assert.deepEqual(await counts(), ["30 / 32", "2 / 2", "1 / 1", "1 / 1"]);
      await page().click(tid("tasks-column-more"));
      await page().waitForFunction(() => document.querySelector('[data-testid="tasks-column"][data-column="backlog"] [data-testid="tasks-column-count"]')?.textContent === "32 / 32", null, { timeout: 15_000 });

      // The card names its holder (matched against the fleet) and, once in view, its checkpoint.
      const card = tid("tasks-card", `[data-item-id="${MERGE}"]`);
      await page().waitForSelector(`${card} ${tid("tasks-card-checkpoint")}`, { timeout: 15_000 });
      const text = (await page().textContent(card)) ?? "";
      assert.ok(text.includes("builder: builder-one · workbench") && text.includes("Reproduced the stall"), text);

      // The drawer: description, one safe link and one inert, 20 of the 25 history entries. Only the
      // item's own link row is counted: an evidence entry with a web reference is a link too.
      await page().click(card);
      await page().waitForSelector(tid("tasks-drawer", `[data-item-id="${MERGE}"]`), { timeout: 15_000 });
      await page().waitForFunction(() => document.querySelector('[data-testid="tasks-drawer-body"]')?.textContent?.includes("merge queue stops"), null, { timeout: 15_000 });
      const links = `${tid("tasks-drawer")} .tk-links`;
      assert.deepEqual(await page().$$eval(`${links} a.tk-link`, (els) => els.map((e) => e.getAttribute("href"))), ["https://github.com/example/desktop/pull/12"]);
      assert.ok(((await page().textContent(`${links} .tk-link-inert`)) ?? "").includes("javascript:alert(1)"));
      assert.equal(await page().locator(`${links} .tk-link-inert a, ${tid("tasks-drawer")} a[href^="javascript:"]`).count(), 0);
      await page().waitForFunction(() => [...document.querySelectorAll('[data-testid="tasks-drawer-section"] h4')].some((h) => h.textContent?.includes("20 of 25")), null, { timeout: 15_000 });

      // Ctrl+K: the Board group finds the task and choosing it opens its drawer.
      await page().click(tid("tasks-drawer-close"));
      await page().keyboard.press("Control+K");
      await page().waitForSelector(tid("search-dialog"), { timeout: 10_000 });
      await page().fill(tid("search-input"), "merge queue");
      await page().waitForSelector(tid("search-hit", '[data-group="board"]'), { timeout: 15_000 });
      await page().keyboard.press("Enter");
      await page().waitForSelector(tid("tasks-drawer", `[data-item-id="${MERGE}"]`), { timeout: 15_000 });

      // New chat in this task: the draft carries the details, and nothing is sent.
      await page().click(tid("tasks-chat-menu"));
      await page().getByText("New chat in this task").click();
      await page().waitForFunction((id) => document.querySelector('[data-testid="composer"]')?.textContent?.includes(id), MERGE, { timeout: 15_000 });
      const draft = (await page().textContent(tid("composer"))) ?? "";
      assert.ok(draft.includes("Task board item") && draft.includes("Fix the merge queue"), draft);

      assert.deepEqual(board!.violations, [], "no request but GET reached the board");
      assert.ok(board!.requests.every((r) => r.authorized || r.path === "/api/ready"), "every request but readiness carried the token");
      assert.deepEqual(await app!.cspViolations(), []);
    } finally {
      await shot(page(), OUT, "S4");
    }
  });
});
