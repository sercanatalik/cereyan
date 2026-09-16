"""Run and task-run context, carried by contextvars."""

from __future__ import annotations

import contextvars
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .engine.backends import Backend
    from .flows import Flow


@dataclass
class RunContext:
    id: int
    external_id: str
    name: str
    flow: Flow
    parameters: dict[str, Any]
    backend: Backend | None = None
    runner: Any = None
    futures: list = field(default_factory=list)
    #: Which execution of the flow body this is: 0 the first time, the next
    #: after a resume or an in-process retry. Task runs carry it, so the same
    #: call keeps its dynamic key across executions.
    pass_: int = 0
    #: The thread executing the flow body, captured where the context is made.
    #: `wait_for_input` pauses by raising out of the body, which only works on
    #: this thread; from a task on a worker thread it would surface as a task
    #: failure instead.
    body_thread: int = field(default_factory=__import__("threading").get_ident)
    _counters: dict[str, int] = field(default_factory=dict)
    _input_index: int = 0
    _lock: Any = field(default_factory=__import__("threading").Lock)

    @property
    def flow_name(self) -> str:
        return self.flow.name

    @property
    def project(self) -> str:
        return self.flow.project

    def next_dynamic_key(self, task_name: str) -> str:
        with self._lock:
            n = self._counters.get(task_name, 0)
            self._counters[task_name] = n + 1
        return f"{task_name}-{n}"

    def next_input_index(self) -> int:
        """The position of this `wait_for_input` call in the body's execution.

        Answers are matched to questions by this position, so the numbering
        restarts with each pass exactly as the dynamic keys do: a replayed body
        asks its questions in the same order and gets the same answers back.
        """
        with self._lock:
            n = self._input_index
            self._input_index = n + 1
            return n

    def start_pass(self) -> int:
        """Begin the next execution of the body, numbering its calls from zero.

        The keys of a pass repeat the keys of the one before it, which is what
        makes the same call comparable across executions; the pass tells the
        task runs apart.
        """
        with self._lock:
            self.pass_ += 1
            self._counters.clear()
            self._input_index = 0
            return self.pass_


@dataclass
class TaskRunContext:
    """The task run in progress. ``id`` is the external UUID; the store's
    integer id is available offline as ``row_id``."""

    id: str
    name: str
    task_key: str
    dynamic_key: str
    row_id: int | None = None

    @property
    def external_id(self) -> str:
        return self.id


_run: contextvars.ContextVar[RunContext | None] = contextvars.ContextVar("cereyan_run", default=None)
_task_run: contextvars.ContextVar[TaskRunContext | None] = contextvars.ContextVar(
    "cereyan_task_run", default=None
)


def current_run() -> RunContext | None:
    return _run.get()


def current_task_run() -> TaskRunContext | None:
    return _task_run.get()


def set_run(ctx: RunContext | None) -> contextvars.Token:
    return _run.set(ctx)


def reset_run(token: contextvars.Token) -> None:
    _run.reset(token)


def set_task_run(ctx: TaskRunContext | None) -> contextvars.Token:
    return _task_run.set(ctx)


def reset_task_run(token: contextvars.Token) -> None:
    _task_run.reset(token)
