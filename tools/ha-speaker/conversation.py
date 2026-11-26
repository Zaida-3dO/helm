"""The conversation agent: a small model answers here, Helm takes the rest.

One turn arrives -> build the minimised context (time, asking room, exposed
light and climate names, short history) -> agent_core runs the lean prompt
against the configured chat endpoint with the four house tools -> speak the
answer. Anything personal or long comes back as kind ``escalate`` and is put
to the hub over its speaker route; the hub's short answer is what gets
spoken then. If the hub is not configured, escalation falls back to one of
our fixed sentences. After the words are ready the exchange is recorded at
the hub as a background task - recording never delays speech, and a hub
without the record route quietly does nothing.

The entity keeps its name and id (conversation.helm_speaker): it lives in
the voice pipeline like any other agent, and Home Assistant's local intents
still answer light and timer sentences first when the pipeline says so.
"""

import asyncio
import logging
import time
import uuid

from homeassistant.components.conversation import (
    ConversationEntity,
    ConversationInput,
    ConversationResult,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers import area_registry as ar
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers import intent
from homeassistant.helpers.device_registry import DeviceInfo

from . import agent_core
from .const import CONF_HINT, CONF_ROOM, DOMAIN
from .ha_backend import HaBackend
from .homecontrol import HomeControl
from .hubapi import (
    ERROR_TEXT,
    KIND_ANSWER,
    OFFLINE_TEXT,
    TIMEOUT_TEXT,
    HubApi,
    build_presence,
)

_LOGGER = logging.getLogger(__name__)

# Benchmark-only convenience: a helper text entity may carry tags like
# "arm=A run=3"; they ride into every timing/record line unchanged.
TAGS_ENTITY = "input_text.helm_speaker_tags"


async def async_setup_entry(
    hass: HomeAssistant,
    config_entry: ConfigEntry,
    async_add_entities,
) -> None:
    async_add_entities([HelmSpeakerAgent(hass, config_entry)])


class HelmSpeakerAgent(ConversationEntity):
    """One agent entity per entry (a hub entry may be optional; the model
    endpoint is what this entity actually needs)."""

    _attr_has_entity_name = False
    _attr_name = "Helm speaker"

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry):
        self.hass = hass
        self.entry = entry
        self._attr_unique_id = f"{entry.entry_id}-agent"
        self._attr_device_info = DeviceInfo(identifiers={(DOMAIN, entry.entry_id)})

    @property
    def supported_languages(self):
        # The model answers in whatever language it was asked in.
        return "*"

    async def async_process(self, user_input: ConversationInput) -> ConversationResult:
        started = time.monotonic()
        runtime = self.entry.runtime_data
        cfg = runtime["model_cfg"]
        text = (user_input.text or "").strip()
        turn_id = uuid.uuid4().hex
        device_id = getattr(user_input, "device_id", None)
        key = device_id or self.entry.entry_id

        backend = HaBackend(self.hass, satellite_device_id=device_id)
        control = HomeControl(backend, dry=bool(runtime.get("control_dry")))
        ctx = await self._ctx(backend, device_id, runtime["history"].setdefault(key, []))
        result = await agent_core.run_turn(
            cfg, runtime["transport"], control, ctx, text,
            tags=self._tags(), clock=time.time)

        kind = result.kind
        spoken = result.text
        record_kind = "answer"
        if kind == agent_core.KIND_ESCALATE:
            record_kind = "escalated"
            api = runtime.get("api")
            if api is not None:
                hub = await self._escalate(api, device_id,
                                           result.escalate_text or text, ctx)
                spoken = hub["text"]
            else:
                spoken = ERROR_TEXT
        elif kind == agent_core.KIND_TIMEOUT:
            record_kind = "error"
        elif kind in (agent_core.KIND_OFFLINE, agent_core.KIND_ERROR):
            record_kind = "error"

        history = runtime["history"].setdefault(key, [])
        history.append({"user": text, "spoken": spoken, "ts": time.time()})
        del history[:-8]  # agent_core filters by age and count anyway

        _LOGGER.info(
            "helm_speaker timing: turn %s answered in %.2f s (%s, model=%s)",
            turn_id[:8], time.monotonic() - started, kind, result.model_used)

        try:
            from . import stages  # noqa: PLC0415 - optional benchmark hook
        except ImportError:
            pass
        else:
            if hasattr(stages, "note_turn"):
                stages.note_turn(self.hass, self.entry, device_id=device_id,
                                 turn_id=turn_id, text_in=text,
                                 text_out=spoken, kind=kind, result=result)

        api = runtime.get("api")
        if api is not None and getattr(api, "record", None) is not None \
                and cfg.record_text:
            asyncio.create_task(self._record(
                api, turn_id, device_id, text, spoken, record_kind, result))

        response = intent.IntentResponse(language=user_input.language)
        response.async_set_speech(spoken)
        return ConversationResult(
            response=response,
            conversation_id=user_input.conversation_id,
            continue_conversation=agent_core.ends_with_question(spoken),
        )

    async def _ctx(self, backend, device_id, history):
        """The minimised context agent_core expects: room label, exposed
        names per area (names only - no entity ids, states or attributes)
        and the recent spoken exchanges."""
        room = self.entry.data.get(CONF_ROOM)
        if not room and device_id:
            device = dr.async_get(self.hass).async_get(device_id)
            if device is not None and device.area_id:
                room = ar.async_get(self.hass).async_get_area_name(device.area_id)
        exposed = await backend.exposed()
        areas = {}
        for area, entry in exposed.items():
            areas[area] = {
                "lights": [name for name, _ in entry.get("lights") or []],
                "climates": [name for name, _ in entry.get("climates") or []],
            }
        return {"room": room, "areas": areas, "history": list(history)}

    def _tags(self):
        state = self.hass.states.get(TAGS_ENTITY)
        if state is None or not state.state or state.state in ("unavailable", "unknown"):
            return None
        tags = {}
        for piece in state.state.replace(",", " ").split():
            if "=" in piece:
                name, value = piece.split("=", 1)
                tags[name] = value
        return tags or None

    async def _escalate(self, api: HubApi, device_id, ask_text, ctx=None):
        """Send the personal/long request to the hub; speak its short answer.

        The turn carries origin=escalation plus what the little brain knew
        locally (room facts and the last few exchanges); hubapi clips it."""
        area_name, presence = self._room_facts(device_id)
        recent = [{"ask": h.get("user"), "said": h.get("spoken")}
                  for h in (ctx or {}).get("history", [])[-4:]]
        context = {"areas": (ctx or {}).get("areas"), "recent": recent}
        context = {k: v for k, v in context.items() if v} or None
        result = await api.run_turn(
            text=ask_text, turn_id=uuid.uuid4().hex,
            device=device_id or self.entry.entry_id,
            room=self.entry.data.get(CONF_ROOM), area=area_name,
            hint=self.entry.data.get(CONF_HINT), presence=presence,
            context=context, escalation=True)
        kind = result.get("kind")
        if kind == KIND_ANSWER:
            return {"text": result["text"]}
        if kind == "offline":
            return {"text": OFFLINE_TEXT}
        if kind == "timeout":
            return {"text": TIMEOUT_TEXT}
        _LOGGER.debug("escalation ended as %s", kind)
        return {"text": ERROR_TEXT}

    async def _record(self, api, turn_id, device_id, asked, spoken, kind, result):
        """Best effort: the hub route may not exist yet; never raise out of
        a background task (H5's record already swallows its own failures)."""
        try:
            area_name, _presence = self._room_facts(device_id)
            await api.record(
                turn_id=turn_id, device=device_id or self.entry.entry_id,
                room=self.entry.data.get(CONF_ROOM), area=area_name,
                text=asked, answer=spoken, kind=kind,
                tool_calls=result.tool_calls, timings=result.timings,
                model=result.model_used, tags=result.tags)
        except Exception:  # noqa: BLE001 - recording is decoration, speech came first
            _LOGGER.debug("could not record turn %s", turn_id[:8])

    def _room_facts(self, device_id):
        """The area the asking satellite sits in, and a plain snapshot of the
        room for the hub's registry. All optional: nothing fails if the
        satellite has no area."""
        area_name = None
        area_id = None
        if device_id:
            device = dr.async_get(self.hass).async_get(device_id)
            if device is not None and device.area_id:
                area_id = device.area_id
                area_name = ar.async_get(self.hass).async_get_area_name(area_id)
        people = None
        occupied = None
        if area_id:
            people = self._people_in_area(area_id)
            occupied = self._area_occupied(area_id)
        return area_name, build_presence(area=area_name, people=people,
                                         occupied=occupied)

    def _people_in_area(self, area_id):
        names = []
        devices = dr.async_get(self.hass)
        for state in self.hass.states.async_all("person"):
            for device_id in state.attributes.get("device_ids", []) or []:
                device = devices.async_get(device_id)
                if device is not None and device.area_id == area_id:
                    names.append(state.name)
                    break
        return names

    def _area_occupied(self, area_id):
        """True if any occupancy sensor in the area is on, False if they all
        read clear, None if the area has none (unknown, not "empty")."""
        entities = er.async_get(self.hass)
        devices = dr.async_get(self.hass)
        seen = False
        for state in self.hass.states.async_all("binary_sensor"):
            if state.attributes.get("device_class") != "occupancy":
                continue
            entry = entities.async_get(state.entity_id)
            device = devices.async_get(entry.device_id) if entry and entry.device_id else None
            if device is None or device.area_id != area_id:
                continue
            seen = True
            if state.state == "on":
                return True
        return False if seen else None
