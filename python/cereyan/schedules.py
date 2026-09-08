"""Schedule declarations for ``@flow(schedule=...)``."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Any

CATCHUP_POLICIES = ("skip", "latest", "all")


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
            "key": self.key,
        }


@dataclass(frozen=True)
class RRule:
    """An iCalendar recurrence rule set; must include DTSTART."""

    rrule: str
    timezone: str | None = None
    catchup: str = "skip"
    catchup_max: int = 100
    key: str | None = None

    def to_json(self) -> dict[str, Any]:
        """The schedule as the server stores it."""
        return {
            "kind": "rrule",
            "rrule": self.rrule,
            "timezone": self.timezone,
            "catchup": _check_catchup(self.catchup),
            "catchup_max": int(self.catchup_max),
            "key": self.key,
        }


Schedule = Cron | Interval | RRule


def normalize(value: Any) -> list[dict[str, Any]]:
    """Accept a schedule, a list of schedules, or None."""
    if value is None:
        return []
    items = value if isinstance(value, (list, tuple)) else [value]
    out = []
    for i, item in enumerate(items):
        if not isinstance(item, (Cron, Interval, RRule)):
            raise TypeError(f"schedule must be Cron, Interval, or RRule, got {type(item).__name__}")
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
