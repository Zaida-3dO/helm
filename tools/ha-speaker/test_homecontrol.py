"""Table-driven tests for homecontrol against a fake Backend house.

python -B -m unittest test_homecontrol

The fixture house: Kitchen (one light, one radiator), Living Room (a colour
bulb with a clamped kelvin range, a brightness-only bulb, duplicated-name
pendant in Bedroom to force ambiguity), plus an unexposed strip that must be
invisible to every resolution path.
"""

import asyncio
import json
import os
import unittest

import homecontrol


GOLDEN_SCHEMA = os.path.join(os.path.dirname(__file__), "tools_schema.golden.json")

EXPOSED = {
    "Kitchen": {"lights": [("Kitchen lights", "light.kitchen")],
                "climates": [("Kitchen Radiator", "climate.kitchen_rad")]},
    "Living Room": {
        "lights": [("Sofa lamp", "light.sofa"),
                   ("Reading lamp", "light.reading")],
        "climates": [("Living Room Thermostat", "climate.lr_therm")]},
    "Bedroom": {"lights": [("Bed light", "light.bed"),
                           ("Desk light", "light.pendant"),
                           ("Bath bulb", "light.ctlamp")],
                "climates": []},
    "Study": {"lights": [("Desk light", "light.desk2")], "climates": []},
}

STATES = {
    "light.kitchen": {"state": "off", "attributes": {
        "friendly_name": "Kitchen lights",
        "supported_color_modes": ["onoff"]}},
    "light.sofa": {"state": "on", "attributes": {
        "friendly_name": "Sofa lamp", "brightness": 128,
        "supported_color_modes": ["rgb", "color_temp"],
        "min_color_temp_kelvin": 3000, "max_color_temp_kelvin": 4000}},
    "light.reading": {"state": "off", "attributes": {
        "friendly_name": "Reading lamp",
        "supported_color_modes": ["brightness"]}},
    "light.bed": {"state": "off", "attributes": {
        "friendly_name": "Bed light",
        "supported_color_modes": ["onoff"]}},
    "light.pendant": {"state": "off", "attributes": {
        "friendly_name": "Desk light",
        "supported_color_modes": ["onoff"]}},
    "light.desk2": {"state": "off", "attributes": {
        "friendly_name": "Desk light",
        "supported_color_modes": ["onoff"]}},
    "light.ctlamp": {"state": "off", "attributes": {
        "friendly_name": "Bath bulb",
        "supported_color_modes": ["color_temp"],
        "min_color_temp_kelvin": 2700, "max_color_temp_kelvin": 6500}},
    "climate.kitchen_rad": {"state": "heat", "attributes": {
        "friendly_name": "Kitchen Radiator", "hvac_modes": ["off", "heat"],
        "min_temp": 7, "max_temp": 27, "target_temp_step": 0.5,
        "current_temperature": 19.0, "temperature": 21.0}},
    "climate.lr_therm": {"state": "heat", "attributes": {
        "friendly_name": "Living Room Thermostat",
        "hvac_modes": ["off", "heat", "auto"], "min_temp": 5, "max_temp": 30,
        "current_temperature": 20.5, "temperature": 21.0}},
}


class FakeBackend:
    def __init__(self, exposed=None):
        self.exposed_map = EXPOSED if exposed is None else exposed
        self.service_calls = []
        self.intent_calls = []

    async def exposed(self):
        return self.exposed_map

    async def states(self, entity_ids):
        return {eid: STATES[eid] for eid in entity_ids if eid in STATES}

    async def call_service(self, domain, service, data):
        self.service_calls.append((domain, service, json.loads(
            json.dumps(data))))  # copy, like crossing a real boundary

    async def intent(self, name, data):
        self.intent_calls.append((name, dict(data)))
        return {"text": "One timer left."}


def run(args_list, exposed=None, dry=False):
    backend = FakeBackend(exposed)
    control = homecontrol.HomeControl(backend, dry=dry)

    async def go():
        out = []
        for args in args_list:
            name, tool_args = args
            out.append(await control.execute(name, tool_args))
        return out
    results = asyncio.run(go())
    return results, backend


class ResolveTest(unittest.TestCase):
    def test_area_target_hits_all_lights_in_area(self):
        _, b = run([("light", {"target": "kitchen", "action": "on"})])
        self.assertEqual(b.service_calls, [("light", "turn_on",
                                            {"entity_id": ["light.kitchen"]})])

    def test_name_target_area_first_then_name(self):
        r, b = run([("light", {"target": "Sofa lamp", "action": "off"})])
        self.assertTrue(r[0]["ok"])
        self.assertEqual(b.service_calls[0],
                         ("light", "turn_off", {"entity_id": ["light.sofa"]}))

    def test_fuzzy_typo_resolves(self):
        _, b = run([("light", {"target": "kitchn lights", "action": "on"})])
        self.assertEqual(b.service_calls[0][2]["entity_id"], ["light.kitchen"])

    def test_duplicate_names_are_ambiguous_not_guessed(self):
        r, b = run([("light", {"target": "desk light", "action": "on"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("which one did you mean", r[0]["error"])
        self.assertEqual(r[0]["candidates"], ["Desk light"])
        self.assertEqual(b.service_calls, [])

    def test_hidden_entity_is_never_a_target(self):
        r, b = run([("light", {"target": "closet strip", "action": "on"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("I don't know a light called", r[0]["error"])
        self.assertEqual(b.service_calls, [])


class LightTest(unittest.TestCase):
    def test_brightness_set_and_change(self):
        r, b = run([("light", {"target": "Kitchen lights", "action": "set",
                               "brightness_pct": 40}),
                    ("light", {"target": "Kitchen lights", "action": "set",
                               "brightness_change_pct": -25})])
        self.assertTrue(r[0]["ok"] and r[1]["ok"])
        first, second = b.service_calls
        self.assertEqual(first[2]["brightness"], int(round(40 * 255 / 100)))
        self.assertEqual(second[2]["brightness_step_pct"], -25)

    def test_brightness_out_of_range_refused(self):
        r, b = run([("light", {"target": "Kitchen lights", "action": "set",
                               "brightness_pct": 140})])
        self.assertFalse(r[0]["ok"])
        self.assertEqual(b.service_calls, [])

    def test_kelvin_clamped_to_each_lights_own_range(self):
        _, b = run([("light", {"target": "Living Room", "action": "set",
                               "color_temp": "cool"})])
        calls = {tuple(sorted(c[2]["entity_id"])): c[2]["color_temp_kelvin"]
                 for c in b.service_calls if "color_temp_kelvin" in c[2]}
        # sofa (3000-4000) clamps 6000 to 4000; reading has no own limits
        self.assertEqual(calls[("light.sofa",)], 4000)
        self.assertEqual(calls[("light.reading",)], 6000)

    def test_colour_on_a_white_only_bulb_refused(self):
        r, b = run([("light", {"target": "Reading lamp", "action": "set",
                               "color": "red"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("cannot change colour", r[0]["error"])
        self.assertEqual(b.service_calls, [])

    def test_colour_word_and_rgb_accepted_on_colour_bulb(self):
        _, b = run([("light", {"target": "Sofa lamp", "action": "set",
                               "color": "warm white"}),
                    ("light", {"target": "Sofa lamp", "action": "set",
                               "color": [12, 200, 30]})])
        rgb = [c[2]["rgb_color"] for c in b.service_calls
               if "rgb_color" in c[2]]
        self.assertEqual(rgb, [[255, 197, 143], [12, 200, 30]])

    def test_toggle_uses_toggle_service(self):
        _, b = run([("light", {"target": "kitchen", "action": "toggle"})])
        self.assertEqual(b.service_calls[0][:2], ("light", "toggle"))

    def test_action_must_be_one_of_the_enum(self):
        r, _ = run([("light", {"target": "kitchen", "action": "sparkle"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("on, off, toggle, set, status", r[0]["error"])

    def test_status_reads_without_service_calls(self):
        r, b = run([("light", {"target": "Living Room", "action": "status"})])
        self.assertTrue(r[0]["ok"])
        self.assertIn("1 of 2 on", r[0]["text"])
        self.assertEqual(b.service_calls, [])

    def test_at_most_twelve_entities_per_call(self):
        big = {"Attic": {"lights": [("Bulb %d" % i, "light.b%d" % i)
                                    for i in range(13)], "climates": []}}
        r, _ = run([("light", {"target": "Attic", "action": "on"})],
                   exposed=big)
        self.assertFalse(r[0]["ok"])
        self.assertIn("only change 12 at once", r[0]["error"])


    def test_on_with_brightness_and_colour_rides_one_turn_on(self):
        # the review bug: brightness was dropped by a second bare turn_on
        _, b = run([("light", {"target": "Sofa lamp", "action": "on",
                               "brightness_pct": 60, "color": "warm white"})])
        self.assertEqual(len(b.service_calls), 1)
        domain, service, data = b.service_calls[0]
        self.assertEqual((domain, service), ("light", "turn_on"))
        self.assertEqual(data["entity_id"], ["light.sofa"])
        self.assertEqual(data["brightness"], int(round(60 * 255 / 100)))
        self.assertEqual(data["rgb_color"], [255, 197, 143])

    def test_service_style_argument_names_are_accepted(self):
        # a model trained on HA service data writes these names
        _, b = run([("light", {"target": "Bath bulb", "action": "set",
                               "color_temp_kelvin": 5000}),
                    ("climate", {"target": "Kitchen Radiator", "action": "set",
                                 "hvac_mode": "heat"})])
        kelvin = [c[2].get("color_temp_kelvin") for c in b.service_calls]
        self.assertIn(5000, kelvin)
        modes = [c[2].get("hvac_mode") for c in b.service_calls
                 if c[1] == "set_hvac_mode"]
        self.assertEqual(modes, ["heat"])

    def test_colour_word_only_needs_rgb_support(self):
        # a colour-temperature-only bulb takes warm/cool words, not red
        r, b = run([("light", {"target": "Bath bulb", "action": "set",
                               "color": "red"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("cannot change colour", r[0]["error"])
        r2, _ = run([("light", {"target": "Bath bulb", "action": "set",
                                "color_temp": "warm"})])
        self.assertTrue(r2[0]["ok"])

    def test_zero_brightness_means_off(self):
        _, b = run([("light", {"target": "kitchen", "action": "on",
                               "brightness_pct": 0})])
        self.assertEqual([(d, s) for d, s, _ in b.service_calls],
                         [("light", "turn_off")])

    def test_turn_budget_counts_the_whole_spoken_ask(self):
        # one HomeControl instance = one spoken ask: devices touched by any
        # call count against 12, and the fifth write call is refused outright
        fixture = {
            "Attic": {"lights": [("Bulb %d" % i, "light.b%d" % i)
                                 for i in range(12)], "climates": []},
            "Hall": {"lights": [("Hall %d" % i, "light.h%d" % i)
                                for i in range(5)], "climates": []},
        }

        async def go(control):
            out = []
            # Hall 0 is outside Attic: the second call alone would pass,
            # but together they reach for thirteen devices in one ask
            out.append(await control.execute("light", {"target": "Hall 0",
                                                       "action": "on"}))
            out.append(await control.execute("light", {"target": "Attic",
                                                       "action": "on"}))
            control.reset_budget()  # the person asked again in a new breath
            for i in range(5):      # same five devices: calls, not devices, run out
                out.append(await control.execute("light",
                                                 {"target": "Hall %d" % i,
                                                  "action": "on"}))
            return out

        control = homecontrol.HomeControl(FakeBackend(fixture))
        r = asyncio.run(go(control))
        self.assertTrue(r[0]["ok"])
        self.assertFalse(r[1]["ok"])
        self.assertIn("more than 12 devices", r[1]["error"])
        for call in r[2:6]:  # four write calls fit after the reset
            self.assertTrue(call["ok"], call)
        self.assertFalse(r[6]["ok"])   # the fifth write in one ask
        self.assertIn("changes per ask", r[6]["error"])

class ClimateTimerTest(unittest.TestCase):
    def test_setpoint_out_of_range_refused(self):
        r, b = run([("climate", {"target": "Kitchen Radiator", "action": "set",
                                 "temperature": 30})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("goes from 7 to 27", r[0]["error"])
        self.assertEqual(b.service_calls, [])

    def test_mode_must_come_from_the_entity(self):
        r, _ = run([("climate", {"target": "Kitchen Radiator", "action": "set",
                                 "mode": "eco"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("supports off, heat", r[0]["error"])

    def test_set_temperature_snaps_to_step(self):
        _, b = run([("climate", {"target": "Kitchen Radiator", "action": "set",
                                 "temperature": 21.3})])
        self.assertEqual(b.service_calls[0][2]["temperature"], 21.5)

    def test_status_reads_current_and_target(self):
        r, _ = run([("climate", {"target": "kitchen", "action": "status"})])
        self.assertIn("now 19", r[0]["text"])
        self.assertIn("set to 21", r[0]["text"])

    def test_timer_start_shapes_hass_intent(self):
        _, b = run([("timer", {"action": "start", "minutes": 5,
                               "label": "pasta"})])
        self.assertEqual(b.intent_calls, [("HassStartTimer",
                                           {"label": "pasta",
                                            "seconds": 300})])

    def test_timer_cancel_and_status_intents(self):
        _, b = run([("timer", {"action": "cancel"}),
                    ("timer", {"action": "status"})])
        self.assertEqual([name for name, _ in b.intent_calls],
                         ["HassCancelTimer", "HassTimerStatus"])

    def test_timer_minutes_bounds(self):
        r, _ = run([("timer", {"action": "start", "minutes": 500})])
        self.assertFalse(r[0]["ok"])
        r2, _ = run([("timer", {"action": "start"})])
        self.assertFalse(r2[0]["ok"])


class DryAndSchemaTest(unittest.TestCase):
    def test_dry_flag_changes_nothing_but_shows_the_plan(self):
        r, b = run([("light", {"target": "kitchen", "action": "on"}),
                    ("climate", {"target": "Kitchen Radiator", "action": "set",
                                 "temperature": 22}),
                    ("timer", {"action": "start", "minutes": 2})], dry=True)
        for out in r:
            self.assertTrue(out["ok"])
            self.assertTrue(out.get("dry"))
            self.assertIn("would_call", out)
        self.assertEqual(b.service_calls, [])
        self.assertEqual(b.intent_calls, [])
        self.assertEqual(r[0]["would_call"], [{"service": "light.turn_on",
                                               "data": {"entity_id":
                                                        ["light.kitchen"]}}])

    def test_home_state_reads_a_room_without_services(self):
        r, b = run([("home_state", {"target": "Living Room"})])
        self.assertTrue(r[0]["ok"])
        self.assertIn("1 lights on", r[0]["text"])
        self.assertIn("21", r[0]["text"])
        self.assertEqual(b.service_calls, [])

    def test_home_state_unknown_room_lists_areas(self):
        r, _ = run([("home_state", {"target": "Cellar"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("Kitchen", r[0]["candidates"])

    def test_schema_matches_golden_and_stays_small(self):
        with open(GOLDEN_SCHEMA, encoding="utf-8") as fh:
            golden = json.load(fh)
        self.assertEqual(golden, json.loads(json.dumps(homecontrol.SCHEMAS)))
        compact = json.dumps(homecontrol.SCHEMAS, separators=(",", ":"))
        self.assertLess(max(1, len(compact) // 4), 450)
        # the review's tight proxy: tool definitions alone must stay small
        self.assertLess(len(compact) // 3, 550)

    def test_ask_helm_is_not_this_modules_business(self):
        r, b = run([("ask_helm", {"text": "anything"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("unknown tool", r[0]["error"])
        self.assertEqual(b.service_calls + b.intent_calls, [])


if __name__ == "__main__":
    unittest.main()
