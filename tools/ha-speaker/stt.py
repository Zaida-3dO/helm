"""Speech to text: hand the hub what the microphone heard.

Home Assistant gives us raw 16 kHz 16-bit signed mono PCM. We collect it,
cut it at the hub's own cap, wrap it in a WAV header, and post it. The hub
transcribes with its own speech model, and it is also the place where a
spoken passcode is recognised: when the capture turns out to be one, the
hub returns a placeholder instead of the words, so the digits never travel
further than the hub.

Note: Home Assistant does not tell an STT provider which satellite the audio
came from, so we cannot name a device here; the hub keys the capture on the
token (and the configured room label). The agent call right after carries
the real satellite device.
"""

import logging

from homeassistant.components.stt import Provider, SpeechResult, SpeechSettings
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant

from .const import CONF_HINT, CONF_ROOM
from .hubapi import (
    KIND_ANSWER,
    KIND_EMPTY,
    MAX_STT_SECONDS,
    SAMPLE_RATE,
    HubApi,
    pcm_cap,
    wav_from_pcm,
)

_LOGGER = logging.getLogger(__name__)


async def async_setup_entry(
    hass: HomeAssistant,
    config_entry: ConfigEntry,
    async_add_entities,
) -> None:
    api = config_entry.runtime_data["api"]
    async_add_entities(
        [HelmSpeechToText(api, config_entry.data.get(CONF_ROOM),
                          config_entry.data.get(CONF_HINT))])


class HelmSpeechToText(Provider):
    """One STT provider for the entry's hub. `supported_languages` is left
    unset, so the pipeline lets any language through: the hub's model
    decides."""

    def __init__(self, api: HubApi, room=None, hint=None):
        self.api = api
        self.room = room
        self.hint = hint

    async def async_process_audio_stream(
        self, audio_sample_rate, audio_stream, options=None
    ):
        """Collect the stream, cap it, wrap it, ask the hub.

        Returns a `SpeechResult` with the words (or the hub's placeholder),
        or None when there is nothing to say — empty capture, hub
        unreachable or failed call — which the pipeline handles as "not
        understood".
        """
        rate = int(audio_sample_rate) or SAMPLE_RATE
        cap = pcm_cap(rate, MAX_STT_SECONDS)
        pcm = bytearray()
        async for chunk in audio_stream:
            if len(pcm) < cap:
                pcm.extend(chunk)
            # keep draining the stream past the cap so the sender stays happy
        if not pcm:
            return None
        result = await self.api.transcribe(bytes(pcm[:cap]), room=self.room,
                                           hint=self.hint)
        if result.get("kind") == KIND_ANSWER:
            return SpeechResult(SpeechSettings(language=None), result["text"])
        if result.get("kind") != KIND_EMPTY:
            _LOGGER.debug("speaker stt: hub said %s", result.get("kind"))
        return None
