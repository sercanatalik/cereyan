from __future__ import annotations


class RunPaused(BaseException):
    """Raised by ``wait_for_input`` to end the attempt while the run waits."""

    def __init__(self, prompt: str, schema: dict | None, task_run: str | None, index: int = 0) -> None:
        super().__init__(prompt)
        self.prompt = prompt
        self.schema = schema
        self.task_run = task_run
        #: Which question of this execution of the body is waiting: 0 is the
        #: first. The answer is stored against it, so asking twice works.
        self.index = index


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


class Abort(CereyanError):
    """Raised to fail a run or task run at once, with no retry.

    ``retries`` says how many times to try again; this says that trying again
    cannot help. The failure is recorded with ``abort`` in its state details,
    so a refused retry is distinguishable from an exhausted one.
    """


class FlowRegistrationError(CereyanError):
    """A flow could not be registered on an App."""


class ConfigError(CereyanError):
    """A cereyan.toml file is invalid."""
