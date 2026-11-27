"""Setup UI: one entry, model endpoint required, hub optional.

The conversation agent talks to the model endpoint directly, so that is what
setup needs; the hub fields are optional and only used for escalation and
records (a hub-less entry still sets up and answers home-control). The
free-text JSON boxes for extra headers/body keep per-profile oddities - like
a priority-lane header or a chat-template thinking switch - configurable
without a code change. An options flow edits the same fields, stores them
under the two named profiles (path_a/path_b) and switches which one is
active; no restart needed. The model key has its own field, is never echoed
back to the UI, and never reaches a log line (profiles.redact at every
touch point).
"""

import json

import voluptuous as vol
from homeassistant.config_entries import (
    ConfigEntry,
    ConfigFlow,
    ConfigFlowResult,
    OptionsFlow,
)
from homeassistant.helpers.aiohttp_client import async_create_clientsession

from . import AiohttpTransport
from .const import (
    CONF_DRY,
    CONF_EXTRA_BODY,
    CONF_EXTRA_HEADERS,
    CONF_HINT,
    CONF_HOUSE_TOKEN,
    CONF_HOUSE_URL,
    CONF_LED,
    CONF_MODEL_KEY,
    CONF_MODEL_NAME,
    CONF_MODEL_URL,
    CONF_PLAIN_HTTP,
    CONF_PROFILE,
    CONF_ROOM,
    CONF_THINKING,
    CONF_TIMEOUT_S,
    CONF_TOKEN,
    CONF_URL,
    DOMAIN,
    PROFILES,
)
from .hubapi import KIND_BAD_AUTH, HubApi
from .profiles import parse_json_field, profiles_from_entry


def _json_text(value):
    """Stored dict -> the text shown in a JSON box (blank when unset)."""
    if not value:
        return ""
    return json.dumps(value)


def _model_schema(defaults=None):
    defaults = defaults or {}
    return {
        vol.Required(CONF_MODEL_URL, default=defaults.get("model_url", "")): str,
        vol.Required(CONF_MODEL_NAME, default=defaults.get("model_name", "")): str,
        vol.Optional(CONF_MODEL_KEY, default=""): str,
        vol.Optional(CONF_THINKING, default=defaults.get("thinking", True)): bool,
        vol.Optional(CONF_EXTRA_HEADERS,
                     default=_json_text(defaults.get("extra_headers"))): str,
        vol.Optional(CONF_EXTRA_BODY,
                     default=_json_text(defaults.get("extra_body"))): str,
        vol.Optional(CONF_TIMEOUT_S, default=defaults.get("timeout_s", 30)):
            vol.Coerce(float),
    }


STEP_SCHEMA = vol.Schema({
    **_model_schema(),
    vol.Optional(CONF_URL): str,
    vol.Optional(CONF_TOKEN): str,
    vol.Optional(CONF_ROOM): str,
    vol.Optional(CONF_HINT): str,
    vol.Optional(CONF_LED): str,
    vol.Optional(CONF_HOUSE_URL): str,
    vol.Optional(CONF_HOUSE_TOKEN): str,
    vol.Optional(CONF_PLAIN_HTTP, default=False): bool,
})


class HelmSpeakerFlowHandler(ConfigFlow, domain=DOMAIN):
    """The single ``user`` step. One entry per install; uniqueness follows
    the hub address when one is given, otherwise the model endpoint."""

    VERSION = 1

    @staticmethod
    def async_get_options_flow(
        config_entry: ConfigEntry,
    ) -> "HelmSpeakerOptionsFlow":
        # the entry reaches the flow through its handler; it is not handed to
        # the constructor (that base class is on its way out)
        return HelmSpeakerOptionsFlow()

    async def async_step_user(self, user_input=None) -> ConfigFlowResult:
        if self._async_current_entries():
            return self.async_abort(reason="already_configured")
        if user_input is None:
            return self.async_show_form(step_id="user", data_schema=STEP_SCHEMA)

        headers, herr = parse_json_field(user_input.get(CONF_EXTRA_HEADERS))
        body, berr = parse_json_field(user_input.get(CONF_EXTRA_BODY))
        if herr or berr:
            return self.async_show_form(
                step_id="user", data_schema=STEP_SCHEMA,
                errors={"base": herr or berr})

        url = (user_input.get(CONF_URL) or "").strip().rstrip("/")
        model_url = user_input[CONF_MODEL_URL].strip().rstrip("/")
        if url.lower().startswith("http://") and not user_input.get(CONF_PLAIN_HTTP):
            # the speaker token would cross the network readable that way
            return self.async_show_form(
                step_id="user", data_schema=STEP_SCHEMA,
                errors={"base": "plain_http"})
        if model_url.startswith("http://") and not user_input.get(CONF_PLAIN_HTTP):
            # model turns carry the whole conversation: same explicit-allow
            # rule as the hub token (loopback or trusted-LAN http is fine,
            # but it has to be allowed on purpose)
            return self.async_show_form(
                step_id="user", data_schema=STEP_SCHEMA,
                errors={"base": "model_plain_http"})

        house_url = (user_input.get(CONF_HOUSE_URL) or "").strip().rstrip("/")
        house_token = (user_input.get(CONF_HOUSE_TOKEN) or "").strip()
        if (house_url.lower().startswith("http://") and house_token
                and not user_input.get(CONF_PLAIN_HTTP)):
            # the read-only feed token would travel readable too; without a
            # token the fetch is anonymous, so only the http+token pair needs
            # the explicit tick
            return self.async_show_form(
                step_id="user", data_schema=STEP_SCHEMA,
                errors={"base": "house_plain_http"})

        profile = {
            "model_url": model_url,
            "model_name": (user_input[CONF_MODEL_NAME] or "").strip(),
            "api_key": user_input.get(CONF_MODEL_KEY) or None,
            "thinking": bool(user_input.get(CONF_THINKING, True)),
            "extra_headers": headers or {},
            "extra_body": body or {},
            "timeout_s": float(user_input.get(CONF_TIMEOUT_S) or 30),
        }

        if url:
            # the hub is optional, but when it is named an outright refusal
            # of the token is a mistake worth catching at setup (unreachable
            # is accepted: escalation degrades to one sentence and the rest
            # of the speaker keeps working)
            session = async_create_clientsession(self.hass)
            api = HubApi(url, user_input.get(CONF_TOKEN) or "",
                         AiohttpTransport(session))
            result = await api.status()
            if result.get("kind") == KIND_BAD_AUTH:
                return self.async_show_form(
                    step_id="user", data_schema=STEP_SCHEMA,
                    errors={"base": "bad_auth"})

        identity = url.lower() if url else "model:" + model_url.lower()
        await self.async_set_unique_id(identity)
        self._abort_if_unique_id_configured()

        data = {
            CONF_MODEL_URL: profile["model_url"],
            CONF_MODEL_NAME: profile["model_name"],
            CONF_MODEL_KEY: profile["api_key"],
            CONF_THINKING: profile["thinking"],
            "headers": profile["extra_headers"],
            "body": profile["extra_body"],
            CONF_TIMEOUT_S: profile["timeout_s"],
            CONF_URL: url or None,
            CONF_TOKEN: user_input.get(CONF_TOKEN) or None,
            CONF_ROOM: user_input.get(CONF_ROOM) or None,
            CONF_HINT: user_input.get(CONF_HINT) or None,
            CONF_LED: user_input.get(CONF_LED) or None,
            CONF_HOUSE_URL: house_url or None,
            CONF_HOUSE_TOKEN: house_token or None,
        }
        options = {"profile": PROFILES[0],
                   "profiles": {PROFILES[0]: dict(profile),
                                PROFILES[1]: dict(profile)}}
        title = user_input.get(CONF_ROOM) or profile["model_name"]
        return self.async_create_entry(title=title, data=data, options=options)


class HelmSpeakerOptionsFlow(OptionsFlow):
    """Edit the model profiles and switch the active one without a restart.

    Two steps on purpose: first pick which profile to edit (the form cannot
    re-read a dropdown mid-step, so picking and editing in one form once
    wrote the wrong profile's values), then the fields arrive prefilled from
    *that* profile. The key field starts empty by design - the stored key is
    never echoed back, and leaving it blank keeps what is stored."""

    async def async_step_init(self, user_input=None) -> ConfigFlowResult:
        options = dict(self.config_entry.options or {})
        active = options.get(CONF_PROFILE) or PROFILES[0]
        schema = vol.Schema({
            vol.Required("edit_profile", default=active): vol.In(list(PROFILES)),
            vol.Optional("make_active", default="keep"):
                vol.In(["keep", *PROFILES]),
            vol.Optional(CONF_DRY, default=bool(options.get(CONF_DRY))): bool,
        })
        if user_input is None:
            return self.async_show_form(step_id="init", data_schema=schema)
        self._picked = user_input["edit_profile"]
        self._dry = bool(user_input.get(CONF_DRY))
        self._active = user_input.get("make_active") or "keep"
        return await self.async_step_model()

    async def async_step_model(self, user_input=None) -> ConfigFlowResult:
        picked = getattr(self, "_picked", PROFILES[0])
        profiles = profiles_from_entry(self.config_entry.data,
                                       self.config_entry.options)
        schema = vol.Schema(_model_schema(profiles[picked]))
        if user_input is None:
            return self.async_show_form(step_id="model", data_schema=schema)

        headers, herr = parse_json_field(user_input.get(CONF_EXTRA_HEADERS))
        body, berr = parse_json_field(user_input.get(CONF_EXTRA_BODY))
        if herr or berr:
            return self.async_show_form(
                step_id="model", data_schema=schema,
                errors={"base": herr or berr})

        updated = {
            "model_url": (user_input[CONF_MODEL_URL] or "").strip().rstrip("/"),
            "model_name": (user_input[CONF_MODEL_NAME] or "").strip(),
            "api_key": user_input.get(CONF_MODEL_KEY) or profiles[picked]["api_key"],
            "thinking": bool(user_input.get(CONF_THINKING, True)),
            "extra_headers": headers or {},
            "extra_body": body or {},
            "timeout_s": float(user_input.get(CONF_TIMEOUT_S) or 30),
        }
        options = dict(self.config_entry.options or {})
        stored = profiles_from_entry(self.config_entry.data, options)
        stored[picked] = updated          # the profile actually on screen
        options["profiles"] = stored
        options[CONF_DRY] = self._dry
        if getattr(self, "_active", "keep") != "keep":
            options[CONF_PROFILE] = self._active  # else active stays put
        return self.async_create_entry(title="", data=options)
