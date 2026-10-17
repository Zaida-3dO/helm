// Host-tool names the worker itself serves.
//
// The host sends every other host tool in launch.json's hostTools[]; these are the worker's own
// compatibility tools. Every name here is also in host/crates/hosttools/reserved.json, and
// test/hosttools-reserved.test.ts fails if the two drift.

/** Always listed on Claude Code: search the current host tools, call any by name. */
export const COMPAT_ALWAYS = ["app_list_tools", "app_mcp_call"] as const;

/** Listed when at least one MCP server is enabled. */
export const COMPAT_WITH_MCP = ["app_mcp_tools"] as const;

export const WORKER_SERVED: readonly string[] = [...COMPAT_ALWAYS, ...COMPAT_WITH_MCP];
