"""Names and fixed values shared by the Helm Speaker integration."""

DOMAIN = "helm_speaker"

CONF_URL = "url"
CONF_TOKEN = "token"
CONF_ROOM = "room"
CONF_HINT = "hint"
CONF_LED = "led_entity"
CONF_PLAIN_HTTP = "allow_plain_http"

# Optional read-only chores/plants feed for the spoken check-in. The token is
# a read-only one: it can ask for the lists and nothing else, and like every
# other secret here it is never echoed back to the UI nor written to a log.
CONF_HOUSE_URL = "house_api_url"
CONF_HOUSE_TOKEN = "house_api_token"

# The model lane: the speaker's own agent talks straight to an OpenAI-style
# chat endpoint; hub settings stay optional (escalation and record only).
CONF_MODEL_URL = "model_url"
CONF_MODEL_NAME = "model_name"
CONF_MODEL_KEY = "model_api_key"
CONF_THINKING = "thinking"
CONF_EXTRA_HEADERS = "extra_headers_json"
CONF_EXTRA_BODY = "extra_body_json"
CONF_TIMEOUT_S = "timeout_s"

# Two named model profiles (the benchmark's A/B arms) plus the active one.
PROFILES = ("path_a", "path_b")
CONF_PROFILE = "profile"

# House tools describe what they would do and change nothing while this
# option is on. Off by default: the speaker is meant to work the lights.
CONF_DRY = "control_dry"

MAX_JSON_FIELD = 4096  # guard on the free-text JSON fields in the UI

# The unlock indicator: how often to ask the hub whether the session is
# raised, and the colour it shows while it is.
LED_POLL_SECONDS = 30
LED_RGB = (0, 220, 220)
