"""The conversation agent: ask the hub one question, speak its answer.

The hub owns the conversation: sessions, who is speaking, what the speaker
may do, and the answers themselves. This side only:
  * gives every attempt a fresh uuid, so the hub can spot a duplicate,
  * tells the hub which satellite and area the question came from, plus a
    small snapshot of who we think is in the room,
  * keeps asking while the hub says it is still working, and
  * speaks a fixed sentence when there is no model answer (hub unreachable,
    still working past the wait, or a failed call).
"""

import logging
import uuid

from homeassistant.components.conversation import (
    ConversationAgentEntity,
    ConversationAgentEntityDescription,
)
from homeassistant.components.conversation.const import TYPE_ANSWER
from homeassistant.components.conversation.models import (
    ConversationResponse,
    ConversationResult,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers import area_registry as ar
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er

from .const import CONF_HINT, CONF_ROOM
from .hubapi import (
    ERROR_TEXT,
    KIND_ANSWER,
    KIND_OFFLINE,
    KIND_TIMEOUT,
    OFFLINE_TEXT,
    TIMEOUT_TEXT,
    HubApi,
    build_presence,
    ends_with_question,
)

_LOGGER = logging.getLogger(__name__)

AGENT_DESCRIPTION = ConversationAgentEntityDescription(
    key="helm_speaker",
    name="Helm speaker",
)


async def async_setup_entry(
    hass: HomeAssistant,
    config_entry: ConfigEntry,
    async_add_entities,
) -> None:
    async_add_entities([HelmSpeakerAgent(hass, config_entry)])


class HelmSpeakerAgent(ConversationAgentEntity):
    """One agent entity per hub entry."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry):
        super().__init__(AGENT_DESCRIPTION)
        self.hass = hass
        self.entry = entry
        self._attr_has_entity_name = False

    async def async_generate(self, conversation_input) -> ConversationResult:
        api: HubApi = self.entry.runtime_data["api"]
        device_id = getattr(conversation_input, "device_id", None)
        area_name, presence = self._room_facts(device_id)
        result = await api.run_turn(
            text=conversation_input.text,
            turn_id=uuid.uuid4().hex,
            device=device_id or self.entry.entry_id,
            room=self.entry.data.get(CONF_ROOM),
            area=area_name,
            hint=self.entry.data.get(CONF_HINT),
            presence=presence,
        )
        kind = result.get("kind")
        if kind == KIND_ANSWER:
            text = result["text"]
        elif kind == KIND_OFFLINE:
            text = OFFLINE_TEXT
        elif kind == KIND_TIMEOUT:
            text = TIMEOUT_TEXT
        else:
            _LOGGER.debug("speaker turn ended as %s", kind)
            text = ERROR_TEXT
        # HA reopens the microphone exactly when continue_conversation is
        # true; we do that only for replies that plainly ask something back.
        return ConversationResult(
            conversation_response=ConversationResponse(
                text=text,
                response_type=TYPE_ANSWER,
                continue_conversation=ends_with_question(text),
            ),
            conversation_id=None,
        )

    def _room_facts(self, device_id):
        """The area the asking satellite sits in, and a plain-data snapshot
        of the room for the hub's registry. All of it optional: nothing
        fails if the satellite has no area."""
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
