from __future__ import annotations


class RunPaused(BaseException):
    """Raised by ``wait_for_input`` to end the attempt while the run waits."""

    def __init__(self, prompt: str, schema: dict | None, task_run: str | None) -> None:
        super().__init__(prompt)
        self.prompt = prompt
        self.schema = schema
        self.task_run = task_run


class CereyanError(Exception):
    """Base class for errors raised by cereyan."""


class ParameterError(CereyanError, ValueError):
    """A parameter value could not be coerced to its declared type."""

    def __init__(self, name: str, expected: str, value: object) -> None:
        self.name = name
        self.expected = expected
        self.value = value
        super().__init__(
            f"parameter {name!r} expects {expected}, got {value!r}"
        )


class FlowRegistrationError(CereyanError):
    """A flow could not be registered on an App."""


class ConfigError(CereyanError):
    """A cereyan.toml file is invalid."""
