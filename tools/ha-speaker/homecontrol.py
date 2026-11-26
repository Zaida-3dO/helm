"""The four house-control tools the speaker model sees, and their execution.

Pure standard library: everything Home Assistant-side arrives through an
injected Backend, which production wires to ``hass`` (ha_backend.py) and tests
to a small fake house. Like hubapi and agent_core this keeps the decision
logic testable without HA loaded.

Backend contract (all async where it says await):
    exposed() -> {area_name: {"lights": [(name, entity_id)],
                              "climates": [(name, entity_id)]}}
                only entities Home Assistant itself exposes to the
                conversation assistant - that expose switch is the user's
                control over what voice may touch at all
    await states(entity_ids) -> {entity_id: {"state": str,
                                             "attributes": dict}}
    await call_service(domain, service, data) -> None
    await intent(name, data) -> plain dict (maybe {}) or None

Targeting is by exposed names and areas only: area first, then exact name,
then fuzzy name. Hidden or unexposed entities simply do not exist here. An
ambiguous target returns the candidates instead of guessing. A dry executor
returns what it would call and changes nothing; replay defaults to dry.
"""

import difflib


MAX_ENTITIES = 12

LIGHT_ACTIONS = ("on", "off", "toggle", "set", "status")
CLIMATE_ACTIONS = ("set", "status")
TIMER_ACTIONS = ("start", "cancel", "status")

# Common colour words a spoken sentence actually uses, to hex.
CSS_COLORS = {
    "red": "#ff0000", "orange": "#ff8c00", "yellow": "#ffff00",
    "green": "#00ff00", "teal": "#008080", "cyan": "#00ffff",
    "blue": "#0000ff", "purple": "#a020f0", "pink": "#ffc0cb",
    "magenta": "#ff00ff", "white": "#ffffff", "warm white": "#ffc58f",
    "cool white": "#c9e7ff", "black": "#000000", "brown": "#8b4513",
}

# Spoken temperature words to kelvin (then clamped per light).
COLOR_TEMP_WORDS = {"warm": 2700, "neutral": 4000, "cool": 6000}

# Built-in HA voice-timer intents (the satellite owns these; the timer.* helper
# entities in this house are motion/door helpers and never targets).
TIMER_INTENTS = {"start": "HassStartTimer", "cancel": "HassCancelTimer",
                 "status": "HassTimerStatus"}


def _norm(text):
    """Normalise a spoken or listed name for matching: no case, no punctuation,
    one space between words, no leading article."""
    s = "".join(ch if ch.isalnum() else " " for ch in str(text).lower())
    s = " ".join(s.split())
    for lead in ("the ", "a ", "an "):
        if s.startswith(lead):
            s = s[len(lead):]
    return s


# --------------------------------------------------------------------------
# schemas: flat argument names a small model can use. This exact JSON is the
# golden file tools_schema.golden.json; keep it short - a test pins the whole
# set under 450 estimated tokens.

def _fn(name, desc, props, required=()):
    return {"type": "function",
            "function": {"name": name, "description": desc,
                         "parameters": {"type": "object", "properties": props,
                                        "required": list(required)}}}


SCHEMAS = [
    _fn("light", "Turn or set lights; status reads them.", {
        "target": {"type": "string", "description": "area or light name"},
        "action": {"type": "string", "enum": list(LIGHT_ACTIONS)},
        "brightness_pct": {"type": "number", "description": "0-100"},
        "brightness_change_pct": {"type": "number",
                                  "description": "-100..100, brighter/dimmer"},
        "color": {"type": "string", "description": "colour word or r,g,b"},
        "color_temp": {"type": "string",
                       "description": "warm, neutral, cool or kelvin"}},
        ("target", "action")),
    _fn("climate", "Set heating; status reads it.", {
        "target": {"type": "string", "description": "area or heater name"},
        "action": {"type": "string", "enum": list(CLIMATE_ACTIONS)},
        "temperature": {"type": "number"},
        "mode": {"type": "string"}},
        ("target", "action")),
    _fn("timer", "Voice timer: start, cancel or status.", {
        "action": {"type": "string", "enum": list(TIMER_ACTIONS)},
        "minutes": {"type": "number"},
        "label": {"type": "string"}},
        ("action",)),
    _fn("home_state", "Read one room: what is on, temperatures.", {
        "target": {"type": "string", "description": "area name"}}),
]


class HomeControlError(Exception):
    """A refusal with a ready sentence for the model."""

    def __init__(self, text, candidates=None):
        super().__init__(text)
        self.text = text
        self.candidates = candidates or []


class HomeControl:
    """Executes one tool call end to end: resolve, validate, act (or dry)."""

    def __init__(self, backend, dry=False):
        self.backend = backend
        self.dry = dry
        # exposed() is cheap in HA; cache for the turn and refresh per execute
        # only when empty, so a mid-activation expose still works next call.
        self._exposed_cache = None

    @property
    def schemas(self):
        return SCHEMAS

    async def execute(self, name, args):
        args = args if isinstance(args, dict) else {}
        try:
            if name == "light":
                return await self._light(args)
            if name == "climate":
                return await self._climate(args)
            if name == "timer":
                return await self._timer(args)
            if name == "home_state":
                return await self._home_state(args)
            return {"ok": False, "error": "unknown tool: " + str(name)}
        except HomeControlError as exc:
            out = {"ok": False, "error": exc.text}
            if exc.candidates:
                out["candidates"] = exc.candidates[:6]
            return out

    # ---------------- targeting ----------------

    async def _exposed(self):
        if not self._exposed_cache:
            data = await self.backend.exposed()
            self._exposed_cache = data or {}
        return self._exposed_cache

    def _pool(self, exposed, kind):
        """[(display_name, entity_id, area)] for lights or climates."""
        pool = []
        for area, entry in (exposed or {}).items():
            for name, entity_id in ((entry or {}).get(kind) or []):
                pool.append((str(name), str(entity_id), str(area)))
        return pool

    def _resolve(self, target, kind):
        """Area first, then exact name, then fuzzy. Returns a list of unique
        (name, entity_id) or raises HomeControlError with candidates when the
        spoken target is unknown or ambiguous."""
        wanted = _norm(target)
        if not wanted:
            raise HomeControlError("no target given")
        pool = self._pool(self._exposed_cache, kind)
        areas = {area for _, _, area in pool}
        hit_areas = [a for a in areas if _norm(a) == wanted]
        if hit_areas:
            picked = [(n, e) for n, e, a in pool if a in hit_areas]
            return dedupe(picked)
        exact = dedupe([(n, e) for n, e, _ in pool if _norm(n) == wanted])
        if len(exact) == 1:
            return exact
        if len(exact) > 1:
            raise HomeControlError("which one did you mean?",
                                   candidates=sorted({n for n, _ in exact}))
        # fuzzy: score every exposed name; keep the best spelling and any
        # other that is within a narrow margin of it. One distinct name wins;
        # several are a question we must ask, never guess.
        scored = []
        for n, e, _ in pool:
            score = difflib.SequenceMatcher(None, wanted, _norm(n)).ratio()
            if score >= 0.72:
                scored.append((score, n))
        if scored:
            best = max(s for s, _ in scored)
            keep = {n for s, n in scored if s >= max(0.72, best - 0.1)}
            distinct = {_norm(n) for n in keep}
            fuzzy = dedupe([(n, e) for n, e, _ in pool if n in keep])
            if len(distinct) == 1:
                return fuzzy
            raise HomeControlError("which one did you mean?",
                                   candidates=sorted(keep))
        label = {"lights": "light", "climates": "climate"}.get(kind, kind)
        raise HomeControlError("I don't know a %s called '%s'." % (label, target),
                               candidates=sorted({n for n, _, _ in pool})[:8])

    async def _limited(self, picked, what):
        """Pass the (name, entity_id) pairs through, or refuse the call."""
        if len(picked) > MAX_ENTITIES:
            raise HomeControlError(
                "that is %d %s; I only change %d at once."
                % (len(picked), what, MAX_ENTITIES))
        return picked


    # ---------------- helpers ----------------

    async def _states(self, entity_ids):
        got = await self.backend.states(list(entity_ids))
        return got or {}

    @staticmethod
    def _attrs(states, eid):
        entry = states.get(eid)
        if isinstance(entry, dict):
            attrs = entry.get("attributes")
            return attrs if isinstance(attrs, dict) else {}
        attrs = getattr(entry, "attributes", None)  # tolerate HA State objects
        return attrs if isinstance(attrs, dict) else {}

    @staticmethod
    def _state_of(states, eid):
        entry = states.get(eid)
        if isinstance(entry, dict):
            return str(entry.get("state", ""))
        return str(getattr(entry, "state", ""))

    # ---------------- light ----------------

    async def _light(self, args):
        action = str(args.get("action") or "").lower()
        if action not in LIGHT_ACTIONS:
            raise HomeControlError("the light action must be one of: "
                                   + ", ".join(LIGHT_ACTIONS),
                                   candidates=list(LIGHT_ACTIONS))
        picked = await self._limit(args.get("target"), "lights", "lights")
        eids = [e for _, e in picked]
        states = await self._states(eids)

        if action == "status":
            return self._light_status(picked, states)

        plan = []  # (service, data) - one call per payload group
        base = {"entity_id": eids}
        extra = {}

        brightness_pct = args.get("brightness_pct")
        if brightness_pct is not None:
            pct = self._pct(brightness_pct, lo=0, hi=100)
            extra["brightness"] = int(round(pct * 255 / 100))
        change_pct = args.get("brightness_change_pct")
        if change_pct is not None:
            step = self._pct(change_pct, lo=-100, hi=100)
            extra["brightness_step_pct"] = int(round(step))

        kelvin_groups = []
        color_temp = args.get("color_temp")
        if color_temp is not None:
            kelvin_groups = self._kelvin_by_group(color_temp, eids, states)
        color_hex = None
        if args.get("color") is not None:
            color_hex = self._parse_color(args["color"])
            cannot = [name for name, eid in picked
                      if not self._supports_color(states, eid)]
            if cannot:
                raise HomeControlError(
                    "these lights cannot change colour: "
                    + ", ".join(cannot))

        if action == "on" or (action == "set" and not extra):
            plan.append(("light", "turn_on", dict(base)))
        elif action == "off":
            plan.append(("light", "turn_off", dict(base)))
        elif action == "toggle":
            plan.append(("light", "toggle", dict(base)))
        elif action == "set":
            if not (extra or kelvin_groups or color_hex):
                raise HomeControlError(
                    "to set a light give brightness, colour or temperature")
            if extra:
                plan.append(("light", "turn_on", {**base, **extra}))
        if color_hex:
            plan.append(("light", "turn_on", {"entity_id": eids,
                                              "rgb_color": self._hex_to_rgb(
                                                  color_hex)}))
        for kelvin, group in kelvin_groups:
            plan.append(("light", "turn_on",
                         {"entity_id": group, "color_temp_kelvin": kelvin}))

        return await self._run(plan, "done")

    async def _limit(self, target, what, kind):
        # populate the expose map before any resolution consults it
        await self._exposed()
        picked = self._resolve(target, kind)
        return await self._limited(picked, what)

    def _light_status(self, picked, states):
        lines, on_count = [], 0
        for name, eid in picked:
            st = self._state_of(states, eid)
            attrs = self._attrs(states, eid)
            if st == "on":
                on_count += 1
                bright = attrs.get("brightness")
                pct = round(bright * 100 / 255) if isinstance(
                    bright, (int, float)) else None
                lines.append("%s is on at %d percent" % (name, pct)
                             if pct is not None else "%s is on" % name)
            else:
                lines.append("%s is off" % name)
        summary = "%d of %d on." % (on_count, len(picked))
        return {"ok": True, "text": summary + " " + "; ".join(lines)}

    @staticmethod
    def _pct(value, lo, hi):
        try:
            num = float(value)
        except (TypeError, ValueError):
            raise HomeControlError("that number does not compute")
        if not (lo <= num <= hi):
            raise HomeControlError("it must be between %d and %d" % (lo, hi))
        return num

    @staticmethod
    def _supports_color(states, eid):
        modes = HomeControl._attrs(states, eid).get("supported_color_modes") or []
        return any(m in ("rgb", "rgbw", "rgbww", "hs", "xy", "color_temp")
                   for m in modes)

    @staticmethod
    def _parse_color(value):
        """Colour word to hex, or r,g,b list/tuple/string of three ints."""
        if isinstance(value, str):
            key = " ".join(value.lower().split())
            if key in CSS_COLORS:
                return CSS_COLORS[key]
            if "," in key:  # "255, 128, 0" written as one string
                value = [part.strip() for part in key.split(",")]
            else:
                raise HomeControlError("I don't know the colour '%s'" % value)
        if isinstance(value, (list, tuple)) and len(value) == 3:
            try:
                parts = [int(float(p)) for p in value]
            except (TypeError, ValueError):
                raise HomeControlError("rgb values must be whole numbers")
            if all(0 <= p <= 255 for p in parts):
                return "#%02x%02x%02x" % tuple(parts)
            raise HomeControlError("rgb values go from 0 to 255")
        raise HomeControlError("I don't understand that colour")

    @staticmethod
    def _hex_to_rgb(hex_str):
        return [int(hex_str[1:3], 16), int(hex_str[3:5], 16),
                int(hex_str[5:7], 16)]

    def _kelvin_by_group(self, value, eids, states):
        """Spoken or numeric colour temperature, clamped to each light's own
        range; lights end up in one group per resulting kelvin."""
        if isinstance(value, str) and value.strip().lower() in COLOR_TEMP_WORDS:
            base_k = COLOR_TEMP_WORDS[value.strip().lower()]
        else:
            try:
                base_k = int(float(value))
            except (TypeError, ValueError):
                raise HomeControlError("temperature must be warm, neutral, cool"
                                       " or kelvin")
            if not 1000 <= base_k <= 10000:
                raise HomeControlError("kelvin must be between 1000 and 10000")
        groups = {}
        for eid in eids:
            attrs = self._attrs(states, eid)
            lo = attrs.get("min_color_temp_kelvin", 2000)
            hi = attrs.get("max_color_temp_kelvin", 6500)
            k = max(int(lo), min(int(hi), base_k))
            groups.setdefault(k, []).append(eid)
        return sorted(groups.items())

# --------------------------------------------------------------------------
# climate, timer, home_state and the executor live on HomeControl.

    # ---------------- climate ----------------

    async def _climate(self, args):
        action = str(args.get("action") or "").lower()
        if action not in CLIMATE_ACTIONS:
            raise HomeControlError("the climate action must be set or status",
                                   candidates=list(CLIMATE_ACTIONS))
        picked = await self._limit(args.get("target"), "heating", "climates")
        eids = [e for _, e in picked]
        states = await self._states(eids)

        if action == "status":
            lines = []
            for name, eid in picked:
                attrs = self._attrs(states, eid)
                cur = attrs.get("current_temperature")
                tgt = attrs.get("temperature")
                mode = attrs.get("hvac_mode", "")
                bits = ["%s is %s" % (name, mode)] if mode else [str(name)]
                if isinstance(cur, (int, float)):
                    bits.append("now %.0f" % cur)
                if isinstance(tgt, (int, float)):
                    bits.append("set to %.0f" % tgt)
                lines.append(", ".join(bits))
            return {"ok": True, "text": "; ".join(lines) + "."}

        temperature = args.get("temperature")
        mode = args.get("mode")
        if temperature is None and not mode:
            raise HomeControlError("setting the heating needs a temperature"
                                   " or a mode")
        plan = []
        if mode:
            wanted = str(mode).lower()
            bad = []
            for name, eid in picked:
                modes = [str(m).lower() for m in
                         (self._attrs(states, eid).get("hvac_modes") or [])]
                if wanted not in modes:
                    bad.append("%s supports %s"
                               % (name, ", ".join(modes) or "no modes"))
            if bad:
                raise HomeControlError("; ".join(bad))
            plan.append(("climate", "set_hvac_mode",
                         {"entity_id": eids, "hvac_mode": wanted}))
        if temperature is not None:
            try:
                want = float(temperature)
            except (TypeError, ValueError):
                raise HomeControlError("that temperature does not compute")
            groups, complaints = {}, []
            for name, eid in picked:
                attrs = self._attrs(states, eid)
                lo = float(attrs.get("min_temp", 7))
                hi = float(attrs.get("max_temp", 35))
                step = float(attrs.get("target_temp_step") or 0.5)
                if want < lo - 1e-9 or want > hi + 1e-9:
                    complaints.append("%s goes from %g to %g" % (name, lo, hi))
                    continue
                snapped = max(lo, min(hi, round(want / step) * step))
                groups.setdefault(round(snapped, 4), []).append(eid)
            if not groups:
                raise HomeControlError("; ".join(complaints))
            for setpoint in sorted(groups):
                plan.append(("climate", "set_temperature",
                             {"entity_id": groups[setpoint],
                              "temperature": setpoint}))
        summary = ("heating set to %s" % temperature
                   if temperature is not None else str(mode))
        return await self._run(plan, summary)

    # ---------------- timer (HA built-in voice timers) ----------------

    async def _timer(self, args):
        action = str(args.get("action") or "").lower()
        if action not in TIMER_ACTIONS:
            raise HomeControlError("the timer action must be start, cancel"
                                   " or status", candidates=list(TIMER_ACTIONS))
        data = {}
        label = args.get("label")
        if isinstance(label, str) and label.strip():
            data["label"] = label.strip()[:80]
        plain = None
        if action == "start":
            minutes = args.get("minutes")
            try:
                mins = float(minutes)
            except (TypeError, ValueError):
                raise HomeControlError("starting a timer needs minutes")
            if not 0 < mins <= 240:
                raise HomeControlError("timers go up to 240 minutes")
            data["seconds"] = int(round(mins * 60))
            plain = "Timer started for %g minute%s." % (
                mins, "" if mins == 1 else "s")
        elif action == "cancel":
            plain = "Timer cancelled."
        intent_name = TIMER_INTENTS[action]
        if self.dry:
            return {"ok": True, "dry": True,
                    "would_call": [{"intent": intent_name, "data": data}],
                    "text": plain or "timer status"}
        result = await self.backend.intent(intent_name, data)
        speech = None
        if isinstance(result, dict):
            for key in ("text", "speech", "response"):
                value = result.get(key)
                if isinstance(value, str) and value.strip():
                    speech = value.strip()[:200]
                    break
        return {"ok": True, "text": speech or plain or "done"}

    # ---------------- home_state (the only read tool) ----------------

    async def _home_state(self, args):
        exposed = await self._exposed()
        wanted = _norm(args.get("target"))
        areas = set(exposed or {})
        hit = [a for a in areas if _norm(a) == wanted]
        if hit:
            entry = exposed[hit[0]] or {}
            eids = [e for _, e in ((entry.get("lights") or [])
                                   + (entry.get("climates") or []))]
            eids = eids[:MAX_ENTITIES]
        else:  # allow an entity name as the target too
            picked = ([p[:2] for p in self._pool(exposed, "lights")
                       if _norm(p[0]) == wanted]
                      + [p[:2] for p in self._pool(exposed, "climates")
                         if _norm(p[0]) == wanted])
            if not picked:
                raise HomeControlError("I don't know that room",
                                       candidates=sorted(areas)[:8])
            eids = [e for _, e in picked]
        states = await self._states(eids)
        bits, on_count = [], 0
        for eid in eids:
            attrs = self._attrs(states, eid)
            name = attrs.get("friendly_name") or eid
            if eid.startswith("light."):
                if self._state_of(states, eid) == "on":
                    on_count += 1
            elif eid.startswith("climate."):
                cur, tgt = attrs.get("current_temperature"), \
                    attrs.get("temperature")
                if isinstance(cur, (int, float)):
                    bits.append("%s at %.0f (set %.0f)"
                                % (name, cur,
                                   tgt if isinstance(tgt, (int, float))
                                   else cur))
        text = "%d lights on" % on_count
        if bits:
            text += "; " + "; ".join(bits)
        return {"ok": True, "text": text + "."}

    # ---------------- acting ----------------

    async def _run(self, plan, summary):
        if self.dry:
            return {"ok": True, "dry": True,
                    "would_call": [{"service": "%s.%s" % (d, s), "data": data}
                                   for d, s, data in plan],
                    "text": "would " + str(summary)}
        for domain, service, data in plan:
            await self.backend.call_service(domain, service, data)
        return {"ok": True, "text": str(summary)}


def dedupe(pairs):
    seen, out = set(), []
    for name, eid in pairs:
        if eid not in seen:
            seen.add(eid)
            out.append((name, eid))
    return out
