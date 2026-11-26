"""The HomeControl Backend that runs inside live Home Assistant.

Everything HA-shaped about the house tools lives here; homecontrol.py holds
the decisions and stays testable without HA. The expose decision is Home
Assistant's own (the same switch a user sets per entity for the voice
assistant), so hidden or unexposed entities are invisible to every path in
this component - they cannot be resolved, called, or even seen.
"""

import logging

from homeassistant.components.homeassistant.exposed_entities import (
    async_should_expose,
)
from homeassistant.core import HomeAssistant
from homeassistant.helpers import area_registry as ar
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers import intent

_LOGGER = logging.getLogger(__name__)

# The assistant whose expose switch we honour: the plain voice conversation.
ASSISTANT = "conversation"

_KINDS = {"light": "lights", "climate": "climates"}

# Logical keys from homecontrol -> slot names HA's built-in timer intents read.
_TIMER_SLOTS = {
    "seconds": "start_seconds",
    "minutes": "start_minutes",
    "label": "name",
}


class HaBackend:
    """One per conversation turn (it remembers the asking satellite so the
    built-in voice timers land on it)."""

    def __init__(self, hass: HomeAssistant, satellite_device_id=None):
        self.hass = hass
        self.satellite_device_id = satellite_device_id

    async def exposed(self):
        """{area: {"lights": [(name, entity_id)], "climates": [...]}} using
        only entities HA itself exposes to the voice assistant."""
        out = {}
        ent_reg = er.async_get(self.hass)
        dev_reg = dr.async_get(self.hass)
        area_reg = ar.async_get(self.hass)
        for domain, kind in _KINDS.items():
            for state in self.hass.states.async_all(domain):
                if not async_should_expose(self.hass, ASSISTANT, state.entity_id):
                    continue
                name = state.attributes.get("friendly_name") or state.entity_id
                entry = ent_reg.async_get(state.entity_id)
                area_name = "Unassigned"
                if entry is not None and entry.device_id:
                    device = dev_reg.async_get(entry.device_id)
                    if device is not None and device.area_id:
                        area = area_reg.async_get(device.area_id)
                        if area is not None:
                            area_name = area.name or area.id
                out.setdefault(area_name, {"lights": [], "climates": []})[
                    kind].append((name, state.entity_id))
        return out

    async def states(self, entity_ids):
        result = {}
        for entity_id in entity_ids:
            state = self.hass.states.get(entity_id)
            if state is not None:
                result[entity_id] = {"state": state.state,
                                     "attributes": dict(state.attributes)}
        return result

    async def call_service(self, domain, service, data):
        # blocking so one turn's tool plan runs in order; failures are the
        # caller's to see - HA raises ServiceCallError/EntityNotFound here.
        await self.hass.services.async_call(domain, service, dict(data),
                                            blocking=True)

    async def intent(self, name, data):
        """HA's built-in voice-timer intents, on the asking satellite."""
        slots = {}
        for key, value in (data or {}).items():
            slot_name = _TIMER_SLOTS.get(key, key)
            if value is None:
                continue
            slots[slot_name] = {"value": value}
        try:
            response = await intent.async_handle(
                self.hass, ASSISTANT, name, slots or None,
                language="en", device_id=self.satellite_device_id)
        except intent.IntentError as exc:  # includes MatchFailedError
            return {"ok": False, "error": str(exc)[:150]}
        except Exception:  # noqa: BLE001 - an intent blowing up must not kill a spoken turn
            _LOGGER.exception("intent %s failed", name)
            return {"ok": False, "error": "the timer handler failed"}
        speech = None
        if response is not None:
            plain = (getattr(response, "speech", None) or {}).get("plain") or {}
            speech = plain.get("speech")
        return {"ok": True, "text": speech} if speech else {"ok": True}
