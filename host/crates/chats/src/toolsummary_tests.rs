use super::toolsummary::tool_summary;
use serde_json::json;

/// One table, one row per call shape. The examples are invented; none is a real note, command
/// or address.
#[test]
fn one_table_says_what_each_call_is_doing() {
    let cases: &[(&str, serde_json::Value, &str)] = &[
        // A call that describes itself: the description is the summary.
        (
            "Bash",
            json!({"command": "ls reports/", "description": "List the report folder"}),
            "List the report folder",
        ),
        (
            "Bash",
            json!({"command": "ls", "description": "  Spaced   out   words  "}),
            "Spaced out words",
        ),
        (
            "Agent",
            json!({"description": "Audit the upload flow"}),
            "Audit the upload flow",
        ),
        (
            "Task",
            json!({"description": "Map the queue paths"}),
            "Map the queue paths",
        ),
        (
            "mcp__home_assistant__turn_on",
            json!({"entity_id": "light.kitchen", "description": "Dim the kitchen lights"}),
            "Dim the kitchen lights",
        ),
        // Shell without a description: a fixed phrase, never the command line, which could
        // carry a secret.
        (
            "Bash",
            json!({"command": "git status"}),
            "Running a command",
        ),
        (
            "Bash",
            json!({"command": "curl -H 'Authorization: Bearer sk-live-1' https://x\nls"}),
            "Running a command",
        ),
        (
            "PowerShell",
            json!({"cmd": "$env:TOKEN='abc'; Invoke-WebRequest"}),
            "Running a command",
        ),
        ("Bash", json!({}), "Running a command"),
        // File tools: the file's name, not the path.
        (
            "Read",
            json!({"file_path": "C:/notes/house/summer.md", "offset": 10}),
            "Reading summer.md",
        ),
        (
            "Read",
            json!({"file_path": "D:\\notes\\house.md"}),
            "Reading house.md",
        ),
        ("Read", json!({}), "Read"),
        (
            "Write",
            json!({"file_path": "src/app/toolsummary.rs", "content": "x"}),
            "Writing toolsummary.rs",
        ),
        (
            "Edit",
            json!({"file_path": "worker/src/driver.ts", "old_string": "a", "new_string": "b"}),
            "Editing driver.ts",
        ),
        (
            "MultiEdit",
            json!({"file_path": "C:\\x\\panel.rs"}),
            "Editing panel.rs",
        ),
        (
            "NotebookEdit",
            json!({"notebook_path": "D:/nb/plan.ipynb"}),
            "Editing plan.ipynb",
        ),
        (
            "LS",
            json!({"path": "C:/work/projects"}),
            "Listing projects",
        ),
        ("LS", json!({"path": "/home/user/repos/"}), "Listing repos"),
        // Patterns and queries.
        (
            "Grep",
            json!({"pattern": "HVAC.*filter"}),
            "Searching HVAC.*filter",
        ),
        ("Glob", json!({"pattern": "**/*.md"}), "Searching **/*.md"),
        ("Grep", json!({}), "Grep"),
        (
            "WebSearch",
            json!({"query": "when does daylight saving end"}),
            "Web search: when does daylight saving end",
        ),
        ("WebSearch", json!({"query": "   "}), "Web Search"),
        // A fetched page, as far as a line has room.
        (
            "WebFetch",
            json!({"url": "https://example.com/docs/install?x=1"}),
            "Reading example.com",
        ),
        // A path can carry a secret: only the host is ever shown.
        (
            "WebFetch",
            json!({"url": "https://api.telegram.org/bot123456:AAH-secret/getMe"}),
            "Reading api.telegram.org",
        ),
        (
            "WebFetch",
            json!({"url": "http://www.build.example.org:8080/logs"}),
            "Reading build.example.org",
        ),
        (
            "WebFetch",
            json!({"url": "example.com"}),
            "Reading example.com",
        ),
        ("WebFetch", json!({"url": ""}), "Web Fetch"),
        // MCP calls: never the raw dotted name.
        (
            "mcp__claude_ai_Gmail__search_threads",
            json!({"query": "winter bills"}),
            "Gmail · Search threads: winter bills",
        ),
        (
            "mcp__home_assistant__get_state",
            json!({"entity_id": "climate.bedroom"}),
            "Home Assistant · Get state",
        ),
        (
            "mcp__gmail__search_threads",
            json!({}),
            "Gmail · Search threads",
        ),
        ("mcp__helm__app_list_chats", json!({}), "Helm · List chats"),
        ("mcp__weird", json!({}), "Weird"),
        // Unknown tools: the name, spoken plainly.
        ("AskUserQuestion", json!({}), "Ask User Question"),
        ("TodoWrite", json!({"todos": []}), "Updating the plan"),
        ("", json!({"command": "x"}), ""),
    ];
    for (name, args, want) in cases {
        assert_eq!(tool_summary(name, args), *want, "name={name} args={args}");
    }
}

#[test]
fn a_summary_never_outgrows_its_line() {
    let long_word = "word ".repeat(40);
    let s = tool_summary("Bash", &json!({"description": long_word}));
    assert!(s.chars().count() <= 80, "{s}");
    assert!(s.ends_with('…'), "{s}");
    let s = tool_summary("Grep", &json!({"pattern": "x".repeat(300)}));
    assert!(s.chars().count() <= 80, "{s}");
    assert!(s.starts_with("Searching "), "{s}");
}
