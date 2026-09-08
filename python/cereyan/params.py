"""Parameter coercion from type hints and JSON schema generation.

Supported hints: str, int, float, bool, date, datetime, timedelta, Optional[T],
list[T], dict[str, T], Literal, Enum, and dataclasses. Anything else is opaque:
the value passes through unchanged and the schema describes it as untyped.
"""

from __future__ import annotations

import dataclasses
import enum
import inspect
import json
import types
import typing
from dataclasses import dataclass
from datetime import date, datetime, timedelta
from typing import Any, Callable, get_args, get_origin, get_type_hints

from .exceptions import ParameterError

_MISSING = inspect.Parameter.empty
_TRUE = {"true", "1", "yes", "on", "y", "t"}
_FALSE = {"false", "0", "no", "off", "n", "f"}


@dataclass(frozen=True)
class ParamSpec:
    name: str
    hint: Any
    default: Any
    kind: inspect._ParameterKind

    @property
    def required(self) -> bool:
        return self.default is _MISSING


def parameter_specs(fn: Callable) -> list[ParamSpec]:
    sig = inspect.signature(fn)
    try:
        hints = get_type_hints(fn)
    except Exception:  # forward refs to unimportable names: fall back to raw annotations
        hints = {}
    specs = []
    for p in sig.parameters.values():
        if p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD):
            continue
        hint = hints.get(p.name, p.annotation)
        if hint is _MISSING:
            hint = Any
        specs.append(ParamSpec(p.name, hint, p.default, p.kind))
    return specs


# ---------------------------------------------------------------------------
# hint classification


def _is_optional(hint: Any) -> tuple[bool, Any]:
    origin = get_origin(hint)
    if origin is typing.Union or origin is types.UnionType:
        args = [a for a in get_args(hint) if a is not type(None)]
        if len(args) == 1 and len(get_args(hint)) == 2:
            return True, args[0]
    return False, hint


def is_supported(hint: Any) -> bool:
    optional, inner = _is_optional(hint)
    if optional:
        return is_supported(inner)
    if hint in (str, int, float, bool, date, datetime, timedelta):
        return True
    origin = get_origin(hint)
    if origin is list:
        return True
    if origin is dict:
        return True
    if origin is typing.Literal:
        return True
    if inspect.isclass(hint) and issubclass(hint, enum.Enum):
        return True
    if dataclasses.is_dataclass(hint) and inspect.isclass(hint):
        return True
    return False


def describe(hint: Any) -> str:
    optional, inner = _is_optional(hint)
    if optional:
        return f"{describe(inner)} or None"
    if hint is Any:
        return "any value"
    if inspect.isclass(hint):
        return hint.__name__
    return str(hint).replace("typing.", "")


# ---------------------------------------------------------------------------
# coercion


def coerce(value: Any, hint: Any, name: str) -> Any:
    """Coerce ``value`` (a Python object, a JSON value, or a CLI string) to ``hint``."""
    if hint is Any or not is_supported(hint):
        return value
    optional, inner = _is_optional(hint)
    if optional:
        if value is None:
            return None
        if isinstance(value, str) and value.strip().lower() in ("none", "null"):
            return None
        return coerce(value, inner, name)

    def fail() -> ParameterError:
        return ParameterError(name, describe(hint), value)

    if hint is bool:
        if isinstance(value, bool):
            return value
        if isinstance(value, int) and value in (0, 1):
            return bool(value)
        if isinstance(value, str):
            v = value.strip().lower()
            if v in _TRUE:
                return True
            if v in _FALSE:
                return False
        raise fail()
    if hint is int:
        if isinstance(value, bool):
            raise fail()
        if isinstance(value, int):
            return value
        if isinstance(value, float) and value.is_integer():
            return int(value)
        if isinstance(value, str):
            try:
                return int(value.strip())
            except ValueError:
                raise fail() from None
        raise fail()
    if hint is float:
        if isinstance(value, bool):
            raise fail()
        if isinstance(value, (int, float)):
            return float(value)
        if isinstance(value, str):
            try:
                return float(value.strip())
            except ValueError:
                raise fail() from None
        raise fail()
    if hint is str:
        if isinstance(value, str):
            return value
        if isinstance(value, (int, float, bool)):
            return str(value)
        raise fail()
    if hint is datetime:
        if isinstance(value, datetime):
            return value
        if isinstance(value, date):
            return datetime(value.year, value.month, value.day)
        if isinstance(value, str):
            try:
                return datetime.fromisoformat(value.strip())
            except ValueError:
                raise fail() from None
        raise fail()
    if hint is date:
        if isinstance(value, datetime):
            return value.date()
        if isinstance(value, date):
            return value
        if isinstance(value, str):
            try:
                return date.fromisoformat(value.strip())
            except ValueError:
                raise fail() from None
        raise fail()
    if hint is timedelta:
        if isinstance(value, timedelta):
            return value
        if isinstance(value, bool):
            raise fail()
        if isinstance(value, (int, float)):
            return timedelta(seconds=value)
        if isinstance(value, str):
            return _parse_timedelta(value, fail)
        raise fail()

    origin = get_origin(hint)
    if origin is list:
        (item_hint,) = get_args(hint) or (Any,)
        if isinstance(value, str):
            value = _loads(value, fail)
        if isinstance(value, (list, tuple)):
            return [coerce(v, item_hint, f"{name}[{i}]") for i, v in enumerate(value)]
        raise fail()
    if origin is dict:
        args = get_args(hint)
        value_hint = args[1] if len(args) == 2 else Any
        if isinstance(value, str):
            value = _loads(value, fail)
        if isinstance(value, dict):
            return {str(k): coerce(v, value_hint, f"{name}.{k}") for k, v in value.items()}
        raise fail()
    if origin is typing.Literal:
        choices = get_args(hint)
        if value in choices:
            return value
        for choice in choices:
            try:
                if coerce(value, type(choice), name) == choice:
                    return choice
            except ParameterError:
                continue
        raise fail()
    if inspect.isclass(hint) and issubclass(hint, enum.Enum):
        if isinstance(value, hint):
            return value
        try:
            return hint(value)
        except ValueError:
            pass
        if isinstance(value, str):
            if value in hint.__members__:
                return hint.__members__[value]
            for member in hint:
                try:
                    if coerce(value, type(member.value), name) == member.value:
                        return member
                except ParameterError:
                    continue
        raise fail()
    if dataclasses.is_dataclass(hint) and inspect.isclass(hint):
        if isinstance(value, hint):
            return value
        if isinstance(value, str):
            value = _loads(value, fail)
        if isinstance(value, dict):
            hints = get_type_hints(hint)
            kwargs = {}
            for field in dataclasses.fields(hint):
                if field.name in value:
                    kwargs[field.name] = coerce(
                        value[field.name], hints.get(field.name, Any), f"{name}.{field.name}"
                    )
            try:
                return hint(**kwargs)
            except TypeError:
                raise fail() from None
        raise fail()
    return value


def _loads(text: str, fail: Callable[[], ParameterError]) -> Any:
    try:
        return json.loads(text)
    except ValueError:
        raise fail() from None


def _parse_timedelta(text: str, fail: Callable[[], ParameterError]) -> timedelta:
    text = text.strip()
    try:
        return timedelta(seconds=float(text))
    except ValueError:
        pass
    if ":" in text:
        parts = text.split(":")
        try:
            nums = [float(p) for p in parts]
        except ValueError:
            raise fail() from None
        if len(nums) == 2:
            return timedelta(minutes=nums[0], seconds=nums[1])
        if len(nums) == 3:
            return timedelta(hours=nums[0], minutes=nums[1], seconds=nums[2])
    raise fail()


def coerce_all(specs: list[ParamSpec], values: dict[str, Any]) -> dict[str, Any]:
    """Coerce a mapping of raw values; unknown names raise, missing required names raise."""
    known = {s.name for s in specs}
    unknown = set(values) - known
    if unknown:
        raise ParameterError(sorted(unknown)[0], "a declared parameter", values[sorted(unknown)[0]])
    out: dict[str, Any] = {}
    for spec in specs:
        if spec.name in values:
            out[spec.name] = coerce(values[spec.name], spec.hint, spec.name)
        elif spec.required:
            raise ParameterError(spec.name, describe(spec.hint), None)
        else:
            out[spec.name] = spec.default
    return out


# ---------------------------------------------------------------------------
# JSON schema


def schema_for(hint: Any) -> dict[str, Any]:
    if hint is Any or not is_supported(hint):
        return {}
    optional, inner = _is_optional(hint)
    if optional:
        return {"anyOf": [schema_for(inner), {"type": "null"}]}
    if hint is str:
        return {"type": "string"}
    if hint is bool:
        return {"type": "boolean"}
    if hint is int:
        return {"type": "integer"}
    if hint is float:
        return {"type": "number"}
    if hint is date:
        return {"type": "string", "format": "date"}
    if hint is datetime:
        return {"type": "string", "format": "date-time"}
    if hint is timedelta:
        return {"type": "number", "format": "duration-seconds"}
    origin = get_origin(hint)
    if origin is list:
        (item,) = get_args(hint) or (Any,)
        return {"type": "array", "items": schema_for(item)}
    if origin is dict:
        args = get_args(hint)
        return {"type": "object", "additionalProperties": schema_for(args[1] if len(args) == 2 else Any)}
    if origin is typing.Literal:
        return {"enum": list(get_args(hint))}
    if inspect.isclass(hint) and issubclass(hint, enum.Enum):
        return {"enum": [m.value for m in hint], "title": hint.__name__}
    if dataclasses.is_dataclass(hint) and inspect.isclass(hint):
        hints = get_type_hints(hint)
        props = {}
        required = []
        for field in dataclasses.fields(hint):
            props[field.name] = schema_for(hints.get(field.name, Any))
            if field.default is dataclasses.MISSING and field.default_factory is dataclasses.MISSING:
                required.append(field.name)
        out: dict[str, Any] = {"type": "object", "title": hint.__name__, "properties": props}
        if required:
            out["required"] = required
        return out
    return {}


def to_json_value(value: Any) -> Any:
    """Render a parameter value as JSON-compatible data."""
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, datetime):
        return value.isoformat()
    if isinstance(value, date):
        return value.isoformat()
    if isinstance(value, timedelta):
        return value.total_seconds()
    if isinstance(value, enum.Enum):
        return to_json_value(value.value)
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {f.name: to_json_value(getattr(value, f.name)) for f in dataclasses.fields(value)}
    if isinstance(value, dict):
        return {str(k): to_json_value(v) for k, v in value.items()}
    if isinstance(value, (list, tuple, set, frozenset)):
        return [to_json_value(v) for v in value]
    try:
        json.dumps(value)
        return value
    except (TypeError, ValueError):
        return repr(value)


def json_schema(specs: list[ParamSpec]) -> dict[str, Any]:
    properties: dict[str, Any] = {}
    required: list[str] = []
    for spec in specs:
        prop = dict(schema_for(spec.hint))
        if spec.required:
            required.append(spec.name)
        else:
            prop["default"] = to_json_value(spec.default)
        properties[spec.name] = prop
    schema: dict[str, Any] = {"type": "object", "properties": properties}
    if required:
        schema["required"] = required
    return schema
