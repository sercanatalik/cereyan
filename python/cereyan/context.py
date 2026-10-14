"""Run and task-run context, carried by contextvars."""

from __future__ import annotations

import contextvars
import hashlib
import json
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
    #: Checkpoints this attempt may replay, by dynamic key: ``task_key``,
    #: ``input_hash`` and ``result_ref`` of the latest completed task run of an
    #: earlier pass or, for a crash rerun, of the crashed runs before it.
    checkpoints: dict[str, dict] = field(default_factory=dict)
    #: Whether the next task call may return a checkpoint. The first task that
    #: has none, or whose inputs differ, turns it off for the rest of the attempt.
    replaying: bool = False
    #: A forced (restated) run: targets, cache hits and checkpoints are ignored.
    force: bool = False
    #: The effect journal, by slot: the flow-body effects (events, artifacts,
    #: published state, attributes) an earlier pass, or a crashed run of the
    #: chain, already performed, as ``{"hash", "ref"}``. A call whose slot and
    #: hash match is skipped. See `effect`.
    effects: dict[str, dict] = field(default_factory=dict)
    #: Whether flow-body effects are journaled and skipped at all: off for
    #: ``checkpoint=False`` flows and forced runs, which repeat everything.
    journal: bool = True
    #: The external id of the run that started this logical attempt: the first
    #: run of the crash chain, or what a retry from failure continues. Every
    #: task call's effect key derives from it.
    lineage: str = ""
    #: Dynamic keys of ``once`` calls an earlier run of the lineage started and
    #: never finished: they must not execute again unless something settles them.
    in_doubt: set[str] = field(default_factory=set)
    #: The thread executing the flow body, captured where the context is made.
    #: `wait_for_input` pauses by raising out of the body, which only works on
    #: this thread; from a task on a worker thread it would surface as a task
    #: failure instead.
    body_thread: int = field(default_factory=__import__("threading").get_ident)
    _counters: dict[str, int] = field(default_factory=dict)
    _effect_counters: dict[str, int] = field(default_factory=dict)
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
            self._effect_counters.clear()
            self._input_index = 0
            return self.pass_

    def stop_replaying(self) -> None:
        with self._lock:
            self.replaying = False

    def effect(self, kind: str, name: str, content: Any) -> tuple[bool, dict | None, str | None]:
        """Decide whether a flow-body effect is performed: ``(perform, mark, ref)``.

        The call takes the slot ``<kind>:<name>-<n>``, numbered per kind and name
        in call order and restarted every pass, as a task's dynamic key is, and
        hashes its content. A slot already journaled with the same hash happened
        before: the caller skips it and returns ``ref``. Otherwise the caller
        performs it and sends ``mark`` along, so the store journals it in the same
        write. Inside a task run nothing is journaled: a replayed task does not run
        its body, and a task that runs again does its effects again.
        """
        if current_task_run() is not None or not self.journal:
            return True, None, None
        with self._lock:
            counter = f"{kind}:{name}"
            n = self._effect_counters.get(counter, 0)
            self._effect_counters[counter] = n + 1
            slot = f"{counter}-{n}"
            text = json.dumps(content, sort_keys=True, separators=(",", ":"), default=str)
            digest = hashlib.sha256(text.encode()).hexdigest()
            known = self.effects.get(slot)
        if known is not None and known.get("hash") == digest:
            return False, None, known.get("ref")
        return True, {"slot": slot, "hash": digest, "pass": self.pass_}, None

    def remember_effect(self, mark: dict | None, ref: str | None = None) -> None:
        """Journal a performed effect in memory, for the later passes of this process."""
        if mark is not None:
            with self._lock:
                self.effects[mark["slot"]] = {"hash": mark["hash"], "ref": ref}

    def record_checkpoint(self, dynamic_key: str, task_key: str, input_hash: str, result_ref: str) -> None:
        """Remember a checkpoint this attempt wrote, so an in-process retry can replay it."""
        with self._lock:
            self.checkpoints[dynamic_key] = {"task_key": task_key, "input_hash": input_hash, "result_ref": result_ref}


@dataclass
class TaskRunContext:
    """The task run in progress. ``id`` is the external UUID; the store's
    integer id is available offline as ``row_id``."""

    id: str
    name: str
    task_key: str
    dynamic_key: str
    row_id: int | None = None
    # An async unit (`run_async_unit`) awaits on the flow thread, so the thread
    # check alone would let a wait inside it pause the run from a task run.
    unit: bool = False
    #: The run's lineage root, from which `effect_key` derives.
    lineage: str = ""

    @property
    def effect_key(self) -> str:
        """A key for this call's effect that an outside system can deduplicate on.

        The same for this call across passes, crash reruns and retries from
        failure; different after a retry from the start or a forced run, and for
        every other call. Pass it as the idempotency key of the API the task calls.
        """
        from .effects import effect_key

        return effect_key(self.lineage, self.dynamic_key)

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


def in_async_unit() -> bool:
    """Whether the code running now is inside an async unit's body."""
    task = _task_run.get()
    return task is not None and task.unit


def set_task_run(ctx: TaskRunContext | None) -> contextvars.Token:
    return _task_run.set(ctx)


def reset_task_run(token: contextvars.Token) -> None:
    _task_run.reset(token)
