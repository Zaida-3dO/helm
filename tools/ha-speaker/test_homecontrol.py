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
    "Bedroom": {"lights": [("Bedroom Lights", "light.br_main"),
                           ("Bed light", "light.bed"),
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
    "light.br_main": {"state": "on", "attributes": {
        "friendly_name": "Bedroom Lights",
        "supported_color_modes": ["color_temp", "xy"],
        "min_color_temp_kelvin": 2000, "max_color_temp_kelvin": 6500}},
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
    def __init__(self, exposed=None, states=None):
        self.exposed_map = EXPOSED if exposed is None else exposed
        self.states_map = STATES if states is None else states
        self.service_calls = []
        self.intent_calls = []

    async def exposed(self):
        return self.exposed_map

    async def states(self, entity_ids):
        return {eid: self.states_map[eid] for eid in entity_ids
                if eid in self.states_map}

    async def call_service(self, domain, service, data):
        self.service_calls.append((domain, service, json.loads(
            json.dumps(data))))  # copy, like crossing a real boundary

    async def intent(self, name, data):
        self.intent_calls.append((name, dict(data)))
        return {"text": "One timer left."}


def run(args_list, exposed=None, dry=False, states=None):
    backend = FakeBackend(exposed, states)
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

    def test_room_word_with_lights_beats_same_named_bulb(self):
        # The real-world case: "the home office lights" must mean EVERY light
        # of the room in ONE call, even though one bulb is named after it.
        for target in ("the bedroom lights", "bedroom lights",
                       "all the bedroom lights"):
            with self.subTest(target):
                r, b = run([("light", {"target": target, "action": "off"})])
                self.assertTrue(r[0]["ok"], r)
                self.assertEqual(len(b.service_calls), 1)
                self.assertEqual(sorted(b.service_calls[0][2]["entity_id"]),
                                 ["light.bed", "light.br_main",
                                  "light.ctlamp", "light.pendant"])

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

    # the review house: real lamps are named after rooms that are also areas.
    # A singular lamp/light/bulb must keep the exact name; only plural room
    # words mean "the whole area".
    NAMED_HOUSE = {
        "Home office": {"lights": [("Office lamp", "light.officelamp"),
                                   ("Floor lamp", "light.floorlamp")],
                        "climates": []},
        "Office": {"lights": [("Desk light", "light.desk1")], "climates": []},
        "Bedroom": {"lights": [("Bedroom lamp", "light.brlamp"),
                               ("Ceiling", "light.brceil")], "climates": []},
        "Hall": {"lights": [("Hall light", "light.hall1"),
                            ("Hall extra", "light.hall2")], "climates": []},
    }

    def test_singular_kind_word_keeps_the_exact_lamp(self):
        # was: trimming "office lamp" to the area word hit a different room's
        # Desk light; "bedroom lamp" switched every bedroom light
        table = [
            ("office lamp", ["light.officelamp"]),
            ("the office lamp", ["light.officelamp"]),
            ("bedroom lamp", ["light.brlamp"]),
            ("hall light", ["light.hall1"]),
        ]
        for target, want in table:
            with self.subTest(target):
                r, b = run([("light", {"target": target,
                                       "action": "toggle"})],
                           exposed=self.NAMED_HOUSE)
                self.assertTrue(r[0]["ok"], r[0])
                self.assertEqual([c[2]["entity_id"] for c in b.service_calls],
                                 [want])

    def test_plural_room_word_still_moves_the_whole_area(self):
        table = [
            ("home office lights", ["light.floorlamp", "light.officelamp"]),
            ("office lamps", ["light.desk1"]),
            ("the hall lights", ["light.hall1", "light.hall2"]),
        ]
        for target, want in table:
            with self.subTest(target):
                r, b = run([("light", {"target": target,
                                       "action": "toggle"})],
                           exposed=self.NAMED_HOUSE)
                self.assertTrue(r[0]["ok"], r[0])
                self.assertEqual(
                    sorted(b.service_calls[0][2]["entity_id"]), want)


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

    def test_kelvin_clamped_and_a_non_tunable_bulb_gets_a_plain_note(self):
        # was: a bare default range made the brightness-only bulb take the
        # setting. Now it is excluded with an honest per-light note while the
        # tunable ones move (partial result, ok=True - not an opaque failure).
        r, b = run([("light", {"target": "Living Room lights", "action": "set",
                               "color_temp": "cool"})])
        self.assertTrue(r[0]["ok"])
        calls = {tuple(sorted(c[2]["entity_id"])): c[2].get("color_temp_kelvin")
                 for c in b.service_calls}
        # sofa clamps 6000 to its own max; reading never gets the call at all
        self.assertEqual(calls, {("light.sofa",): 4000})
        self.assertIn("Reading lamp cannot set the colour temperature",
                      r[0]["text"])
        self.assertEqual(r[0]["notes"],
                         ["Reading lamp cannot set the colour temperature"])

    def test_partial_colour_only_the_capable_lights_move(self):
        r, b = run([("light", {"target": "Living Room lights", "action": "set",
                               "color": "red"})])
        self.assertTrue(r[0]["ok"])
        self.assertEqual([(d, s, c["entity_id"]) for d, s, c in b.service_calls],
                         [("light", "turn_on", ["light.sofa"])])
        self.assertIn("Reading lamp cannot change colour", r[0]["text"])
        self.assertEqual(r[0]["notes"],
                         ["Reading lamp cannot change colour"])

    def test_turn_on_with_colour_still_carries_the_white_only_bulb(self):
        # "turn on to red": the state part applies to every light, the colour
        # note only lists those that cannot do it
        r, b = run([("light", {"target": "Living Room lights", "action": "on",
                               "color": "red"})])
        self.assertTrue(r[0]["ok"])
        self.assertEqual(len(b.service_calls), 2)  # two parameter groups
        withrgb = [c for c in (x[2] for x in b.service_calls)
                   if "rgb_color" in c]
        plain = [c for c in (x[2] for x in b.service_calls)
                 if "rgb_color" not in c]
        self.assertEqual(withrgb, [{"entity_id": ["light.sofa"],
                                    "rgb_color": [255, 0, 0]}])
        self.assertEqual(plain, [{"entity_id": ["light.reading"]}])
        self.assertIn("Reading lamp cannot change colour", r[0]["text"])

    def test_room_target_moves_every_light_in_one_call(self):
        _, b = run([("light", {"target": "Living Room lights", "action": "on"})])
        self.assertEqual(b.service_calls, [("light", "turn_on",
                                            {"entity_id": ["light.sofa",
                                                           "light.reading"]})])

    def test_colour_on_a_white_only_bulb_refused(self):
        r, b = run([("light", {"target": "Reading lamp", "action": "set",
                               "color": "red"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("cannot change colour", r[0]["error"])
        self.assertEqual(b.service_calls, [])

    def test_colour_temperature_emulates_on_xy_hs_bulbs(self):
        # HA's light.turn_on emulates colour temperature on xy, hs and rgb
        # bulbs; only fixed-white, brightness-only and on/off bulbs cannot.
        # was: the xy and hs bulbs were refused the warm setting outright.
        fixture = {"Den": {"lights": [("Xy bulb", "light.xy"),
                                      ("Hs bulb", "light.hs"),
                                      ("White only", "light.whiteo")],
                           "climates": []}}
        states = {
            "light.xy": {"state": "on", "attributes": {
                "friendly_name": "Xy bulb",
                "supported_color_modes": ["xy"]}},
            "light.hs": {"state": "on", "attributes": {
                "friendly_name": "Hs bulb",
                "supported_color_modes": ["hs"]}},
            "light.whiteo": {"state": "on", "attributes": {
                "friendly_name": "White only",
                "supported_color_modes": ["white"]}},
        }
        r, b = run([("light", {"target": "Den lights", "action": "set",
                               "color_temp": "warm"})],
                   exposed=fixture, states=states)
        self.assertTrue(r[0]["ok"], r[0])
        self.assertEqual([(d, s, c["entity_id"]) for d, s, c in b.service_calls],
                         [("light", "turn_on", ["light.xy", "light.hs"])])
        self.assertEqual(b.service_calls[0][2]["color_temp_kelvin"], 2700)
        self.assertEqual(r[0]["notes"],
                         ["White only cannot set the colour temperature"])

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
            for i in range(7):      # calls, not devices, run out eventually
                out.append(await control.execute(
                    "light", {"target": "Hall %d" % (i % 5), "action": "on"}))
            return out

        control = homecontrol.HomeControl(FakeBackend(fixture))
        r = asyncio.run(go(control))
        self.assertTrue(r[0]["ok"])
        self.assertFalse(r[1]["ok"])
        self.assertIn("more than 12 devices", r[1]["error"])
        for call in r[2:8]:  # six write calls fit after the reset (a room is
            self.assertTrue(call["ok"], call)   # one, so this is real slack)
        self.assertFalse(r[8]["ok"])   # the seventh write in one ask
        self.assertIn("changes per ask", r[8]["error"])

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
        # Measured at five tools (1690 chars): home_status is a new honest
        # noun, not prose drift - the ceiling follows the measurement by the
        # same ~7% rule it has always had, so sloppy wording still trips it.
        self.assertLess(max(1, len(compact) // 4), 450)   # measured 422
        # the review's tight proxy: tool definitions alone must stay small
        self.assertLess(len(compact) // 3, 600)           # measured 563

    def test_the_speaker_only_ever_gets_these_five_tools(self):
        # the spoken side stays read/write bounded by design: house control,
        # timers, two room/list reads. No locks, vacuums or other new write
        # tools may appear here without amending this pin on purpose.
        self.assertEqual([fn["function"]["name"] for fn in homecontrol.SCHEMAS],
                         ["light", "climate", "timer", "home_state",
                          "home_status"])

    def test_ask_helm_is_not_this_modules_business(self):
        r, b = run([("ask_helm", {"text": "anything"})])
        self.assertFalse(r[0]["ok"])
        self.assertIn("unknown tool", r[0]["error"])
        self.assertEqual(b.service_calls + b.intent_calls, [])


class HomeStatusTest(unittest.TestCase):
    """The spoken chores/plants check-in: the pure formatter over real feed
    shapes (fetched with status=all, so this half does the picking), and the
    dispatch where a read must cost nothing and every failure still speaks."""

    def test_formatter_table(self):
        cases = [
            # overdue first; chores only count when due within a week
            ("chores", {"chores": [
                {"name": "Bins", "overdue": True, "hours_until_due": -3.0},
                {"name": "Vacuum", "overdue": False, "hours_until_due": 20.0},
                {"name": "Paint the shed", "overdue": False,
                 "hours_until_due": 900.0}]},
             "2 chores due: Bins, Vacuum."),
            ("chores", {"chores": [
                {"name": "Soon", "overdue": False, "hours_until_due": 5.0},
                {"name": "Overgrown", "overdue": True, "hours_until_due": -9.0}]},
             "2 chores due: Overgrown, Soon."),   # order survives the payload
            ("chores", {"chores": [
                {"name": "C", "overdue": False, "hours_until_due": 60.0},
                {"name": "A", "overdue": False, "hours_until_due": 10.0},
                {"name": "B", "overdue": False, "hours_until_due": 30.0}]},
             "3 chores due: A, B, C."),           # soonest first, not arrival
            ("chores", {"chores": [
                {"name": "Bins", "overdue": True, "hours_until_due": -1.0}]},
             "1 chore due: Bins."),
            ("chores", {"chores": [
                {"name": n, "overdue": True, "hours_until_due": -1.0}
                for n in "ABCDE"]},
             "5 chores due: A, B, C (+2 more)."),  # cap three, honest total
            ("chores", {"chores": [
                {"name": "Far away", "overdue": False,
                 "hours_until_due": 900.0}]},
             "No chores are due right now."),
            ("chores", {"chores": []}, "No chores are due right now."),
            # junk entries are skipped, never spoken: an unnamed or unreadable
            # row, a bare string, and negative hours without the overdue flag
            ("chores", {"chores": [
                "junk", {"overdue": True}, {"name": "  ", "overdue": True},
                {"name": "Ghost", "overdue": False, "hours_until_due": -4.0},
                {"name": "Bins", "overdue": True}]},
             "1 chore due: Bins."),
            # a payload that is not the promised shape says out-of-reach, so
            # we never promise an empty house when the answer is unknown
            ("chores", {}, "I can't reach the chore list right now."),
            ("plants", {"moisture_error": "the sensor farm failed"},
             "I can't reach the plant list right now."),
            # plants: only overdue (thirsty) ones get named
            ("plants", {"plants": [
                {"name": "Monstera", "overdue": True, "hours_until_due": -2.0},
                {"name": "Fern", "overdue": True},
                {"name": "Cactus", "overdue": False, "hours_until_due": 3.0}]},
             "2 plants need water: Monstera, Fern."),
            ("plants", {"plants": [{"name": "Fern", "overdue": True}]},
             "1 plant needs water: Fern."),
            ("plants", {"plants": [{"name": "Cactus", "overdue": False}]},
             "No plants need water right now."),
            # the two honest failure sentences, per kind
            ("chores", None, "I can't reach the chore list right now."),
            ("plants", None, "I can't reach the plant list right now."),
            ("chores", homecontrol.HOUSE_NOT_SET_UP,
             "Chores are not set up on this speaker yet."),
            ("plants", dict(homecontrol.HOUSE_NOT_SET_UP),
             "Plant care is not set up on this speaker yet."),
        ]
        for kind, payload, want in cases:
            self.assertEqual(homecontrol.format_home_status(kind, payload),
                             want, "%s / %r" % (kind, payload))

    def test_reads_are_free_and_never_write(self):
        # three check-ins plus two changes fit in one ask: home_status never
        # touches the device/write budget and posts nothing
        backend = HouseFeedBackend(
            {"chores": [{"name": "Bins", "overdue": True}]})
        control = homecontrol.HomeControl(backend)

        async def go():
            out = []
            for _ in range(3):
                out.append(await control.execute("home_status",
                                                 {"kind": "chores"}))
            out.append(await control.execute("light", {"target": "kitchen",
                                                       "action": "on"}))
            out.append(await control.execute("light", {"target": "kitchen",
                                                       "action": "off"}))
            return out

        r = asyncio.run(go())
        self.assertTrue(all(x["ok"] for x in r), r)
        self.assertEqual(r[0]["text"], "1 chore due: Bins.")
        self.assertEqual(backend.reads, ["chores"] * 3)
        self.assertEqual([(d, s) for d, s, _ in backend.service_calls],
                         [("light", "turn_on"), ("light", "turn_off")])

    def test_feed_down_not_set_up_and_no_backend_all_speak_plainly(self):
        # every outcome comes back ok=True with a spoken text: never an error
        r, _ = run([("home_status", {"kind": "plants"})],
                   exposed=None)  # plain FakeBackend has no house_read at all
        self.assertTrue(r[0]["ok"])
        self.assertEqual(r[0]["text"],
                         "Plant care is not set up on this speaker yet.")

        async def one(backend):
            return await homecontrol.HomeControl(backend).execute(
                "home_status", {"kind": "chores"})

        down = asyncio.run(one(HouseFeedBackend(None)))
        self.assertTrue(down["ok"])
        self.assertEqual(down["text"],
                         "I can't reach the chore list right now.")
        unset = asyncio.run(one(HouseFeedBackend(
            dict(homecontrol.HOUSE_NOT_SET_UP))))
        self.assertEqual(unset["text"],
                         "Chores are not set up on this speaker yet.")

    def test_kind_is_validated_before_any_fetch(self):
        backend = HouseFeedBackend({"chores": []})
        r = asyncio.run(_execute(backend, "home_status", {"kind": "bins"}))
        self.assertFalse(r[0]["ok"])
        self.assertIn("chores or plants", r[0]["error"])
        self.assertEqual(backend.reads, [])


async def _execute(backend, name, args):
    control = homecontrol.HomeControl(backend)
    return [await control.execute(name, args)]


class RoomTurnEndToEnd(unittest.TestCase):
    """The failing sentence from the first real speaker run, wired straight
    through: a scripted model turn
    driving the real HomeControl against the fake house. "Turn on the bedroom
    lights to red." is ONE call for the whole room; the bulbs that cannot do
    red are not touched and come back as plain notes in the tool result, so
    the spoken answer can tell what worked and what could not."""

    def test_room_red_is_one_call_with_honest_notes(self):
        import agent_core

        class ScriptTransport:
            def __init__(self):
                self.seen = []

            async def request(self, method, url, headers=None, data=None,
                              timeout=None):
                body = json.loads(data.decode("utf-8"))
                self.seen.append(body)
                if len(self.seen) == 1:
                    msg = {"content": None, "tool_calls": [{
                        "id": "c0", "type": "function",
                        "function": {
                            "name": "light",
                            "arguments": json.dumps(
                                {"target": "the bedroom lights",
                                 "action": "set", "color": "red"})}}]}
                else:
                    msg = {"content": ("Bedroom is red on the main light;"
                                       " three bulbs there cannot change"
                                       " colour.")}
                payload = json.dumps({"choices": [{"message": msg,
                                                   "finish_reason": "stop"}]})
                return 200, payload.encode("utf-8")

        backend = FakeBackend()
        control = homecontrol.HomeControl(backend)
        cfg = agent_core.Config(model_url="http://main/x", model_name="m")
        transport = ScriptTransport()
        result = asyncio.run(agent_core.run_turn(
            cfg, transport, control, {}, "Turn on the bedroom lights to red.",
            clock=lambda: 0.0))

        self.assertEqual(result.kind, agent_core.KIND_ANSWER)
        # ONE service call for the whole room - not one per bulb
        self.assertEqual(len(backend.service_calls), 1)
        domain, service, data = backend.service_calls[0]
        self.assertEqual((domain, service), ("light", "turn_on"))
        self.assertEqual(data["entity_id"], ["light.br_main"])
        self.assertEqual(data["rgb_color"], [255, 0, 0])
        # the notes reached the model, so its sentence can be honest
        tool_msgs = [m for m in transport.seen[1]["messages"]
                     if m["role"] == "tool"]
        fed = json.dumps(tool_msgs)
        for bulb in ("Bed light", "Desk light", "Bath bulb"):
            self.assertIn("%s cannot change colour" % bulb, fed)
        self.assertEqual(result.tool_calls[0]["ok"], True)
        # and the gate let it through: a tool really did carry the turn
        self.assertFalse(result.guard_reprompt or result.guard_blocked)


class HouseFeedBackend(FakeBackend):
    """The fake house plus a scripted chores/plants feed read."""

    def __init__(self, payload):
        FakeBackend.__init__(self)
        self.payload = payload
        self.reads = []

    async def house_read(self, kind):
        self.reads.append(kind)
        return self.payload


if __name__ == "__main__":
    unittest.main()
