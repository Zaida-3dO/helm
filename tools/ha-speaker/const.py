"""Names and fixed values shared by the Helm Speaker integration."""

DOMAIN = "helm_speaker"

CONF_URL = "url"
CONF_TOKEN = "token"
CONF_ROOM = "room"
CONF_HINT = "hint"
CONF_LED = "led_entity"
CONF_PLAIN_HTTP = "allow_plain_http"

# The unlock indicator: how often to ask the hub whether the session is
# raised, and the colour it shows while it is.
LED_POLL_SECONDS = 30
LED_RGB = (0, 220, 220)
