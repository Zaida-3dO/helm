// Host-tool names the worker itself serves.
//
// The host sends every other host tool in launch.json's hostTools[]; these are the worker's own
// compatibility tools. Every name here is also in host/crates/hosttools/reserved.json, and
// test/hosttools-reserved.test.ts fails if the two drift.

/** Always listed on Claude Code: search the current host tools, call any by name. */
export const COMPAT_ALWAYS = ["app_list_tools", "app_mcp_call"] as const;

/** Listed whenever the worker has an MCP client, even before any server is configured: the host
 * sends the server list only once the worker is ready, after Claude Code has already read this
 * server's tool list, and that list is never read again. */
export const COMPAT_WITH_MCP = ["app_mcp_tools"] as const;

/** The host tools a local chat lists in its tool list. These are the tools the launch prompt's
 * guidance names, plus web search (a local chat has no WebSearch). Every other host tool stays
 * reachable: `app_list_tools` still searches all of them and `app_mcp_call` still calls any of
 * them — a local server cannot expand tool-search results, so every listed schema is sent in
 * every request and the list has to stay small. */
export const LOCAL_CORE_TOOLS: readonly string[] = [
  "app_this_chat",
  "app_send_message",
  "app_create_chat",
  "app_read_chat",
  "app_list_chats",
  "app_update_chat",
  "app_offer_options",
  "app_show_images",
  "app_link_item",
  "app_save_rule",
  "app_wait_for",
  "app_flag_attention",
  "app_web_search",
  "app_worktree_status",
  "app_workflow",
  "app_workflow_report",
  // A long call moved to the background tells the agent to read its result with this one.
  "app_background_result",
];

export const WORKER_SERVED: readonly string[] = [...COMPAT_ALWAYS, ...COMPAT_WITH_MCP];
