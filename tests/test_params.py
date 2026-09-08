import dataclasses
import enum
from datetime import date, datetime, timedelta
from typing import Literal, Optional

import pytest

from cereyan import ParameterError, flow
from cereyan.params import coerce, json_schema, parameter_specs


class Color(enum.Enum):
    RED = "red"
    BLUE = "blue"


@dataclasses.dataclass
class Window:
    start: date
    days: int = 1


def test_string_coerced_to_date():
    assert coerce("2026-09-06", date, "day") == date(2026, 9, 6)
    assert coerce(datetime(2026, 9, 6, 12), date, "day") == date(2026, 9, 6)


def test_scalars():
    assert coerce("3", int, "n") == 3
    assert coerce(3.0, int, "n") == 3
    assert coerce("2.5", float, "x") == 2.5
    assert coerce(2, float, "x") == 2.0
    assert coerce("yes", bool, "b") is True
    assert coerce("off", bool, "b") is False
    assert coerce(1, str, "s") == "1"
    assert coerce("2026-09-06T10:00:00", datetime, "t") == datetime(2026, 9, 6, 10)
    assert coerce("90", timedelta, "d") == timedelta(seconds=90)
    assert coerce("1:30:00", timedelta, "d") == timedelta(hours=1, minutes=30)
    assert coerce(5, timedelta, "d") == timedelta(seconds=5)


@pytest.mark.parametrize(
    "value, hint",
    [("abc", int), ("abc", float), ("maybe", bool), (True, int), ("nope", date), ([], str)],
)
def test_invalid_values_rejected(value, hint):
    with pytest.raises(ParameterError) as info:
        coerce(value, hint, "n")
    assert "n" in str(info.value)
    assert info.value.name == "n"


def test_optional_list_dict_literal_enum_dataclass():
    assert coerce(None, Optional[int], "n") is None
    assert coerce("none", int | None, "n") is None
    assert coerce("4", Optional[int], "n") == 4
    assert coerce("[1, \"2\"]", list[int], "xs") == [1, 2]
    assert coerce(["2026-01-01"], list[date], "ds") == [date(2026, 1, 1)]
    assert coerce('{"a": "1"}', dict[str, int], "m") == {"a": 1}
    assert coerce("b", Literal["a", "b"], "l") == "b"
    assert coerce("3", Literal[1, 3], "l") == 3
    with pytest.raises(ParameterError):
        coerce("c", Literal["a", "b"], "l")
    assert coerce("red", Color, "c") is Color.RED
    assert coerce("BLUE", Color, "c") is Color.BLUE
    with pytest.raises(ParameterError):
        coerce("green", Color, "c")
    w = coerce({"start": "2026-01-01", "days": "3"}, Window, "w")
    assert w == Window(date(2026, 1, 1), 3)
    assert coerce('{"start": "2026-01-01"}', Window, "w") == Window(date(2026, 1, 1))


def test_unsupported_hint_passes_through():
    class Opaque:
        pass

    o = Opaque()
    assert coerce(o, Opaque, "x") is o
    assert coerce("raw", Opaque, "x") == "raw"

    @flow
    def f(x: Opaque, y):
        return x, y

    assert f.schema["properties"]["x"] == {}
    assert f.schema["properties"]["y"] == {}


def test_schema_reflects_defaults_and_required():
    def f(x: int, y: str = "a", d: date = date(2026, 1, 1), c: Color = Color.RED):
        pass

    schema = json_schema(parameter_specs(f))
    assert schema["required"] == ["x"]
    assert schema["properties"]["x"] == {"type": "integer"}
    assert schema["properties"]["y"] == {"type": "string", "default": "a"}
    assert schema["properties"]["d"] == {"type": "string", "format": "date", "default": "2026-01-01"}
    assert schema["properties"]["c"]["enum"] == ["red", "blue"]
    assert schema["properties"]["c"]["default"] == "red"


def test_flow_coerce_rejects_before_running(store):
    calls = []

    @flow
    def f(n: int):
        calls.append(n)

    with pytest.raises(ParameterError) as info:
        f(n="abc")
    assert "n" in str(info.value) and "int" in str(info.value)
    assert calls == []
    assert store.list_runs()  # store reachable
    import json

    assert json.loads(store.list_runs())["items"] == []
