//! Item-to-chat links: which Helm chats work on which task-board
//! item. They live **only in Helm** (table `item_chat_links`); nothing is written to Agent Standup
//! in v1 (write-back comes after v1), and Helm makes no board transitions. This crate does not even
//! depend on the board client.
//!
//! | Module | What |
//! |---|---|
//! | [`ids`] | item ids: a full UUID or an 8+ hex-character prefix, one canonical form |
//! | [`store`] | the table, linking, unlinking, main chat, open |
//! | [`types`] | shapes shared with the sidebar and the picker |
//! | `ops` | `item_links_for_chats`, `item_links_for_item`, `item_link_set_main`, `item_open` |
//! | [`tool`] | the `app_link_item` host tool and the `itemlinks` prompt section |
//! | [`bindings`] | generated TypeScript in `ui/src/api/generated/itemlinks/` |
//!
//! Event `item_links {itemIds}` tells the UI to re-read. A board item's title shows when the item is
//! a board project the mirror holds; any other item shows its first 8 characters (the mirror holds
//! projects only).

pub mod bindings;
pub mod ids;
mod ops;
pub mod store;
pub mod tool;
pub mod types;

pub use ops::register_ops;
pub use tool::{register_host_tools, ItemLinksSection};

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("item-chat-links", store::init);
}

#[cfg(test)]
mod tests {
    /// US8: nothing here can write to the board. The crate depends on no board client and no HTTP
    /// library, and no source file builds a request (needles are split so this file does not match).
    #[test]
    fn the_crate_has_no_way_to_write_to_the_board() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        for dep in [
            ["helm", "-standup"].concat(),
            "reqwest".to_owned(),
            "ureq".to_owned(),
            "hyper".to_owned(),
        ] {
            assert!(!manifest.contains(&dep), "Cargo.toml mentions {dep}");
        }
        let needles = [
            ["helm_", "standup"].concat(),
            ["Trans", "port"].concat(),
            ["http", "://"].concat(),
            ["https", "://"].concat(),
            [".po", "st("].concat(),
            [".pa", "tch("].concat(),
            ["transition", "_item"].concat(),
        ];
        for e in std::fs::read_dir(root.join("src")).unwrap().flatten() {
            if e.file_name() == "lib.rs" {
                continue;
            }
            let text = std::fs::read_to_string(e.path()).unwrap();
            // Doc and comment lines may talk about the board; code may not touch it.
            let code: String = text
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for n in &needles {
                assert!(
                    !code.contains(n.as_str()),
                    "{} mentions {n}",
                    e.file_name().to_string_lossy()
                );
            }
        }
    }
}
