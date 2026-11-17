"""Setup UI: one entry per hub, checked against the hub before saving."""

import voluptuous as vol
from homeassistant.config_entries import ConfigFlow, ConfigFlowResult
from homeassistant.helpers.aiohttp_client import async_create_clientsession

from . import AiohttpTransport
from .const import CONF_HINT, CONF_LED, CONF_ROOM, CONF_TOKEN, CONF_URL, DOMAIN
from .hubapi import KIND_BAD_AUTH, KIND_OK, HubApi

STEP_SCHEMA = vol.Schema(
    {
        vol.Required(CONF_URL): str,
        vol.Required(CONF_TOKEN): str,
        vol.Optional(CONF_ROOM): str,
        vol.Optional(CONF_HINT): str,
        vol.Optional(CONF_LED): str,
    }
)


class HelmSpeakerFlowHandler(ConfigFlow, domain=DOMAIN):
    """The single ``user`` step. The entry is unique per hub address: two
    entries against one hub would double-answer every turn."""

    VERSION = 1

    async def async_step_user(self, user_input=None) -> ConfigFlowResult:
        if self._async_current_entries():
            return self.async_abort(reason="already_configured")
        if user_input is None:
            return self.async_show_form(step_id="user", data_schema=STEP_SCHEMA)

        url = user_input[CONF_URL].strip().rstrip("/")
        await self.async_set_unique_id(url.lower())
        self._abort_if_unique_id_configured()

        session = async_create_clientsession(self.hass)
        api = HubApi(url, user_input[CONF_TOKEN], AiohttpTransport(session))
        result = await api.status()
        if result.get("kind") == KIND_OK:
            title = user_input.get(CONF_ROOM) or url
            return self.async_create_entry(
                title=title,
                data={
                    CONF_URL: url,
                    CONF_TOKEN: user_input[CONF_TOKEN],
                    CONF_ROOM: user_input.get(CONF_ROOM) or None,
                    CONF_HINT: user_input.get(CONF_HINT) or None,
                    CONF_LED: user_input.get(CONF_LED) or None,
                },
            )

        reason = "bad_auth" if result.get("kind") == KIND_BAD_AUTH else "bad_response"
        return self.async_show_form(
            step_id="user", data_schema=STEP_SCHEMA, errors={"base": reason})
