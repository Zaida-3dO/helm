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

# Logical keys from homecontrol -> slot names HA's built-in timer intents
# read. HassStartTimer takes hours/minutes/seconds/name; start_* are the
# cancel/status matchers, not fill-ins for a new timer.
_TIMER_SLOTS = {"label": "name"}


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

        def area_of(entry, device):
            # an entity can carry its own area; failing that the device's
            if entry is not None and entry.area_id:
                return entry.area_id
            if device is not None and device.area_id:
                return device.area_id
            return None

        for domain, kind in _KINDS.items():
            for state in self.hass.states.async_all(domain):
                entry = ent_reg.async_get(state.entity_id)
                if entry is not None:
                    # hidden or disabled entities do not exist for voice, and
                    # a config/diagnostic entity_category never belongs to it -
                    # even when an old expose switch happens to be on
                    if entry.hidden_by or entry.disabled_by:
                        continue
                    if getattr(entry, "entity_category", None):
                        continue
                    device = (dev_reg.async_get(entry.device_id)
                              if entry.device_id else None)
                else:
                    device = None
                if not async_should_expose(self.hass, ASSISTANT, state.entity_id):
                    continue
                name = state.attributes.get("friendly_name") or state.entity_id
                area_name = "Unassigned"
                area_id = area_of(entry, device)
                if area_id:
                    area = area_reg.async_get_area(area_id)
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
        text = None
        if response is not None:
            plain = (getattr(response, "speech", None) or {}).get("plain") or {}
            text = plain.get("speech")
            slots = getattr(response, "speech_slots", None)
            if isinstance(slots, dict) and "timers" in slots:
                # the status intent puts the live timers in speech slots rather
                # than a sentence; without reading them we would say nothing
                spoken = _timer_speech(slots.get("timers"))
                if spoken:
                    text = spoken
        return {"ok": True, "text": text} if text else {"ok": True}


def _unwrap(value):
    """Slot values arrive plain or wrapped as {"value": ...}; accept both."""
    if isinstance(value, dict) and "value" in value:
        return value["value"]
    return value


def _timer_speech(timers):
    """A spoken line for the timer status slots. Unknown shapes stay quiet -
    invented words are worse than the plain sentence the intent gave."""
    timers = _unwrap(timers)
    if not isinstance(timers, (list, tuple)):
        return None
    if not timers:
        return "No timers are running."
    bits = []
    for timer in timers[:4]:
        if not isinstance(timer, dict):
            continue
        label = str(timer.get("name") or "").strip()
        secs = None
        for key in ("seconds", "remaining", "remaining_seconds"):
            candidate = _unwrap(timer.get(key))
            if isinstance(candidate, (int, float)):
                secs = int(candidate)
                break
        bit = label or "A timer"
        if secs is not None:
            if secs >= 3600:
                hours, rem = divmod(secs, 3600)
                bit += ": %d h %d min left" % (hours, round(rem / 60))
            elif secs >= 90:
                bit += ": %d min left" % round(secs / 60)
            else:
                bit += ": %d s left" % secs
        if _unwrap(timer.get("status")) == "paused":
            bit += ", paused"
        bits.append(bit)
    if not bits:
        return None
    tail = " and %d more." % (len(timers) - 4) if len(timers) > 4 else "."
    return "; ".join(bits) + tail
