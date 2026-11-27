"""Helm Speaker: a Home Assistant voice pipeline with its own small brain.

One config entry carries a model endpoint (the conversation agent calls it
directly) and optionally a Helm hub (escalation of personal or long things,
plus best-effort records). This file wires the pieces together: an aiohttp
transport shared by model and hub, the active model profile built from
entry data + options, per-device turn history, and a service to switch
profiles without a restart. The hub parts still live in ``hubapi.py``; the
thinking lives in ``agent_core.py``.
"""

import asyncio
import logging
from urllib.parse import urlparse

import aiohttp
import voluptuous as vol
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import Context, HomeAssistant, ServiceCall
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.event import async_track_time_interval

from .const import (
    CONF_LED,
    CONF_TOKEN,
    CONF_URL,
    DOMAIN,
    LED_POLL_SECONDS,
    LED_RGB,
    PROFILES,
)
from . import errorlines
from .errors import RequestTimeout, TransportError
from .hubapi import HubApi, session_elevated
from .profiles import build_model_config, profiles_from_entry

_LOGGER = logging.getLogger(__name__)

# the stt entity exists only to send captures through the hub; a
# model-only entry does not want it (it would never answer anyway)
PLATFORMS = ["conversation"]
HUB_PLATFORMS = ["stt"]

# Only one entry per hub install is allowed (the config flow enforces it),
# so the service needs no entry selector.
SET_PROFILE_SCHEMA = vol.Schema({
    vol.Required("profile"): vol.In(list(PROFILES)),
})


class AiohttpTransport:
    """The aiohttp-backed implementation of the small transport interface
    ``hubapi`` and ``agent_core`` expect: ``request(method, url, headers,
    data, timeout)`` returns ``(status, body_bytes)`` or raises
    ``TransportError``."""

    def __init__(self, session):
        self.session = session

    async def request(self, method, url, *, headers=None, data=None, timeout=None):
        try:
            async with self.session.request(
                method, url, headers=headers, data=data,
                timeout=aiohttp.ClientTimeout(total=timeout),
            ) as resp:
                return resp.status, await resp.read()
        except asyncio.TimeoutError as err:
            # a slow endpoint is a timeout, not an offline box: the caller
            # speaks "taking longer than expected" instead of "can't reach it"
            raise RequestTimeout(str(err)) from err
        except aiohttp.ClientError as err:
            raise TransportError(str(err)) from err


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Set up from the config UI. No YAML for this integration."""
    session = async_get_clientsession(hass)
    transport = AiohttpTransport(session)

    hub_url = (entry.data.get(CONF_URL) or "").strip()
    token = entry.data.get(CONF_TOKEN) or ""
    api = HubApi(hub_url, token, transport) if hub_url and token else None

    devices = dr.async_get(hass)
    devices.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, entry.entry_id)},
        name="Helm speaker bridge",
        model="Helm hub connection",
        manufacturer="Helm",
    )

    led = LedWatch(hass, api, entry.data.get(CONF_LED))
    entry.runtime_data = {
        "api": api,
        "session": session,
        "transport": transport,
        "model_cfg": build_model_config(entry.data, entry.options),
        "platforms": PLATFORMS + (HUB_PLATFORMS if api else []),
        "history": {},  # device id -> recent spoken exchanges, per turn ctx
        "led": led,
    }

    hass.services.async_register(
        DOMAIN, "set_profile", _make_set_profile_service(hass, entry),
        schema=SET_PROFILE_SCHEMA)

    # an options change from the UI (profile switch, dry-run toggle) applies to
    # the running entry too; no reload needed for model or dry settings
    entry.async_on_unload(entry.add_update_listener(_async_options_updated))

    # read the pre-recorded-line catalogue once here, off the event loop: it
    # is cached for the module's lifetime, so a spoken turn never touches disk
    await hass.async_add_executor_job(errorlines.lines)

    platforms = PLATFORMS + (HUB_PLATFORMS if api else [])
    await hass.config_entries.async_forward_entry_setups(entry, platforms)
    led.start()

    # Optional benchmark hooks: stages.py (timings joiner and replay service)
    # starts here when the module is present; missing it must not break setup.
    try:
        from . import stages  # noqa: PLC0415 - optional companion module
    except ImportError:
        pass
    else:
        if hasattr(stages, "start"):
            stages.start(hass, entry)

    # benchmark replay door (helm_speaker.replay); same optional-module rule
    try:
        from . import replay  # noqa: PLC0415
    except ImportError:
        pass
    else:
        if hasattr(replay, "register_service"):
            replay.register_service(hass, entry)

    _LOGGER.info("helm_speaker set up (model=%s, hub=%s)",
                 _model_view(entry.runtime_data["model_cfg"]),
                 "yes" if api else "no")
    return True


def _model_view(cfg):
    """Log view of a model profile (stored dict or built Config): the model
    name and where it lives. Never the key or other header values."""
    get = cfg.get if isinstance(cfg, dict) else (
        lambda key, default=None: getattr(cfg, key, default))
    host = urlparse(get("model_url") or "").netloc or "no url"
    return "%s at %s" % (get("model_name") or "?", host)


def _apply_model_options(entry):
    """Rebuild the running model config from current data + options. The
    conversation agent reads model_cfg per turn, so this takes effect live."""
    entry.runtime_data["model_cfg"] = build_model_config(
        entry.data, entry.options or {})


async def _async_options_updated(hass: HomeAssistant, entry: ConfigEntry) -> None:
    _apply_model_options(entry)


def _make_set_profile_service(hass: HomeAssistant, entry: ConfigEntry):
    async def handle(call: ServiceCall) -> None:
        profile = call.data["profile"]
        options = dict(entry.options or {})
        options["profile"] = profile
        # keep the stored profiles next to the switch so a reload agrees
        options.setdefault("profiles", profiles_from_entry(entry.data, options))
        hass.config_entries.async_update_entry(entry, options=options)
        # apply it now too, rather than waiting for the update listener's
        # callback to be scheduled: a follow-up turn must see the new path
        _apply_model_options(entry)
        _LOGGER.info("helm_speaker switched to profile %s (%s)",
                     profile, _model_view(entry.runtime_data["model_cfg"]))
    return handle


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    unload_ok = await hass.config_entries.async_unload_platforms(
        entry, entry.runtime_data.get("platforms", PLATFORMS))
    if unload_ok:
        hass.services.async_remove(DOMAIN, "set_profile")
        try:
            hass.services.async_remove(DOMAIN, "replay")
        except Exception:  # noqa: BLE001 - the replay module may not be loaded
            pass
        data = entry.runtime_data
        data["led"].stop()
    return unload_ok


class LedWatch:
    """Best-effort unlock indicator.

    While the hub session is raised by a passcode, the chosen light entity
    glows in a fixed colour; when the unlock ends it is put back the way it
    was. If there is no hub at all (model-only entry) this simply idles. If
    the entity does not exist or refuses the call, nothing else is affected:
    the indicator is decoration, not function. A single poll interval keeps
    this simple; a status call every half minute is cheap.
    """

    def __init__(self, hass, api, entity_id):
        self.hass = hass
        self.api = api
        self.entity_id = entity_id
        self._unsub = None
        self._elevated = False
        self._saved = None  # attributes to restore, captured once

    def start(self):
        if not self.entity_id or self.api is None:
            return
        self._unsub = async_track_time_interval(
            self.hass, self._tick, LED_POLL_SECONDS)

    def stop(self):
        if self._unsub is not None:
            self._unsub()
            self._unsub = None

    async def _tick(self, _now):
        result = await self.api.status()
        elevated = result.get("kind") == "ok" and session_elevated(
            result.get("session"))
        if elevated and not self._elevated:
            await self._indicate()
        elif not elevated and self._elevated:
            await self._restore()
        self._elevated = elevated

    async def _indicate(self):
        state = self.hass.states.get(self.entity_id)
        if state is None:
            return
        if self._saved is None:
            attrs = state.attributes
            self._saved = {
                "brightness": attrs.get("brightness"),
                "rgb_color": attrs.get("rgb_color"),
                "was_off": state.state == "off",
            }
        try:
            await self.hass.services.async_call(
                "light", "turn_on",
                {"entity_id": self.entity_id, "rgb_color": list(LED_RGB)},
                blocking=True, context=Context(),
            )
        except Exception:  # noqa: BLE001 - decoration, never fatal
            _LOGGER.debug("Could not colour %s", self.entity_id)

    async def _restore(self):
        if self._saved is None:
            return
        saved, self._saved = self._saved, None
        try:
            if saved.get("was_off"):
                await self.hass.services.async_call(
                    "light", "turn_off", {"entity_id": self.entity_id},
                    blocking=True, context=Context())
            else:
                data = {"entity_id": self.entity_id}
                if saved.get("brightness") is not None:
                    data["brightness"] = saved["brightness"]
                if saved.get("rgb_color") is not None:
                    data["rgb_color"] = saved["rgb_color"]
                await self.hass.services.async_call(
                    "light", "turn_on", data,
                    blocking=True, context=Context())
        except Exception:  # noqa: BLE001 - decoration, never fatal
            _LOGGER.debug("Could not restore %s", self.entity_id)
