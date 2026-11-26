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
import time

from homeassistant.components.stt import (
    AudioBitRates,
    AudioChannels,
    AudioCodecs,
    AudioFormats,
    AudioSampleRates,
    SpeechMetadata,
    SpeechResult,
    SpeechResultState,
    SpeechToTextEntity,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers.device_registry import DeviceInfo

from .const import CONF_HINT, CONF_ROOM, DOMAIN
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

# The hub's model decides the language; Home Assistant only needs the pipeline's own language
# to appear in this list.
LANGUAGES = ["en", "en-GB", "en-US", "en-AU", "en-IE", "en-CA", "en-NZ", "en-ZA", "en-IN"]


async def async_setup_entry(
    hass: HomeAssistant,
    config_entry: ConfigEntry,
    async_add_entities,
) -> None:
    api = config_entry.runtime_data["api"]
    async_add_entities(
        [HelmSpeechToText(config_entry, api, config_entry.data.get(CONF_ROOM),
                          config_entry.data.get(CONF_HINT))])


class HelmSpeechToText(SpeechToTextEntity):
    """One speech-to-text entity for the entry's hub."""

    _attr_has_entity_name = False
    _attr_name = "Helm speaker"

    def __init__(self, entry: ConfigEntry, api: HubApi, room=None, hint=None):
        self.entry = entry  # for the stage timings hand-off
        self.api = api
        self.room = room
        self.hint = hint
        self._attr_unique_id = f"{entry.entry_id}-stt"
        self._attr_device_info = DeviceInfo(identifiers={(DOMAIN, entry.entry_id)})

    @property
    def supported_languages(self):
        return LANGUAGES

    @property
    def supported_formats(self):
        return [AudioFormats.WAV]

    @property
    def supported_codecs(self):
        return [AudioCodecs.PCM]

    @property
    def supported_bit_rates(self):
        return [AudioBitRates.BITRATE_16]

    @property
    def supported_sample_rates(self):
        return [AudioSampleRates.SAMPLERATE_16000]

    @property
    def supported_channels(self):
        return [AudioChannels.CHANNEL_MONO]

    async def async_process_audio_stream(
        self, metadata: SpeechMetadata, stream
    ) -> SpeechResult:
        """Collect the stream, cap it, wrap it, ask the hub.

        Returns the words (or the hub's placeholder for a spoken passcode), or an error result
        when there is nothing to say: empty capture, hub unreachable or a failed call. The
        pipeline treats that as "not understood".
        """
        started = time.monotonic()
        rate = int(metadata.sample_rate) or SAMPLE_RATE
        cap = pcm_cap(rate, MAX_STT_SECONDS)
        pcm = bytearray()
        async for chunk in stream:
            if len(pcm) < cap:
                pcm.extend(chunk)
            # keep draining the stream past the cap so the sender stays happy
        collected = time.monotonic()
        if not pcm:
            return SpeechResult(None, SpeechResultState.ERROR)
        result = await self.api.transcribe(wav_from_pcm(bytes(pcm[:cap]), rate),
                                           room=self.room, hint=self.hint)
        done = time.monotonic()
        wall = time.time()
        # Stage timings, so a slow answer can be pinned on a stage: how long the speaker talked
        # (audio plus the pipeline's silence wait), then the hub's transcription call.
        _LOGGER.info(
            "helm_speaker timing: stt heard %.1f s of audio over %.2f s, hub transcribed in %.2f s (%s)",
            len(pcm) / (2 * rate), collected - started, done - collected, result.get("kind"),
        )
        # Hand the same numbers to stages.py (wall-clock stamps so the turn
        # can join them later); one slot, newest capture wins.
        try:
            self.entry.runtime_data["stt_last"] = {
                "listen_start_ts": wall - (done - started),
                "collected_ts": wall - (done - collected),
                "done_ts": wall,
                "listened_s": collected - started,
                "stt_ms": done - collected,
            }
        except (AttributeError, KeyError, TypeError):
            pass  # entry torn down or no runtime yet: the log line already told it
        if result.get("kind") == KIND_ANSWER:
            return SpeechResult(result["text"], SpeechResultState.SUCCESS)
        if result.get("kind") != KIND_EMPTY:
            _LOGGER.debug("speaker stt: hub said %s", result.get("kind"))
        return SpeechResult(None, SpeechResultState.ERROR)
