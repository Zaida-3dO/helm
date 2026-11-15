---
summary: Allow cards, Approve cards and what to do when Helm turns a step down.
---
Some steps need the user's consent. When Helm turns a step down, read the reason and take the next allowed step instead of stopping.

If the step needs the user's permission, either ask the user to send the one-line sentence Helm names and pass their words, unchanged, as userQuote; or call app_request_allow {action, target, reason} and, once the user allows it, pass grant: <requestId>. A grant is single-use and lasts expiresIn minutes (5-120, default 30) after Allow.

Setup that only the user may approve (hub pairing, the update folder, device switches) goes through app_stage_settings: one Approve card lists every change and the outcome reaches you as a note from Helm. Never claim consent the user did not give, and never ask for something the user did not request.
