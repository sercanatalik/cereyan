"""``@task``: a function whose calls inside a flow are recorded as task runs."""

from __future__ import annotations

import functools
import inspect
from datetime import timedelta
from typing import Any, Callable, Iterable

from . import context
from .results import CachePolicy


def _is_async_function(fn: Any) -> bool:
    """Whether ``fn`` is an ``async def``, with or without ``yield``.

    ``inspect.iscoroutinefunction`` is false for an async generator function, which
    fails the same way: its body never runs and the call returns an object.
    """
    return inspect.iscoroutinefunction(fn) or inspect.isasyncgenfunction(fn)


def _async_message(kind: str, fn: Callable) -> str:
    """The error raised when ``@flow`` or ``@task`` is given an ``async def``."""
    name = getattr(fn, "__name__", "<anonymous>")
    record = "task run" if kind == "task" else "run"
    if inspect.isasyncgenfunction(fn):
        what = "`async def` with `yield`"
        returns = "an async generator"
        wrapper = (
            f"    @{kind}\n"
            f"    def {name}(...):\n"
            f"        async def collect():\n"
            f"            return [item async for item in _{name}(...)]\n"
            f"        return asyncio.run(collect())"
        )
    else:
        what = "`async def`"
        returns = "a coroutine"
        wrapper = (
            f"    @{kind}\n"
            f"    def {name}(...):\n"
            f"        return asyncio.run(_{name}(...))"
        )
    body = "Task bodies" if kind == "task" else "Flow bodies"
    return (
        f"@{kind} cannot decorate an async function: {name} is {what}. "
        f"{body} run synchronously, so calling it would return {returns} without "
        f"running the body, and the {record} would be recorded as having succeeded. "
        f"Drive it yourself instead:\n\n"
        f"{wrapper}\n\n"
        f"See https://sercanatalik.github.io/cereyan/guides/fetch-from-an-api/"
    )


class Task:
    """A registered task: the wrapped function plus its options.

    Instances are created by `task`. Calling one inside a flow records a task run;
    ``submit`` and ``map`` return `Future` objects instead of waiting.
    """
    def __init__(
        self,
        fn: Callable,
        *,
        name: str | None = None,
        description: str | None = None,
        tags: Iterable[str] = (),
        log_prints: bool = False,
        retries: int = 0,
        retry_delay: Any = 0,
        timeout_seconds: float | None = None,
        output: Any = None,
        cache: CachePolicy | None = None,
        cache_expires: timedelta | None = None,
        persist_result: bool = False,
        serializer: str = "pickle",
        resources: dict[str, float] | None = None,
        on_completion: Iterable[Callable] = (),
        on_failure: Iterable[Callable] = (),
        on_cancellation: Iterable[Callable] = (),
    ) -> None:
        if _is_async_function(fn):
            raise TypeError(_async_message("task", fn))
        functools.update_wrapper(self, fn)
        self.fn = fn
        self.name = name or fn.__name__
        self.description = description if description is not None else fn.__doc__
        self.tags = sorted(set(tags))
        self.log_prints = bool(log_prints)
        self.retries = int(retries)
        self.retry_delay = retry_delay
        self.timeout_seconds = timeout_seconds
        self.output = output
        self.cache = cache or CachePolicy.NONE
        self.cache_expires = cache_expires
        if self.cache and not persist_result:
            raise ValueError(f"task {self.name!r}: cache=... requires persist_result=True")
        self.persist_result = bool(persist_result)
        if serializer not in ("pickle", "json"):
            raise ValueError("serializer must be 'pickle' or 'json'")
        self.serializer = serializer
        self.resources = dict(resources or {})
        self.on_completion = list(on_completion)
        self.on_failure = list(on_failure)
        self.on_cancellation = list(on_cancellation)
        self.key = f"{fn.__module__}.{fn.__qualname__}"

    def __repr__(self) -> str:
        return f"Task({self.name!r})"

    def __reduce__(self):
        # Pickle by reference so process runners import the decorated object.
        return (_import_task, (self.fn.__module__, self.fn.__qualname__))

    def __call__(self, *args, wait_for=None, **kwargs):
        if context.current_run() is None:
            return self.fn(*args, **kwargs)
        from .engine.runner import run_task

        return run_task(self, args, kwargs, wait_for=wait_for)

    def submit(self, *args, wait_for=None, **kwargs):
        """Run the task concurrently; returns a Future."""
        if context.current_run() is None:
            raise RuntimeError("task.submit() must be called inside a flow")
        from .engine.runner import submit_task

        return submit_task(self, args, kwargs, wait_for=wait_for)

    def map(self, iterable, *, wait_for=None, **static):
        """Submit one task run per element; returns a list of Futures."""
        return [self.submit(item, wait_for=wait_for, **static) for item in iterable]


def _import_task(module: str, qualname: str):
    import importlib

    obj = importlib.import_module(module)
    for part in qualname.split("."):
        obj = getattr(obj, part)
    return obj


def task(
    fn: Callable | None = None,
    **options,
):
    """Register a function as a task: each call inside a flow becomes a task run.

    Use as ``@task`` or ``@task(...)``. Outside a flow the function runs as-is. Inside a
    flow each call is recorded as a task run with a dynamic key such as ``load-0``,
    and ``submit`` and ``map`` run calls concurrently on the flow's runner.

    Args:
        fn (Callable): The function to wrap; supplied by the decorator syntax.
        name (str | None): Task name; defaults to the function name.
        description (str | None): Shown in the UI; defaults to the function's docstring.
        tags (Iterable[str]): Tags recorded on every task run.
        log_prints (bool): Tee ``print`` output into the run log at INFO.
        retries (int): How many times a failed task run is retried.
        retry_delay (float | list[float] | exponential): Wait before each retry: seconds, a list of per-attempt seconds,
            or `exponential`.
        timeout_seconds (float | None): Fail the task run with sub-state ``TimedOut`` after this many
            seconds.
        output (Target | Callable[..., Target] | None): A Target, or a callable over the task's arguments returning one; when
            the target exists the task run ends Skipped without executing.
        cache (CachePolicy | None): ``INPUTS``, ``SOURCE``, or ``INPUTS + SOURCE``: reuse a persisted result
            when the arguments, the source, or both are unchanged. Requires
            ``persist_result=True``.
        cache_expires (timedelta | None): A ``timedelta`` after which a cached result is stale.
        persist_result (bool): Store the return value under ``<home>/storage`` so cached and
            replayed runs can read it.
        serializer (str): ``"pickle"`` (default) or ``"json"`` for persisted results.
        resources (dict[str, float] | None): Named resources and the amount each task run holds, for example
            ``{"gpu": 1}``.
        on_completion (Iterable[Callable]): Hooks ``hook(task, run, state)`` called after a task run completes.
        on_failure (Iterable[Callable]): Hooks called after a task run fails.
        on_cancellation (Iterable[Callable]): Hooks called after a task run is cancelled.

    Returns:
        Task: The task wrapping ``fn``.
    """
    def decorate(func: Callable) -> Task:
        return Task(func, **options)

    if fn is not None:
        return decorate(fn)
    return decorate
