# Helm hub

The always-on service that runs on a home server, beside the PC that runs Helm. This
first part is **shared memory**: SQLite with an FTS5 index and optional vectors, served as MCP over
streamable HTTP. The relay and the Telegram bridge come later.

It is its own Cargo workspace and builds from `hub/` alone, so the container build needs nothing
else from the repo.

## Tools

| Tool | What it does |
|---|---|
| `memory_search {query, k?, vector?, model?, tags?}` | Keyword search (FTS5, porter stemming, bm25 with title > description > tags > body). With `vector`, a hybrid: keyword and cosine rankings fused by reciprocal rank. Only vectors of the same dimension (and `model`, if given) are compared. Every listed tag must match. |
| `memory_read {id}` | One memory in full, by numeric id or by name |
| `memory_write {title, body, tags?, source?, name?, vector?, model?}` | New memory. A vector must come with the id of the model that made it |
| `memory_list {tag?, since?, limit?}` | Newest first; `since` is an ISO-8601 date or UTC timestamp |
| `memory_update`, `memory_delete` | Only when `HUB_ALLOW_MUTATE=1`. A new body without a new vector clears the old vector |

**The hub never computes an embedding.** Callers that have one (Helm on the PC) pass it; callers
that do not (another assistant, or anyone while the PC is asleep) get keyword search.

## Auth

`HUB_TOKENS=client:token[:scope],...`, scope `rw` (default) or `ro`. Tokens are at least 24
characters, unique per client, held only as SHA-256 digests and compared in constant time. A
read-only client sees and may call only the three read tools. `GET /health` is the one open route
and reports only status and version. The server refuses to start with no tokens configured.

## Commands

```
helm-hub serve                       # HUB_DB, HUB_BIND, HUB_TOKENS, HUB_ALLOW_MUTATE
helm-hub import <dir> [--source S]   # S: the source tag the imported memories carry
```

`import` reads the top-level `*.md` files of a folder (subfolders are not read). Frontmatter `name`,
`description` and `type` (or `metadata.type`) are kept, and so are the body and its `[[wiki links]]`.
The memory's title is the body's first `# heading`, else its name. A file with no frontmatter is
keyed on its file stem. The import is idempotent on `name`: unchanged files are left alone, changed
files are updated in place, and a changed body drops that memory's stored vector, since the vector
described the old text. `MEMORY.md` and `README.md` are index files and are skipped. Two files
claiming one name are reported as failures rather than overwriting each other. Imported memories
are tagged `imported`, the source tag, and their type.

## Build, test, run

```
cd hub
cargo test
cargo clippy --all-targets -- -D warnings
docker build -t helm-hub:latest .
```

`compose.snippet.yml` is the service block for the server's compose group. Run the import inside
the container: `docker exec helm-hub helm-hub import /mnt/memory`.

## Budget

One-thread runtime, one SQLite connection, a 2 MB page cache. A release build idles at about 7 MB
of working set with ~340 memories loaded (measured on Windows). Vector search is a brute-force
scan, which is fine below roughly 50k rows.
