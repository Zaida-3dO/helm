//! Registers the MCP admin tools with the host-tool registry. The bodies are in
//! [`crate::admin`]; every refusal there happens before anything is written, so it maps to
//! "nothing changed".

use crate::admin;
use crate::config::McpStore;
use helm_hosttools::{HostToolRegistry, ToolError, ToolSpec};

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
            admin::call(&store, name, args).map_err(|e| ToolError::refused(e.0))
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
}
