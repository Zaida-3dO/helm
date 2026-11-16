"""Helm Speaker: lets a Home Assistant voice pipeline talk to a Helm hub.

One config entry describes one hub. The entry carries the hub address, the
speaker's token and a few optional labels. Everything that actually talks to
the hub lives in ``hubapi.py``; this file only wires it to Home Assistant:
an aiohttp transport, a device entry for the bridge, and — if the user named
one — a light entity that is coloured while the hub session is unlocked and
put back the way it was when the unlock ends.
"""

import asyncio
import logging

import aiohttp
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers.event import async_track_time_interval

from .const import CONF_LED, DOMAIN, LED_POLL_SECONDS, LED_RGB
from .hubapi import HubApi, TransportError, session_elevated

_LOGGER = logging.getLogger(__name__)

PLATFORMS = ["conversation", "stt"]


class AiohttpTransport:
    """The aiohttp-backed implementation of the small transport interface
    ``hubapi`` expects: ``request(method, url, headers, data, timeout)``
    returns ``(status, body_bytes)`` or raises ``TransportError``."""

    def __init__(self, session):
        self.session = session

    async def request(self, method, url, *, headers=None, data=None, timeout=None):
        try:
            async with self.session.request(
                method, url, headers=headers, data=data,
                timeout=aiohttp.ClientTimeout(total=timeout),
            ) as resp:
                return resp.status, await resp.read()
        except (aiohttp.ClientError, asyncio.TimeoutError) as err:
            raise TransportError(str(err)) from err


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Set up from the config UI. No YAML for this integration."""
    session = aiohttp.ClientSession()
    api = HubApi(entry.data["url"], entry.data["token"], AiohttpTransport(session))

    devices = dr.async_get(hass)
    devices.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, entry.entry_id)},
        name="Helm speaker bridge",
        model="Helm hub connection",
        manufacturer="Helm",
    )

    led = LedWatch(hass, api, entry.data.get(CONF_LED))
    entry.runtime_data = {"api": api, "session": session, "led": led}

    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    led.start()
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    unload_ok = await hass.config_entries.async_unload_platforms(entry, PLATFORMS)
    if unload_ok:
        data = entry.runtime_data
        data["led"].stop()
        await data["session"].close()
    return unload_ok


class LedWatch:
    """Best-effort unlock indicator.

    While the hub session is raised by a passcode, the chosen light entity
    glows in a fixed colour; when the unlock ends it is put back the way it
    was. If the entity does not exist or refuses the call, nothing else is
    affected: the indicator is decoration, not function. A single poll
    interval keeps this simple; a status call every half minute is cheap.
    """

    def __init__(self, hass, api, entity_id):
        self.hass = hass
        self.api = api
        self.entity_id = entity_id
        self._unsub = None
        self._elevated = False
        self._saved = None  # attributes to restore, captured once

    def start(self):
        if not self.entity_id:
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
                blocking=True, context=self.hass.contexts.create_context(DOMAIN),
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
                    blocking=True, context=self.hass.contexts.create_context(DOMAIN))
            else:
                data = {"entity_id": self.entity_id}
                if saved.get("brightness") is not None:
                    data["brightness"] = saved["brightness"]
                if saved.get("rgb_color") is not None:
                    data["rgb_color"] = saved["rgb_color"]
                await self.hass.services.async_call(
                    "light", "turn_on", data,
                    blocking=True, context=self.hass.contexts.create_context(DOMAIN))
        except Exception:  # noqa: BLE001 - decoration, never fatal
            _LOGGER.debug("Could not restore %s", self.entity_id)
