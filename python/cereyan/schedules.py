"""Schedule declarations for ``@flow(schedule=...)``."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Any

CATCHUP_POLICIES = ("skip", "latest", "all")


def _policy_json(catchup_window: int | None, jitter: int, start_deadline: int | None) -> dict[str, Any]:
    """The catch-up window, jitter, and start deadline as the server stores them; negatives are refused."""
    out: dict[str, Any] = {}
    for name, value in (("catchup_window", catchup_window), ("jitter", jitter), ("start_deadline", start_deadline)):
        if value is None:
            continue
        seconds = int(value)
        if seconds < 0:
            raise ValueError(f"{name} must be zero or more seconds")
        out[name] = seconds
    return out


def _check_catchup(value: str) -> str:
    if value not in CATCHUP_POLICIES:
        raise ValueError(f"catchup must be one of {CATCHUP_POLICIES}, got {value!r}")
    return value


@dataclass(frozen=True)
class Cron:
    """A cron schedule evaluated by wall clock in ``timezone``."""

    cron: str
    timezone: str | None = None
    day_or: bool = True
    catchup: str = "skip"
    catchup_max: int = 100
    catchup_window: int | None = None
    jitter: int = 0
    start_deadline: int | None = None
    key: str | None = None

    def to_json(self) -> dict[str, Any]:
        """The schedule as the server stores it."""
        return {
            "kind": "cron",
            "cron": self.cron,
            "timezone": self.timezone,
            "day_or": self.day_or,
            "catchup": _check_catchup(self.catchup),
            "catchup_max": int(self.catchup_max),
            **_policy_json(self.catchup_window, self.jitter, self.start_deadline),
            "key": self.key,
        }


@dataclass(frozen=True)
class Interval:
    """Fire every ``interval`` (seconds or timedelta) from ``anchor``.

    Intervals under a day are elapsed time; longer intervals keep their local
    wall-clock time across DST changes.
    """

    interval: float | timedelta
    anchor: datetime | None = None
    timezone: str | None = None
    catchup: str = "skip"
    catchup_max: int = 100
    catchup_window: int | None = None
    jitter: int = 0
    start_deadline: int | None = None
    key: str | None = None

    def to_json(self) -> dict[str, Any]:
        """The schedule as the server stores it; raises ``ValueError`` for a non-positive interval."""
        seconds = self.interval.total_seconds() if isinstance(self.interval, timedelta) else float(self.interval)
        if seconds <= 0:
            raise ValueError("interval must be positive")
        anchor = None
        if self.anchor is not None:
            a = self.anchor if self.anchor.tzinfo else self.anchor.replace(tzinfo=timezone.utc)
            anchor = int(a.timestamp() * 1_000_000)
        return {
            "kind": "interval",
            "interval": seconds,
            "anchor": anchor,
            "timezone": self.timezone,
            "catchup": _check_catchup(self.catchup),
            "catchup_max": int(self.catchup_max),
            **_policy_json(self.catchup_window, self.jitter, self.start_deadline),
            "key": self.key,
        }


@dataclass(frozen=True)
class RRule:
    """An iCalendar recurrence rule set; must include DTSTART."""

    rrule: str
    timezone: str | None = None
    catchup: str = "skip"
    catchup_max: int = 100
    catchup_window: int | None = None
    jitter: int = 0
    start_deadline: int | None = None
    key: str | None = None

    def to_json(self) -> dict[str, Any]:
        """The schedule as the server stores it."""
        return {
            "kind": "rrule",
            "rrule": self.rrule,
            "timezone": self.timezone,
            "catchup": _check_catchup(self.catchup),
            "catchup_max": int(self.catchup_max),
            **_policy_json(self.catchup_window, self.jitter, self.start_deadline),
            "key": self.key,
        }


_UNITS = {"d": 86_400, "h": 3_600, "m": 60, "s": 1}


def duration_seconds(value: float | int | timedelta | str) -> float:
    """Seconds in a number, a ``timedelta``, or a string such as ``"30m"``, ``"1h30m"`` or ``"45s"``.

    Raises ``ValueError`` for a negative value or a string it cannot read.
    """
    if isinstance(value, timedelta):
        seconds = value.total_seconds()
    elif isinstance(value, str):
        text = value.strip().lower()
        seconds, number = 0.0, ""
        for ch in text:
            if ch.isdigit() or ch == ".":
                number += ch
            elif ch in _UNITS and number:
                seconds += float(number) * _UNITS[ch]
                number = ""
            elif not ch.isspace():
                raise ValueError(f"cannot read the duration {value!r}; write it like '30m' or '1h30m'")
        if number:
            if text.replace(".", "", 1).isdigit():
                seconds = float(number)
            else:
                raise ValueError(f"cannot read the duration {value!r}; give every number a unit (d, h, m, s)")
        if not text:
            raise ValueError("the duration is empty")
    else:
        seconds = float(value)
    if seconds < 0:
        raise ValueError("the duration must be zero or more")
    return seconds


@dataclass(frozen=True)
class Continuous:
    """Run again ``delay`` after the last run ends, once a processor is free.

    ``delay`` is seconds, a ``timedelta``, or a string such as ``"30m"``. At
    most one run of the schedule is ever waiting, in line, or running; the wait
    holds no processor. A continuous schedule has no fire times, so it takes no
    catch-up options. Pause the schedule to stop the loop; ``disable_after`` on
    the flow stops it after repeated failures and resumes it later.
    """

    delay: float | int | timedelta | str = 0
    jitter: int = 0
    start_deadline: int | None = None
    key: str | None = None

    def to_json(self) -> dict[str, Any]:
        """The schedule as the server stores it; raises ``ValueError`` for a negative or unreadable delay."""
        return {
            "kind": "continuous",
            "delay": duration_seconds(self.delay),
            **_policy_json(None, self.jitter, self.start_deadline),
            "key": self.key,
        }


Schedule = Cron | Interval | RRule | Continuous


def normalize(value: Any) -> list[dict[str, Any]]:
    """Accept a schedule, a list of schedules, or None."""
    if value is None:
        return []
    items = value if isinstance(value, (list, tuple)) else [value]
    out = []
    for i, item in enumerate(items):
        if not isinstance(item, (Cron, Interval, RRule, Continuous)):
            raise TypeError(f"schedule must be Cron, Interval, RRule, or Continuous, got {type(item).__name__}")
        data = item.to_json()
        if data.get("key") is None:
            data["key"] = f"code-{i}"
        out.append(data)
    return out


@dataclass
class exponential:
    """Retry delay growing as ``base * 2**attempt`` with optional jitter."""

    base: float = 1.0
    jitter: float = 0.0
    maximum: float = 3600.0

    def delay(self, attempt: int) -> float:
        """Seconds to wait before retry number ``attempt`` (0-based), capped at ``maximum``."""
        import random

        d = min(self.base * (2**attempt), self.maximum)
        if self.jitter:
            d += random.uniform(0, self.jitter)
        return d


RetryDelay = float | int | list | tuple | exponential | None


def retry_delay_for(spec: Any, attempt: int) -> float:
    """Seconds to wait before retry number ``attempt`` (0-based)."""
    if spec is None:
        return 0.0
    if isinstance(spec, exponential):
        return spec.delay(attempt)
    if isinstance(spec, (list, tuple)):
        if not spec:
            return 0.0
        return float(spec[min(attempt, len(spec) - 1)])
    if isinstance(spec, timedelta):
        return spec.total_seconds()
    return float(spec)


field  # re-exported for dataclass users
