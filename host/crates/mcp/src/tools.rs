//! Registers the MCP admin tools with the host-tool registry. The bodies are in
//! [`crate::admin`]; every refusal there happens before anything is written, so it maps to
//! "nothing changed".

use crate::admin;
use crate::config::McpStore;
use crate::unencrypted;
use helm_hosttools::{HostToolRegistry, ToolError, ToolSpec};
use serde_json::json;

pub fn register_host_tools(r: &mut HostToolRegistry) {
    for def in admin::tool_defs() {
        let name = def.name;
        let spec = if def.mutating {
            ToolSpec::write(name, def.description, def.input_schema)
        } else {
            ToolSpec::read(name, def.description, def.input_schema)
        };
        let spec = if def.large_request {
            spec.large()
        } else {
            spec
        };
        r.blocking(spec, move |ctx, args| {
            let store = ctx
                .services
                .get::<McpStore>()
                .ok_or_else(|| ToolError::refused("MCP settings are not available."))?;
            // A call that turns the unencrypted opt-in on is recorded, so the user gets a
            // notice with Undo.
            let touched = matches!(name, admin::ADD | admin::EDIT)
                .then(|| {
                    args.pointer("/server/id")
                        .or_else(|| args.get("serverId"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                })
                .flatten();
            let before = touched.as_ref().and_then(|_| store.load().ok());
            let mut out = admin::call(&store, name, args).map_err(|e| ToolError::refused(e.0))?;
            if let (Some(id), Some(before), Ok(after)) = (touched, before, store.load()) {
                if let Some((b, a)) = unencrypted::change_for(&before, &after, &id) {
                    let recorded = helm_hosttools::settings::record_external_change(
                        ctx,
                        unencrypted::KEY,
                        b,
                        a,
                    );
                    out["unencrypted"] = match recorded {
                        Ok(_) => json!("Plain http is now allowed for this server. The user was shown a notice with Undo."),
                        Err(_) => json!("Plain http is now allowed for this server, but the notice with Undo could not be recorded. Tell the user."),
                    };
                }
            }
            Ok(out)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::store;
    use helm_hosttools::{CallOrigin, LaunchKeys, ToolCtx, ToolHandler};
    use helm_ipc::{NullSink, Services};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn the_five_admin_tools_register_and_run() {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let names: Vec<_> = r.names().collect();
        for n in [
            admin::LIST,
            admin::ADD,
            admin::EDIT,
            admin::UPDATE,
            admin::REMOVE,
        ] {
            assert!(names.contains(&n), "{n}");
        }
        assert_eq!(
            r.get(admin::ADD).unwrap().spec.request_limit.bytes(),
            32 * 1024
        );
        assert_eq!(
            r.get(admin::UPDATE).unwrap().spec.request_limit.bytes(),
            8 * 1024
        );
        assert!(!r.get(admin::LIST).unwrap().spec.mutating);
        assert!(r.get(admin::REMOVE).unwrap().spec.mutating);
        // Listed with every feature off: the admin tools are ALWAYS.
        let listed = r.for_launch(&LaunchKeys::default());
        assert_eq!(listed.len(), 5);

        let d = tempfile::tempdir().unwrap();
        let mut services = Services::new();
        services.insert(Arc::new(store(d.path())));
        let ctx = ToolCtx {
            caller_chat_id: "c".into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: Arc::new(services),
            events: Arc::new(NullSink),
            ops: None,
        };
        let ToolHandler::Blocking(list) = &r.get(admin::LIST).unwrap().handler else {
            panic!("blocking")
        };
        let out = list(&ctx, json!({})).unwrap();
        assert_eq!(out["servers"], json!([]));
        let ToolHandler::Blocking(add) = &r.get(admin::ADD).unwrap().handler else {
            panic!("blocking")
        };
        let err = add(&ctx, json!({"revision": "0".repeat(64), "server": {}})).unwrap_err();
        assert!(err.text().ends_with("Nothing changed."));
    }

    #[test]
    fn a_chat_opting_in_to_plain_http_is_recorded_for_undo_and_undo_restores() {
        use helm_core::db::{Database, MigrationRegistry};
        use helm_ipc::RecordingSink;

        let d = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("settings-changes", helm_hosttools::settings::init);
        let db = Arc::new(Database::open(&d.path().join("t.db"), &reg).unwrap());
        let st = Arc::new(store(d.path()));
        let sink = Arc::new(RecordingSink::default());
        let mut services = Services::new();
        services.insert(db.clone());
        services.insert(st.clone());
        let ctx = ToolCtx {
            caller_chat_id: "c".into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: Arc::new(services),
            events: sink.clone(),
            ops: None,
        };
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let run = |name: &str, args| {
            let ToolHandler::Blocking(f) = &r.get(name).unwrap().handler else {
                panic!("blocking")
            };
            f(&ctx, args)
        };
        let rev = || st.snapshot().unwrap().revision;
        let changes = || -> Vec<(String, Option<String>, Option<String>)> {
            db.snapshot(|tx| {
                let mut q =
                    tx.prepare("SELECT key, before, after FROM settings_changes ORDER BY id")?;
                let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
        };
        let ext = unencrypted::settings_extension(st.clone());
        let undo_last = || {
            let (_, before, _) = changes().pop().unwrap();
            let before: serde_json::Value = serde_json::from_str(&before.unwrap()).unwrap();
            db.write(|tx| Ok::<_, rusqlite::Error>(ext.restore(tx, unencrypted::KEY, &before)))
                .unwrap()
                .unwrap();
        };

        // Adding a private http server with the opt-in is recorded and announced.
        let out = run(
            admin::ADD,
            json!({"revision": rev(), "server": {"id": "nas", "name": "NAS",
            "enabled": true, "allowPrivateHttp": true,
            "transport": {"type": "http", "url": "http://192.168.1.5:8080/mcp"}}}),
        )
        .unwrap();
        assert!(out["unencrypted"]
            .as_str()
            .unwrap()
            .contains("notice with Undo"));
        assert_eq!(sink.kinds(), ["external_settings"]);
        assert_eq!(changes().len(), 1);
        assert_eq!(changes()[0].0, unencrypted::KEY);
        // A call that leaves the opt-in as it was records nothing.
        run(
            admin::EDIT,
            json!({"revision": rev(), "serverId": "nas", "name": "Renamed"}),
        )
        .unwrap();
        assert_eq!(changes().len(), 1);
        // Undo removes the server the change added.
        undo_last();
        assert!(st.load().unwrap().is_empty());

        // Editing an https server onto private http with the opt-in: Undo puts the address back.
        run(
            admin::ADD,
            json!({"revision": rev(), "server": {"id": "web", "name": "Web", "enabled": true,
            "transport": {"type": "http", "url": "https://example.com/mcp"}}}),
        )
        .unwrap();
        assert_eq!(changes().len(), 1, "an https server needs no notice");
        run(
            admin::EDIT,
            json!({"revision": rev(), "serverId": "web", "url": "http://10.0.0.8/mcp",
            "allowPrivateHttp": true}),
        )
        .unwrap();
        assert_eq!(changes().len(), 2);
        assert!(st.load().unwrap()[0].allow_private_http);
        undo_last();
        let s = &st.load().unwrap()[0];
        assert!(!s.allow_private_http);
        assert_eq!(s.transport.url.as_deref(), Some("https://example.com/mcp"));
        // The key is read only through app_update_settings.
        let e = db
            .write(|tx| Ok::<_, rusqlite::Error>(ext.apply(tx, unencrypted::KEY, &json!({}))))
            .unwrap();
        assert!(e.is_err());
    }
}
