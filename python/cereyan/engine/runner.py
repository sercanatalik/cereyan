"""Run execution: create or receive a run, drive it through its states, and
record task runs and logs through a backend. Used offline (store backend)
and by engine children (reporter backend)."""

from __future__ import annotations

import inspect
import json
import os
import signal
import sys
import threading
import time
import traceback
from dataclasses import dataclass
from typing import Any

from .. import _core, context, names
from .. import logging as run_logging
from ..config import load_project_config
from ..exceptions import CereyanError, RunPaused
from ..results import ResultStore, cache_key
from ..runners import Future, ThreadRunner, UpstreamFailed, collect_futures, resolve_futures
from ..schedules import retry_delay_for
from ..targets import resolve_output
from .backends import Backend, RunRejected, StoreBackend

_store: _core.Store | None = None
_home_override: str | None = None
_last_outcome: dict | None = None
# Set while a server discovers modules and inside engine children: a flow
# called at import time (outside any run) must not start a new run.
_suppress_top_level_runs: bool = False
_suppress_reason: str = ""


def suppress_top_level_runs(enabled: bool, reason: str = "") -> None:
    global _suppress_top_level_runs, _suppress_reason
    _suppress_top_level_runs = enabled
    _suppress_reason = reason


class RunFailed(CereyanError):
    """A run executed by a server did not complete."""

    def __init__(self, run: dict) -> None:
        self.run = run
        state = run.get("state", {})
        message = state.get("message") or state.get("name", "failed")
        super().__init__(f"run {run.get('name')} ended {state.get('name')}: {message}")


class TaskCancelled(BaseException):
    """Raised inside a task body when the run is being cancelled."""


@dataclass
class RunInfo:
    id: int
    external_id: str
    name: str


def configure(home: str | None) -> None:
    """Set the home directory for this process (the ``--home`` flag)."""
    global _home_override
    _home_override = home


def resolved_home() -> str:
    return _core.Store.resolve_home(_home_override)


def get_store() -> _core.Store:
    global _store
    if _store is None:
        _store = _core.Store.open(_home_override)
    return _store


def close_store() -> None:
    """Release the store and its lock (used by tests and the CLI)."""
    global _store
    if _store is not None:
        try:
            _store.flush()
        finally:
            _store = None


def last_outcome() -> dict | None:
    """The run and task runs of the most recent flow call in this process."""
    return _last_outcome


def _set_outcome(run: dict | None, tasks: list[dict]) -> None:
    global _last_outcome
    _last_outcome = {"run": run, "tasks": tasks}


def _error_message(exc: BaseException) -> str:
    text = str(exc)
    return f"{type(exc).__name__}: {text}" if text else type(exc).__name__


def _failure_details(exc: BaseException) -> dict:
    return {"traceback": traceback.format_exc(), "exception": type(exc).__name__}


def register_flow(store: _core.Store, flow) -> int:
    load_project_config(flow.source_dir)
    return store.upsert_flow(
        flow.project,
        flow.name,
        flow.module,
        flow.source_dir,
        flow.description,
        json.dumps(flow.tags),
        json.dumps(flow.schema),
        json.dumps(flow.options),
    )


def run_hooks(hooks, owner, run: dict | Any, state: dict, logger) -> None:
    """Call hooks with (flow_or_task, run, state); errors are logged only."""
    for hook in hooks or ():
        try:
            hook(owner, run, state)
        except Exception:  # noqa: BLE001
            logger.error("hook %s raised:\n%s", getattr(hook, "__name__", hook), traceback.format_exc())


def _run_dict(ctx: context.RunContext) -> dict:
    return {
        "id": ctx.id,
        "external_id": ctx.external_id,
        "name": ctx.name,
        "flow": ctx.flow.name,
        "project": ctx.project,
        "parameters": ctx.parameters,
    }


class _FlowTimeout:
    """SIGALRM-based flow timeout for the main thread (offline path)."""

    def __init__(self, seconds: float | None) -> None:
        self.seconds = seconds
        self.previous = None
        self.active = bool(seconds) and threading.current_thread() is threading.main_thread() and hasattr(signal, "setitimer")

    def __enter__(self):
        if self.active:
            def handler(signum, frame):
                raise TimeoutError(f"flow exceeded {self.seconds} s")

            self.previous = signal.signal(signal.SIGALRM, handler)
            signal.setitimer(signal.ITIMER_REAL, self.seconds)
        return self

    def __exit__(self, *exc):
        if self.active:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, self.previous)
        return False


def _wait_for_futures(ctx: context.RunContext, logger) -> None:
    pending = [f for f in list(ctx.futures) if not f.done()]
    if pending:
        logger.info("waiting for %d unfinished task(s)", len(pending))
        for f in pending:
            try:
                f.wait()
            except Exception:  # noqa: BLE001
                pass


def execute_run(flow, values: dict[str, Any], backend: Backend, run: RunInfo) -> Any:
    """Drive one run: Pending, Running, the body (with retries), then a terminal state.

    Raises the flow's exception after recording Failed. Returns None without
    raising when the server rejected a transition (the run adopts the
    server's state) or when the run was cancelled cooperatively.
    """
    runner = flow.runner or ThreadRunner()
    runner.start()
    ctx = context.RunContext(
        id=run.id,
        external_id=run.external_id,
        name=run.name,
        flow=flow,
        parameters=values,
        backend=backend,
        runner=runner,
    )
    token = context.set_run(ctx)
    handler = run_logging.install(backend, run.id)
    logger = run_logging.get_run_logger()
    result = None
    run_info = _run_dict(ctx)
    try:
        try:
            backend.transition_run("Pending")
            backend.transition_run("Running")
        except RunRejected as exc:
            logger.warning("run %s not started: %s", run.name, exc)
            return None
        logger.info("run %s of flow %s started (pid %s)", run.name, flow.name, os.getpid())
        attempt = 0
        while True:
            try:
                with run_logging.capture_prints(flow.log_prints), _FlowTimeout(flow.timeout_seconds if backend.offline else None):
                    result = flow.fn(**values)
                _wait_for_futures(ctx, logger)
                break
            except RunPaused as exc:
                # Waiting for a person: end the attempt cleanly, no hooks.
                _wait_for_futures(ctx, logger)
                from ..inputs import pause_details

                try:
                    backend.transition_run("Paused", None, exc.prompt, pause_details(exc))
                except RunRejected as rejected:
                    logger.warning("run %s could not pause: %s", run.name, rejected)
                    return None
                logger.info("run %s paused for input: %s", run.name, exc.prompt)
                return None
            except KeyboardInterrupt as exc:
                if backend.cancel_requested():
                    state = backend.transition_run("Cancelled", None, "cancelled", None)
                    logger.warning("run %s cancelled", run.name)
                    run_hooks(flow.on_cancellation, flow, run_info, state, logger)
                    return None
                backend.transition_run("Failed", None, "KeyboardInterrupt", _failure_details(exc))
                raise
            except BaseException as exc:
                _wait_for_futures(ctx, logger)
                timed_out = isinstance(exc, TimeoutError) and flow.timeout_seconds and "flow exceeded" in str(exc)
                if attempt < flow.retries and not timed_out:
                    delay = retry_delay_for(flow.retry_delay, attempt)
                    attempt += 1
                    try:
                        backend.transition_run("Scheduled", "AwaitingRetry", _error_message(exc), {"attempt": attempt, "delay": delay, "retries": flow.retries, **_failure_details(exc)})
                    except RunRejected:
                        raise exc
                    logger.warning("run %s failed (%s); retry %d/%d in %.1fs", run.name, _error_message(exc), attempt, flow.retries, delay)
                    if delay > 0:
                        time.sleep(delay)
                    backend.transition_run("Running", "Retrying")
                    continue
                try:
                    if timed_out:
                        state = backend.transition_run("Failed", "TimedOut", _error_message(exc), _failure_details(exc))
                    else:
                        state = backend.transition_run("Failed", None, _error_message(exc), _failure_details(exc))
                except RunRejected:
                    state = {"type": "Failed", "name": "Failed", "message": _error_message(exc)}
                logger.error("run %s failed: %s", run.name, _error_message(exc))
                run_hooks(flow.on_failure, flow, run_info, state, logger)
                raise
        try:
            state = backend.transition_run("Completed")
        except RunRejected as exc:
            logger.warning("run %s completion rejected: %s", run.name, exc)
            return result
        logger.info("run %s completed", run.name)
        run_hooks(flow.on_completion, flow, run_info, state, logger)
        return result
    finally:
        try:
            runner.shutdown()
        except Exception:  # noqa: BLE001
            pass
        run_logging.uninstall(handler)
        context.reset_run(token)
        backend.flush()


def run_flow(flow, args: tuple, kwargs: dict) -> Any:
    """A flow call outside any run: execute offline, or hand off to a live server."""
    if context.current_run() is not None:
        # Nested flow calls execute inline within the enclosing run.
        values = flow.bind(args, kwargs)
        return flow.fn(**values)

    if _suppress_top_level_runs:
        print(
            f"cereyan: ignoring call to flow {flow.name!r} at import time ({_suppress_reason}); "
            "guard script-level calls with `if __name__ == '__main__':`",
            file=sys.stderr,
        )
        return None

    values = flow.bind(args, kwargs)
    try:
        store = get_store()
    except _core.StoreLocked as locked:
        return _handoff(flow, values, locked)

    flow_id = register_flow(store, flow)
    run_name = flow.render_run_name(values) or names.generate(store)
    run_id, external_id = store.create_run(flow_id, run_name, flow.parameters_json(values), json.dumps(flow.tags))
    backend = StoreBackend(store, run_id)
    try:
        return execute_run(flow, values, backend, RunInfo(run_id, external_id, run_name))
    finally:
        backend.close()
        run_json = store.get_run(run_id)
        _set_outcome(json.loads(run_json) if run_json else None, json.loads(store.task_runs(run_id)))


def _handoff(flow, values: dict[str, Any], locked: Exception) -> Any:
    """Submit the run to the server that holds the lock and follow it."""
    from .. import client as client_module

    server = client_module.find_server(_home_override)
    if server is None:
        raise CereyanError(
            f"{locked} No live server was found in server.json either; stop the other process or start `cereyan serve`."
        ) from None
    load_project_config(flow.source_dir)
    run_name = flow.render_run_name(values)
    run = server.submit(
        flow.project,
        flow.name,
        json.loads(flow.parameters_json(values)),
        name=run_name,
        module=flow.module,
        source_dir=flow.source_dir,
        description=flow.description,
        parameter_schema=flow.schema,
        options=flow.options,
        flow_tags=flow.tags,
        created_by="script",
    )
    run = follow_run(server, run["id"])
    tasks = server.task_runs(run["id"])
    _set_outcome(run, tasks)
    if run["state"]["type"] != "Completed":
        raise RunFailed(run)
    return None


_LEVELS = {10: "DEBUG", 20: "INFO", 30: "WARNING", 40: "ERROR", 50: "CRITICAL"}


def _print_log_rows(rows, out) -> int:
    last = 0
    for row in rows:
        last = row["id"]
        stamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(row["timestamp"] / 1_000_000))
        level = _LEVELS.get(row["level"], str(row["level"]))
        print(f"{stamp} {level:<7} {row['message']}", file=out)
    return last


def follow_run(server, run_id: int, out=None, poll: float = 0.2) -> dict:
    """Stream a server-side run's logs to the terminal until it ends."""
    out = out or sys.stderr
    after = 0
    while True:
        page = server.logs(run_id, after=after, limit=500)
        if page["items"]:
            after = _print_log_rows(page["items"], out)
        if page.get("next_cursor"):
            continue
        run = server.get_run(run_id)
        if _core.is_terminal(run["state"]["type"]):
            page = server.logs(run_id, after=after, limit=500)
            _print_log_rows(page["items"], out)
            return run
        time.sleep(poll)


# ---------------------------------------------------------------------------
# tasks


def _acquire_resources(run: context.RunContext, task, logger):
    """Task-level resources: lease through the server, or a local semaphore offline."""
    if not task.resources:
        return None
    backend = run.backend
    return backend.acquire_resources(task.resources, logger)


def _release_resources(run: context.RunContext, lease) -> None:
    if lease is not None:
        run.backend.release_resources(lease)


def _bind_task_values(task, args: tuple, kwargs: dict) -> dict[str, Any]:
    try:
        bound = inspect.signature(task.fn).bind_partial(*args, **kwargs)
        return dict(bound.arguments)
    except TypeError:
        return dict(kwargs)


def _drive_generator(gen, logger):
    """Run a generator task: yielded futures or values are resolved and sent back."""
    try:
        sent = None
        while True:
            yielded = gen.send(sent)
            sent = resolve_futures(yielded)
    except StopIteration as stop:
        return stop.value


def _execute_task_body(run: context.RunContext, task, args: tuple, kwargs: dict, external_id: str, logger) -> Any:
    """Run one task attempt including output, cache, timeout and generator handling."""
    values = _bind_task_values(task, args, kwargs)
    target = resolve_output(task.output, values)
    if target is not None and target.exists():
        logger.info("task %s skipped: output %r exists", task.name, target)
        run.backend.transition_task_run(external_id, "Completed", "Skipped", f"output exists: {target!r}", None)
        return target, True

    store = None
    key = None
    if task.persist_result:
        store = ResultStore(resolved_home())
        if task.cache:
            key = cache_key(task.key, task.fn, task.cache, values)
            hit, cached = store.read(key)
            if hit:
                logger.info("task %s cached", task.name)
                run.backend.transition_task_run(external_id, "Completed", "Cached", f"cache key {key[:12]}", None)
                return cached, True
        if key is None:
            key = f"{run.external_id}-{external_id}"

    runner = run.runner
    if runner is None or type(runner).__name__ == "ThreadRunner" or not getattr(runner, "_process_only", False):
        result = runner.run_function(task, args, kwargs, task.timeout_seconds) if runner else task.fn(*args, **kwargs)
    else:
        result = runner.run_function(task, args, kwargs, task.timeout_seconds)
    if inspect.isgenerator(result):
        result = _drive_generator(result, logger)
    if store is not None and key is not None:
        store.write(key, result, task.serializer, task.cache_expires)
    return result, False


def _run_task_attempts(run: context.RunContext, task, args: tuple, kwargs: dict, external_id: str, dynamic_key: str) -> Any:
    backend = run.backend
    logger = run_logging.get_run_logger()
    ctx = context.TaskRunContext(id=external_id, name=task.name, task_key=task.key, dynamic_key=dynamic_key)
    token = context.set_task_run(ctx)
    lease = None
    try:
        lease = _acquire_resources(run, task, logger)
        backend.transition_task_run(external_id, "Running")
        logger.info("task %s started", dynamic_key)
        attempt = 0
        while True:
            try:
                with run_logging.capture_prints(task.log_prints):
                    result, short_circuit = _execute_task_body(run, task, args, kwargs, external_id, logger)
                if short_circuit:
                    return result
                state = backend.transition_task_run(external_id, "Completed")
                logger.info("task %s completed", dynamic_key)
                run_hooks(task.on_completion, task, _run_dict(run), state, logger)
                return result
            except BaseException as exc:
                if isinstance(exc, RunPaused):
                    backend.transition_task_run(external_id, "Cancelled", None, "paused for input", None)
                    raise
                cancelled = isinstance(exc, (KeyboardInterrupt, TaskCancelled)) and backend.cancel_requested()
                timed_out = isinstance(exc, TimeoutError) and task.timeout_seconds and "exceeded" in str(exc)
                if cancelled:
                    state = backend.transition_task_run(external_id, "Cancelled", None, "cancelled", None)
                    run_hooks(task.on_cancellation, task, _run_dict(run), state, logger)
                    raise
                if attempt < task.retries and not timed_out and not isinstance(exc, KeyboardInterrupt):
                    delay = retry_delay_for(task.retry_delay, attempt)
                    attempt += 1
                    backend.transition_task_run(
                        external_id, "Scheduled", "AwaitingRetry", _error_message(exc), {"attempt": attempt, "delay": delay, "retries": task.retries, **_failure_details(exc)}
                    )
                    logger.warning("task %s failed (%s); retry %d/%d in %.1fs", dynamic_key, _error_message(exc), attempt, task.retries, delay)
                    if delay > 0:
                        time.sleep(delay)
                    backend.transition_task_run(external_id, "Running", "Retrying")
                    continue
                name = "TimedOut" if timed_out else None
                state = backend.transition_task_run(external_id, "Failed", name, _error_message(exc), _failure_details(exc))
                logger.error("task %s failed: %s", dynamic_key, _error_message(exc))
                run_hooks(task.on_failure, task, _run_dict(run), state, logger)
                raise
    finally:
        _release_resources(run, lease)
        context.reset_task_run(token)


def _create_task_run(run: context.RunContext, task, parents: list[str]):
    dynamic_key = run.next_dynamic_key(task.name)
    external_id, _row = run.backend.create_task_run(task.name, task.key, dynamic_key, parents)
    run.backend.transition_task_run(external_id, "Pending")
    return external_id, dynamic_key


def run_task(task, args: tuple, kwargs: dict, wait_for=None) -> Any:
    """A direct task call: waits for upstream futures, then runs inline."""
    run = context.current_run()
    assert run is not None and run.backend is not None
    upstream = collect_futures(args, kwargs, wait_for)
    external_id, dynamic_key = _create_task_run(run, task, [f.external_id for f in upstream])
    failed = _wait_upstream(upstream)
    if failed is not None:
        run.backend.transition_task_run(external_id, "Failed", None, "upstream task failed", {"upstream": failed.dynamic_key})
        raise UpstreamFailed(f"upstream task {failed.dynamic_key} failed")
    args = tuple(resolve_futures(a) for a in args)
    kwargs = {k: resolve_futures(v) for k, v in kwargs.items()}
    return _run_task_attempts(run, task, args, kwargs, external_id, dynamic_key)


def _wait_upstream(futures: list[Future]):
    for f in futures:
        try:
            f.wait()
            f.result()
        except BaseException:  # noqa: BLE001
            return f
    return None


def submit_task(task, args: tuple, kwargs: dict, wait_for=None) -> Future:
    run = context.current_run()
    assert run is not None and run.backend is not None and run.runner is not None
    upstream = collect_futures(args, kwargs, wait_for)
    external_id, dynamic_key = _create_task_run(run, task, [f.external_id for f in upstream])
    future = Future(task, external_id, dynamic_key)
    future._runner = run.runner
    run.futures.append(future)

    def body():
        failed = _wait_upstream(upstream)
        if failed is not None:
            run.backend.transition_task_run(external_id, "Failed", None, "upstream task failed", {"upstream": failed.dynamic_key})
            raise UpstreamFailed(f"upstream task {failed.dynamic_key} failed")
        resolved_args = tuple(resolve_futures(a) for a in args)
        resolved_kwargs = {k: resolve_futures(v) for k, v in kwargs.items()}
        return _run_task_attempts(run, task, resolved_args, resolved_kwargs, external_id, dynamic_key)

    inner = run.runner.submit(body)

    def done(f):
        try:
            future._set_result(f.result())
        except BaseException as exc:  # noqa: BLE001
            future._set_exception(exc)

    inner.add_done_callback(done)
    return future
