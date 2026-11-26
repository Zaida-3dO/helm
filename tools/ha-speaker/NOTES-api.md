# NOTES-api.md — HA API names used by this component (H0 working note; delete when H3 is done)

HA on the live box runs 2026.11.2 (`/config/.HA_VERSION`). The docs site redirects
`developers.home-assistant.io/docs/core/platform/conversation/` to `contributors.home-assistant.io/`
(no versioned reference found), so every name below is checked against the real
`homeassistant==2026.11.2` package installed in a scratch venv (`helm/.scratch-havenv`), not against docs.

Checked with `import` probes against that package (see H3 report for the exact probe list):

| Name used | Where | Status |
|---|---|---|
| `components.conversation.ConversationEntity` / `ConversationInput` / `ConversationResult` | conversation.py | pending venv check |
| `components.stt.SpeechToTextEntity` / `SpeechMetadata` / `AudioFormats` / `AudioSources` / `MediaCodecs` | stt.py | pending venv check |
| `helpers.intent.IntentResponse.async_set_speech` | conversation.py | pending venv check |
| `helpers.intent.async_handle` (timer voice intents) | planned, ha_backend.py | pending venv check |
| `helpers.config_area_registry.async_should_expose` / `async_config_entry_first_refresh`… see H3 | planned, ha_backend.py | pending venv check |
| `entry.runtime_data` | conversation.py, __init__.py | pending venv check (2025.2+ pattern; live copy already uses it) |

Uncertain before the venv lands: none beyond normal first-load risk — the installed copy has never been
instantiated on this HA version (audit §4).
