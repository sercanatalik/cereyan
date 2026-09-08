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
        run_name: str | Callable[..., str] | None = None,
        isolated: bool = False,
        log_prints: bool = False,
        schedule: Any = None,
        schedules: Any = None,
        retries: int = 0,
        retry_delay: Any = 0,
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
    ) -> None:
        if not callable(fn):
            raise TypeError("@flow must decorate a callable")
        from . import schedules as _schedules

        declared = list(_schedules.normalize(schedule)) + list(_schedules.normalize(schedules))
        for i, d in enumerate(declared):
            if d.get("key") is None or d["key"].startswith("code-"):
                d["key"] = f"code-{i}"
        self.schedules = declared
        self.retries = int(retries)
        self.retry_delay = retry_delay
        self.timeout_seconds = timeout_seconds
        self.crash_retries = crash_retries
        self.priority = int(priority)
        if max_concurrent is not None and int(max_concurrent) < 1:
            raise ValueError("max_concurrent must be at least 1")
        self.max_concurrent = int(max_concurrent) if max_concurrent is not None else None
        if on_overlap not in ("enqueue", "skip", "cancel_new"):
            raise ValueError("on_overlap must be 'enqueue', 'skip', or 'cancel_new'")
        self.on_overlap = on_overlap
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
        functools.update_wrapper(self, fn)
        self.fn = fn
        self.name = name or fn.__name__
        self.description = description if description is not None else inspect.getdoc(fn)
        self.tags = sorted(set(tags))
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
            Skipped), or ``"cancel_new"`` (end Cancelled).
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

    Returns:
        Flow: The flow wrapping ``fn``; call it like the original function.
    """

    def decorate(func: Callable) -> Flow:
        flow_obj = Flow(
            func,
            name=name,
            description=description,
            tags=tags,
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
