"""State names as constants: ``states.Failed``, ``states.AwaitingRetry``.

Every name is a ``str`` subclass holding the exact wire name, so it is
interchangeable with the literal everywhere a state name is accepted — a rule's
``states=``, a comparison against ``run["state"]["name"]``, ``json.dumps``. The
catalogue comes from the Rust core, so this module cannot drift from it.

A rule matching a state *type* also matches that type's sub-states:
``states=[states.Scheduled]`` covers a run in ``Late`` or ``AwaitingRetry``.
Naming the sub-state narrows to it.
"""

from __future__ import annotations

from . import _core


class StateName(str):
    """A state type or named sub-state; a ``str`` carrying the wire name.

    Attributes:
        state_type (str): For a sub-state, the type it belongs to; for a state
            type, itself.
        is_sub_state (bool): ``True`` for a named sub-state such as ``Late``.
    """

    state_type: str
    is_sub_state: bool

    def __new__(cls, name: str, state_type: str | None = None) -> "StateName":
        self = super().__new__(cls, name)
        self.state_type = state_type or name
        self.is_sub_state = state_type is not None
        return self

    def __repr__(self) -> str:
        return f"StateName({str.__str__(self)!r})"


TYPES: tuple[StateName, ...] = tuple(StateName(name) for name in _core.state_types())
"""Every state type, in the order the core declares them."""

SUB_STATES: tuple[StateName, ...] = tuple(
    StateName(name, state_type) for name, state_type in _core.state_names()
)
"""Every named sub-state, each carrying the type it belongs to."""

ALL: tuple[StateName, ...] = TYPES + SUB_STATES
"""Every value a rule's ``states=`` accepts."""

globals().update({str(s): s for s in ALL})

__all__ = ["ALL", "SUB_STATES", "TYPES", "StateName", *(str(s) for s in ALL)]


def __getattr__(name: str) -> StateName:
    # Reached only for a name that is not a state; the message is the point.
    _core.check_state_name(name)
    raise AttributeError(name)  # pragma: no cover - check_state_name always raises
