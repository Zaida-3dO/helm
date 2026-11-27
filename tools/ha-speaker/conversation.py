"""The conversation agent: a small model answers here, Helm takes the rest.

One turn arrives -> build the minimised context (time, asking room, exposed
light and climate names, short history) -> agent_core runs the lean prompt
against the configured chat endpoint with the five house tools -> speak the
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
from homeassistant.util import dt as dt_util

from . import agent_core
from . import errorlines
from .const import (
    CONF_DRY,
    CONF_HINT,
    CONF_HOUSE_TOKEN,
    CONF_HOUSE_URL,
    CONF_ROOM,
    DOMAIN,
)
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

# Escalation happens while a person is standing at the speaker waiting: the
# whole hub round gets a short budget (the model turn already had its own),
# and when it runs out the fixed sentence goes instead of an open-ended wait.
ESCALATION_BUDGET_S = 18
ESCALATION_FIRST_WAIT = 15
NO_HUB_TEXT = "I can't do that one from here."


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

        # the optional chores/plants feed rides along: same entry transport,
        # read-only token from setup; unset means home_status speaks the
        # not-set-up sentence instead of reaching anywhere
        backend = HaBackend(self.hass, satellite_device_id=device_id,
                            transport=runtime.get("transport"),
                            house_url=self.entry.data.get(CONF_HOUSE_URL),
                            house_token=self.entry.data.get(CONF_HOUSE_TOKEN))
        has_hub = runtime.get("api") is not None
        dry = bool(runtime.get("control_dry")) or bool(
            (self.entry.options or {}).get(CONF_DRY))
        # with no hub there is nobody to ask: home control stops offering the
        # ask_helm tool and the prompt stops mentioning it
        control = HomeControl(backend, dry=dry, ask_helm=has_hub)
        ctx = await self._ctx(backend, device_id, runtime["history"].setdefault(key, []))
        try:
            result = await agent_core.run_turn(
                cfg, runtime["transport"], control, ctx, text,
                tags=self._tags(), clock=time.time)
        except Exception:  # noqa: BLE001 - nothing may escape async_process:
            # a broken turn must still end as a spoken sentence, not a stack
            _LOGGER.exception("helm_speaker turn %s crashed", turn_id[:8])
            result = agent_core.TurnResult(text=ERROR_TEXT, kind="error")

        kind = result.kind
        spoken = result.text
        record_kind = "answer"
        hub_detail = None
        if kind == agent_core.KIND_ESCALATE:
            record_kind = "escalated"
            api = runtime.get("api")
            if api is not None:
                hub_detail = await self._escalate(api, device_id,
                                                  result.escalate_text or text, ctx)
                spoken = hub_detail["text"]
            else:
                # without a hub ask_helm is not even offered; this only catches
                # a model that calls it anyway - answer plainly, no fake error
                spoken = NO_HUB_TEXT
        elif kind == agent_core.KIND_TIMEOUT:
            record_kind = "error"
        elif kind in (agent_core.KIND_OFFLINE, agent_core.KIND_ERROR):
            record_kind = "error"

        # Pre-recorded failure lines: we only DECIDE here. An empty response
        # string makes the voice pipeline skip TTS; a Home Assistant automation
        # watching this satellite's states announces the matching flac file (the
        # component must never announce mid-turn - that cancels our own run).
        # No code, or no satellite entity for this device, or no lines file:
        # every row below carries today's live sentence and the turn behaves
        # exactly as before.
        if kind == agent_core.KIND_ESCALATE and hub_detail is not None:
            code = errorlines.code_for_turn(
                kind, has_satellite=bool(device_id),
                escalate_state=hub_detail.get("kind"),
                escalate_status=hub_detail.get("status"),
                escalate_text=hub_detail["text"])
        else:
            code = errorlines.code_for_turn(kind, text=spoken,
                                            has_satellite=bool(device_id))
        entity_id = (self._satellite_entity(device_id)
                     if code and device_id else None)
        announce = bool(code and entity_id)
        # history, benchmark stages and the hub record keep the canonical
        # sentence even when the flac replaces its live twin
        row_text = errorlines.canonical_text(code) if announce else spoken

        history = runtime["history"].setdefault(key, [])
        history.append({"user": text, "spoken": row_text, "ts": time.time()})
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
                                 text_out=row_text, kind=kind, result=result)

        api = runtime.get("api")
        if api is not None and getattr(api, "record", None) is not None \
                and cfg.record_text:
            # a background task owned by the entry: it dies with an unload
            # instead of haunting a reloaded integration
            coro = self._record(
                api, turn_id, device_id, text, row_text, record_kind, result)
            if hasattr(self.entry, "async_create_background_task"):
                self.entry.async_create_background_task(
                    self.hass, coro, "helm_speaker record " + turn_id[:8])
            else:  # older Home Assistant: plain task still works
                asyncio.create_task(coro)

        response = intent.IntentResponse(language=user_input.language)
        if announce:
            # empty speech: the run loop sees a blank tts_input and ends the
            # turn without synthesising anything - the pre-recorded flac is
            # already on its way from the automation instead
            response.async_set_speech("")
            self.hass.bus.async_fire("helm_speaker_line",
                                     {"code": code, "entity_id": entity_id})
        else:
            response.async_set_speech(spoken)
        return ConversationResult(
            response=response,
            conversation_id=user_input.conversation_id,
            # the microphone rule reads the sentence we meant, not the empty
            # string handed to the pipeline
            continue_conversation=agent_core.ends_with_question(row_text),
        )

    def _satellite_entity(self, device_id):
        """The assist_satellite entity sitting on the asking satellite's
        device - what announces target. None if the device carries none (text
        or REST turns), which keeps this turn on live speech."""
        entries = er.async_get(self.hass)
        for entry in er.async_entries_for_device(entries, device_id):
            if entry.domain == "assist_satellite":
                return entry.entity_id
        return None

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
        # the house clock, not the machine's UTC: a timer or "is it late"
        # question answered in the wrong zone is wrong, not close
        return {"room": room, "areas": areas, "history": list(history),
                "now": dt_util.now().strftime("%Y-%m-%d %H:%M (%A)")}

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
            context=context, escalation=True,
            budget=ESCALATION_BUDGET_S, first_wait=ESCALATION_FIRST_WAIT)
        kind = result.get("kind")
        status = result.get("status")  # http code when the hub itself replied
        if kind == KIND_ANSWER:
            return {"text": result["text"], "kind": "answer"}
        if kind == "offline":
            return {"text": OFFLINE_TEXT, "kind": "offline"}
        if kind == "timeout":
            return {"text": TIMEOUT_TEXT, "kind": "timeout"}
        if status in (401, 403):
            # a wrong token or scope is someone's setup mistake: worth one
            # grep-able line; out loud it stays the plain try-again sentence
            _LOGGER.warning(
                "helm_speaker escalation refused with http %s - check the "
                "speaker token and its scope", status)
        else:
            _LOGGER.debug("escalation ended as %s", kind)
        return {"text": ERROR_TEXT, "kind": "error", "status": status}

    async def _record(self, api, turn_id, device_id, asked, spoken, kind, result):
        """Best effort: the hub route may not exist yet; never raise out of
        a background task (the hub record call already swallows its own failures)."""
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
