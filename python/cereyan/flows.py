"""``@flow``: wrap a function, derive its parameters, register it on an App."""

from __future__ import annotations

import functools
import inspect
import json
import os
import sys
from typing import Any, Callable, Iterable

from . import params as _params
from .exceptions import CereyanError
from .tasks import _async_message, _is_async_function


def _parse_after(after, batch_key):
    """``after=`` forms: a name, ``(name, {param: template})``, or a list of names
    with ``batch_key`` naming the parameter that identifies a batch."""
    import re

    if batch_key is not None and not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,63}", str(batch_key)):
        raise ValueError("batch_key must be a parameter name (letters, digits, underscore)")
    if after is None:
        if batch_key is not None:
            raise ValueError("batch_key= needs after=")
        return None
    parameters: dict = {}
    if isinstance(after, tuple) and after and isinstance(after[0], str) and (len(after) == 1 or isinstance(after[1], dict)):
        names = [after[0]]
        parameters = dict(after[1]) if len(after) > 1 else {}
    elif isinstance(after, str):
        names = [after]
    elif isinstance(after, (list, tuple, set)):
        names = [str(n) for n in after]
        if not names or not all(isinstance(n, str) for n in after):
            raise TypeError("after= list entries must be flow names")
    else:
        raise TypeError("after= must be a flow name, (name, parameter mapping), or a list of names")
    if len(names) > 1 and batch_key is None:
        raise ValueError("after= with several upstreams needs batch_key= naming the parameter that identifies a batch")
    return {"flow": names[0], "flows": names, "key": batch_key, "parameters": parameters}


def _seconds(value, name: str) -> float | None:
    """A positive number of seconds from a number or a timedelta, or None."""
    if value is None:
        return None
    from datetime import timedelta

    seconds = value.total_seconds() if isinstance(value, timedelta) else float(value)
    if seconds <= 0:
        raise ValueError(f"{name} must be a positive number of seconds or a timedelta")
    return seconds


class Unique:
    """At most one run of the flow per key at a time: ``@flow(unique=Unique(...))``.

    Args:
        key: Template over the flow's parameters, ``"{day}"``; every parameter when
            ``None``. Two submissions render the same key when they name the same run.
        period: Seconds; fixed windows bucket the key, so ``period=3600`` allows one
            run per key per clock hour.
        states: State types in which an existing run counts, default every
            non-terminal one. Add ``"Completed"`` to keep a key taken after the run ends.
        on_conflict: ``"skip"`` (the default) answers with the run that holds the key
            and creates nothing; ``"replace"`` cancels it and creates the new run;
            ``"debounce"`` creates the run ``debounce`` seconds ahead and, while it has
            not started, every further submission moves its start to now plus
            ``debounce`` (at most ``max_wait`` after its creation) and replaces its
            parameters; ``"throttle"`` keeps the first run in any sliding window of
            ``period`` seconds and answers the others with it.
        debounce: Seconds a debounced run waits after the last submission.
        max_wait: The longest a debounced run may be pushed back from its creation.
    """

    def __init__(self, key: str | None = None, period: float | None = None,
                 states: Iterable[str] = (), on_conflict: str = "skip",
                 debounce: float | None = None, max_wait: float | None = None) -> None:
        if on_conflict not in ("skip", "replace", "debounce", "throttle"):
            raise ValueError("on_conflict must be 'skip', 'replace', 'debounce', or 'throttle'")
        if period is not None and float(period) <= 0:
            raise ValueError("period must be a positive number of seconds")
        if on_conflict == "debounce" and (debounce is None or float(debounce) <= 0):
            raise ValueError("on_conflict='debounce' needs debounce=<seconds>")
        if on_conflict == "throttle" and period is None:
            raise ValueError("on_conflict='throttle' needs period=<seconds>")
        if max_wait is not None and float(max_wait) <= 0:
            raise ValueError("max_wait must be a positive number of seconds")
        self.debounce = float(debounce) if debounce is not None else None
        self.max_wait = float(max_wait) if max_wait is not None else None
        known = ("Scheduled", "Pending", "Running", "Paused", "Cancelling", "Completed", "Failed", "Cancelled", "Crashed")
        self.states = tuple(states)
        for st in self.states:
            if st not in known:
                raise ValueError(f"unknown state type {st!r} in Unique(states=...)")
        self.key = key
        self.period = float(period) if period is not None else None
        self.on_conflict = on_conflict

    def spec(self) -> dict[str, Any]:
        """The declaration as the server records it in the flow's options."""
        return {"key": self.key, "period": self.period, "states": list(self.states), "on_conflict": self.on_conflict,
                "debounce": self.debounce, "max_wait": self.max_wait}

    def __repr__(self) -> str:
        return f"Unique(key={self.key!r}, period={self.period!r}, states={self.states!r}, on_conflict={self.on_conflict!r})"


class Flow:
    """A registered flow: the wrapped function plus its options, parameters, and identity.

    Instances are created by `flow`; call one to start a run. Attributes mirror the
    decorator options, and ``parameters`` and ``schema`` describe the parameters derived
    from the function's type hints.
    """
    def __init__(
        self,
        fn: Callable,
        *,
        name: str | None = None,
        description: str | None = None,
        tags: Iterable[str] = (),
        group: str | None = None,
        run_name: str | Callable[..., str] | None = None,
        isolated: bool = False,
        log_prints: bool = False,
        schedule: Any = None,
        schedules: Any = None,
        retries: int = 0,
        retry_delay: Any = 0,
        retry_on: tuple[type[BaseException], ...] | None = None,
        retry_when: Callable[[BaseException, int], bool] | None = None,
        timeout_seconds: float | None = None,
        crash_retries: int | None = None,
        priority: int = 0,
        max_concurrent: int | None = None,
        on_overlap: str = "enqueue",
        resources: dict[str, float] | None = None,
        after: Any = None,
        batch_key: str | None = None,
        disable_after: tuple | None = None,
        runner: Any = None,
        bulk_complete: Callable | None = None,
        on_completion: Iterable[Callable] = (),
        on_failure: Iterable[Callable] = (),
        on_crashed: Iterable[Callable] = (),
        on_cancellation: Iterable[Callable] = (),
        mcp_tool: bool = False,
        start_deadline: float | None = None,
        checkpoint: bool | None = None,
        checkpoint_max_bytes: int = 50_000_000,
        unique: "Unique | None" = None,
        fresh_within: "float | timedelta | None" = None,
        expect_by: str | None = None,
        expect_by_tz: str | None = None,
        expected_duration: "float | timedelta | None" = None,
        overdue_factor: float | None = None,
    ) -> None:
        if not callable(fn):
            raise TypeError("@flow must decorate a callable")
        if _is_async_function(fn):
            # Raised before App.register, so a rejected flow is never registered.
            raise TypeError(_async_message("flow", fn))
        from . import schedules as _schedules

        declared = list(_schedules.normalize(schedule)) + list(_schedules.normalize(schedules))
        for i, d in enumerate(declared):
            if d.get("key") is None or d["key"].startswith("code-"):
                d["key"] = f"code-{i}"
        self.schedules = declared
        self.retries = int(retries)
        self.retry_delay = retry_delay
        self.retry_on = tuple(retry_on) if retry_on else None
        self.retry_when = retry_when
        self.timeout_seconds = timeout_seconds
        self.crash_retries = crash_retries
        if checkpoint is not None and not isinstance(checkpoint, bool):
            raise ValueError("checkpoint must be None, True, or False")
        self.checkpoint = checkpoint
        self.checkpoint_max_bytes = int(checkpoint_max_bytes)
        if unique is not None and not isinstance(unique, Unique):
            raise TypeError("unique must be a cereyan.Unique")
        self.unique = unique
        self.fresh_within = _seconds(fresh_within, "fresh_within")
        self.expect_by = expect_by
        self.expect_by_tz = expect_by_tz
        if expect_by is not None:
            import json as _json
            import time as _time

            from . import _core

            spec = {"kind": "cron", "cron": str(expect_by), "timezone": expect_by_tz}
            _core.schedule_fires(_json.dumps(spec), int(_time.time() * 1_000_000), 1)
        self.expected_duration = _seconds(expected_duration, "expected_duration")
        if overdue_factor is not None and float(overdue_factor) <= 0:
            raise ValueError("overdue_factor must be a positive number")
        self.overdue_factor = float(overdue_factor) if overdue_factor is not None else None
        self.priority = int(priority)
        if max_concurrent is not None and int(max_concurrent) < 1:
            raise ValueError("max_concurrent must be at least 1")
        self.max_concurrent = int(max_concurrent) if max_concurrent is not None else None
        if on_overlap not in ("enqueue", "skip", "cancel_new", "cancel_old", "buffer_one"):
            raise ValueError("on_overlap must be 'enqueue', 'skip', 'cancel_new', 'cancel_old', or 'buffer_one'")
        self.on_overlap = on_overlap
        if start_deadline is not None and float(start_deadline) < 0:
            raise ValueError("start_deadline must be zero or more seconds")
        self.start_deadline = float(start_deadline) if start_deadline is not None else None
        self.resources = dict(resources or {})
        self.after = _parse_after(after, batch_key)
        if disable_after is not None:
            count, window, persist = disable_after
            self.disable_after = (int(count), int(window), int(persist))
        else:
            self.disable_after = None
        self.runner = runner
        self.bulk_complete = bulk_complete
        self.on_completion = list(on_completion)
        self.on_failure = list(on_failure)
        self.on_crashed = list(on_crashed)
        self.on_cancellation = list(on_cancellation)
        self.mcp_tool = bool(mcp_tool)
        functools.update_wrapper(self, fn)
        self.fn = fn
        self.name = name or fn.__name__
        self.description = description if description is not None else inspect.getdoc(fn)
        self.tags = sorted(set(tags))
        self.declared_group = str(group) if group is not None else None
        self.run_name = run_name
        self.isolated = bool(isolated)
        self.log_prints = bool(log_prints)
        self.parameters = _params.parameter_specs(fn)
        self.schema = _params.json_schema(self.parameters)
        self.signature = inspect.signature(fn)
        module = sys.modules.get(fn.__module__)
        file = getattr(module, "__file__", None) or inspect.getsourcefile(fn)
        self.source_file = os.path.abspath(file) if file else os.path.join(os.getcwd(), "<unknown>")
        self.source_dir = os.path.dirname(self.source_file)
        if fn.__module__ == "__main__" and file:
            # A script run directly: engines import it by file stem.
            self.module = os.path.splitext(os.path.basename(file))[0]
        else:
            self.module = fn.__module__
        try:
            line = inspect.getsourcelines(fn)[1]
        except (OSError, TypeError):
            line = 0
        self.source_location = f"{self.source_file}:{line}"
        self.app = None  # set by App.register

    def __repr__(self) -> str:
        return f"Flow({self.name!r})"

    @property
    def group(self) -> str:
        """The group this flow belongs to: the one it declared, else its `project`.

        A group is a level inside the project: the same name declared in two projects
        is two groups, one under each. A flow whose group is its project, because it
        declared none or declared the project's own name, is one of the project's own
        flows rather than a member of a group.
        """
        return self.declared_group if self.declared_group is not None else self.project

    @property
    def project(self) -> str:
        """The project this flow belongs to: the name of its App, or ``"default"`` before registration."""
        return self.app.name if self.app is not None else "default"

    @property
    def options(self) -> dict[str, Any]:
        """The execution options recorded with the flow on registration, as the server stores them."""
        return {
            "isolated": self.isolated,
            "log_prints": self.log_prints,
            "priority": self.priority,
            "max_concurrent": self.max_concurrent,
            "on_overlap": self.on_overlap,
            "resources": self.resources,
            "crash_retries": self.crash_retries,
            "timeout_seconds": self.timeout_seconds,
            "retries": self.retries,
            "after": self.after,
            "disable_after": list(self.disable_after) if self.disable_after else None,
            "schedules": self.schedules,
            "has_bulk_complete": self.bulk_complete is not None,
            "has_crash_hooks": bool(self.on_crashed),
            "mcp_tool": self.mcp_tool,
            "start_deadline": self.start_deadline,
            "checkpoint": self.checkpoint,
            "checkpoint_max_bytes": self.checkpoint_max_bytes,
            "unique": self.unique.spec() if self.unique else None,
            "fresh_within": self.fresh_within,
            "expect_by": self.expect_by,
            "expect_by_tz": self.expect_by_tz,
            "expected_duration": self.expected_duration,
            "overdue_factor": self.overdue_factor,
        }

    # -- parameters -------------------------------------------------------

    def bind(self, args: tuple, kwargs: dict) -> dict[str, Any]:
        """Bind call arguments to parameter names and coerce them."""
        bound = self.signature.bind(*args, **kwargs)
        bound.apply_defaults()
        raw = {k: v for k, v in bound.arguments.items()}
        return self.coerce(raw)

    def coerce(self, values: dict[str, Any]) -> dict[str, Any]:
        """Coerce a mapping of raw parameter values to the declared types."""
        return _params.coerce_all(self.parameters, values)

    def parameters_json(self, values: dict[str, Any]) -> str:
        """Serialise parameter values to the JSON string stored with a run."""
        return json.dumps(_params.to_json_value(values), sort_keys=True)

    def render_run_name(self, values: dict[str, Any]) -> str | None:
        """Render the run name from ``run_name`` for these parameter values, or ``None`` when unset."""
        if self.run_name is None:
            return None
        if callable(self.run_name):
            return str(self.run_name(**values))
        try:
            return self.run_name.format(**values)
        except (KeyError, IndexError, ValueError) as exc:
            raise CereyanError(f"run_name template {self.run_name!r} failed: {exc}") from exc

    # -- execution --------------------------------------------------------

    def __call__(self, *args, **kwargs):
        from .engine import run_flow

        return run_flow(self, args, kwargs)


def flow(
    fn: Callable | None = None,
    *,
    name: str | None = None,
    description: str | None = None,
    tags: Iterable[str] = (),
    group: str | None = None,
    run_name: str | Callable[..., str] | None = None,
    isolated: bool = False,
    log_prints: bool = False,
    app=None,
    **options,
):
    """Register a function as a flow on ``app`` or on the default App.

    Use as ``@flow`` or ``@flow(...)``. Calling the returned `Flow` runs it:
    offline, the run is recorded in the local store; while a server is up, the run
    is handed to the server and its logs stream back. Parameters come from the
    function's type hints and are coerced before the body runs.

    Args:
        fn (Callable): The function to wrap; supplied by the decorator syntax.
        name (str | None): Flow name; defaults to the function name. A flow's identity is
            ``(project, name)``.
        description (str | None): Shown in the UI; defaults to the function's docstring.
        tags (Iterable[str]): Tags copied onto every run.
        group (str | None): The group this flow is listed under, inside its project, in
            the UI and in the ``group`` filter of the API, the CLI, and MCP. The
            same name declared in two projects is two groups. Defaults to the
            project, which lists the flow as one of the project's own flows.
        run_name (str | Callable[..., str] | None): A ``str.format`` template over the parameters, such as
            ``"etl-{day}"``, or a callable taking the parameters as keyword
            arguments and returning the name.
        isolated (bool): Run every run in a fresh engine process instead of the warm pool.
        log_prints (bool): Tee ``print`` output into the run log at INFO.
        app (App | None): The `App` to register on; defaults to the default App of the
            defining module.
        schedule (Cron | Interval | RRule | list | None): A `Cron`, `Interval`, or `RRule`, or a list
            of them; the server materialises runs from it.
        schedules (Cron | Interval | RRule | list | None): Same as ``schedule``; both are combined.
        retries (int): How many times a failed run is retried.
        retry_delay (float | list[float] | exponential): Wait before each retry: seconds, a list of per-attempt seconds,
            or `exponential`.
        retry_on (tuple[type[BaseException], ...] | None): Retry only these exception types; any other failure ends the
            run on its first attempt. Combined with ``retry_when``, both must
            allow a retry.
        retry_when (Callable[[BaseException, int], bool] | None): Called with the exception and the attempt that just
            failed; return ``False`` to stop retrying. A predicate that raises
            stops the retry and leaves the original failure recorded.
        timeout_seconds (float | None): End the run as Failed with sub-state ``TimedOut`` after this
            many seconds.
        crash_retries (int | None): How many times a run whose engine died is rerun before it is
            marked Failed; defaults to ``[defaults] crash_retries`` in
            ``cereyan.toml``, then 5.
        priority (int): Dispatch order among queued runs, higher first; never preempts a
            running run. A negative value also raises the engine's OS niceness on
            Linux and macOS.
        max_concurrent (int | None): Cap on runs of this flow in Pending or Running at once,
            implemented as a resource named after the flow; unlimited by default.
        on_overlap (str): What a new run does when ``max_concurrent`` is reached:
            ``"enqueue"`` (wait as AwaitingResource, the default), ``"skip"`` (end
            Skipped), ``"cancel_new"`` (end Cancelled), ``"cancel_old"`` (cancel the
            flow's other runs and take their slot), or ``"buffer_one"`` (wait, unless a
            run is already waiting, in which case end Skipped).
        start_deadline (float | None): Seconds a run may wait to start before it is
            skipped with reason ``missed_start_deadline``; a schedule's own value wins.
        checkpoint (bool | None): Whether completed tasks' results are checkpointed and
            replayed by a later attempt. ``None`` (the default) replays crash reruns and
            resumes after ``wait_for_input``; ``True`` also replays in-process retries;
            ``False`` never checkpoints. Replay stops at the first task whose inputs differ.
        checkpoint_max_bytes (int): Largest encoded result that is checkpointed (default
            50 MB); a larger or unpicklable result executes again on replay.
        unique (Unique | None): At most one run per key at a time; see `Unique`. A
            second submission answers with the run that holds the key (or, with
            ``on_conflict="replace"``, cancels it), wherever the run is created.
        fresh_within (float | timedelta | None): The flow's health is FAIL when its
            last Completed run is older than this, WARN past three quarters of it.
        expect_by (str | None): A cron by which a run must have completed
            (``"0 9 * * *"``, in ``expect_by_tz``); health is FAIL when the latest
            deadline passed without a completed run since the previous one.
        expect_by_tz (str | None): IANA timezone for ``expect_by``.
        expected_duration (float | timedelta | None): A Running run longer than this
            makes the flow WARN and records ``run.overdue`` once.
        overdue_factor (float | None): As ``expected_duration``, against this many
            times the median duration of the last 20 Completed runs.
        resources (dict[str, float] | None): Named resources and the amount each run holds, for example
            ``{"db": 1}``; a run waits as AwaitingResource until they are free.
        after (str | tuple | list[str] | None): Upstream dependency: a flow name, ``(name, {param: template})`` to map
            the upstream run's parameters onto this flow's, or a list of names
            together with ``batch_key`` for fan-in.
        batch_key (str | None): With a list in ``after``, the parameter whose value identifies a
            batch; this flow runs once per value after every upstream has a
            completed run for it.
        disable_after (tuple[int, int, int] | None): ``(count, window_seconds, persist_seconds)``: after ``count``
            failures within ``window_seconds`` the flow's schedules pause for
            ``persist_seconds`` and resume automatically.
        runner (ThreadRunner | ProcessRunner | None): A `ThreadRunner` or `ProcessRunner` executing tasks
            submitted with ``submit`` and ``map``; a thread runner by default.
        bulk_complete (Callable | None): ``bulk_complete(values) -> set`` called once by a backfill;
            runs for the returned values are recorded as Skipped.
        on_completion (Iterable[Callable]): Hooks ``hook(flow, run, state)`` called after a run completes.
        on_failure (Iterable[Callable]): Hooks called after a run fails.
        on_crashed (Iterable[Callable]): Hooks called after a run crashes.
        on_cancellation (Iterable[Callable]): Hooks called after a run is cancelled.
        mcp_tool (bool): Publish the flow as an MCP tool named ``flow__<project>__<name>`` whose
            arguments are its parameters, so an agent can start it directly.
        start_deadline (float | None): Seconds a run may wait to start before it is skipped.

    Returns:
        Flow: The flow wrapping ``fn``; call it like the original function.
    """

    def decorate(func: Callable) -> Flow:
        flow_obj = Flow(
            func,
            name=name,
            description=description,
            tags=tags,
            group=group,
            run_name=run_name,
            isolated=isolated,
            log_prints=log_prints,
            **options,
        )
        target = app
        if target is None:
            from .apps import get_default_app

            target = get_default_app(source_file=flow_obj.source_file)
        target.register(flow_obj)
        return flow_obj

    if fn is not None:
        return decorate(fn)
    return decorate
