"""One shared error vocabulary for every HTTP transport in this component.

agent_core, hubapi and the aiohttp transport must raise and catch the SAME
classes - three private definitions drifted once and a refused model port
then crashed the turn instead of speaking the offline sentence. Stdlib only,
so both pure modules keep importing it.
"""


class TransportError(Exception):
    """The request never reached its answer (DNS, refused, connection reset)."""


class RequestTimeout(TransportError):
    """The connection was fine; the answer just did not arrive in time.

    Kept distinct so a slow model is spoken and recorded as a timeout, not
    mistaken for an offline endpoint."""
