"""Concurrent task execution: futures, thread and process runners."""

from __future__ import annotations

import concurrent.futures
import contextvars
import multiprocessing
import os
import threading
from typing import Any, Callable

from .exceptions import CereyanError


class UpstreamFailed(CereyanError):
    """A future passed as an argument ended in failure."""


class Future:
    """Handle to a submitted task run."""

    def __init__(self, task, external_id: str, dynamic_key: str) -> None:
        self.task = task
        self.external_id = external_id
        self.dynamic_key = dynamic_key
        self._inner: concurrent.futures.Future = concurrent.futures.Future()
        self.state: dict | None = None
        self._runner = None

    def __repr__(self) -> str:
        return f"Future({self.dynamic_key!r}, done={self.done()})"

    def done(self) -> bool:
        """``True`` once the task run has finished, in any state."""
        return self._inner.done()

    def wait(self, timeout: float | None = None) -> None:
        """Block until the task run finishes or ``timeout`` seconds pass."""
        self._warn_if_deadlock()
        concurrent.futures.wait([self._inner], timeout=timeout)

    def result(self, timeout: float | None = None) -> Any:
        """The task's return value, blocking until it is available; re-raises the task's exception on failure."""
        self._warn_if_deadlock()
        return self._inner.result(timeout=timeout)

    def exception(self, timeout: float | None = None):
        """The exception the task raised, or ``None``; blocks like `result`."""
        return self._inner.exception(timeout=timeout)

    def _warn_if_deadlock(self) -> None:
        runner = self._runner
        if runner is not None and not self.done():
            runner.check_deadlock()

    # internal
    def _set_result(self, value: Any) -> None:
        if not self._inner.done():
            self._inner.set_result(value)

    def _set_exception(self, exc: BaseException) -> None:
        if not self._inner.done():
            self._inner.set_exception(exc)


def _is_future(value: Any) -> bool:
    return isinstance(value, Future)


def collect_futures(args: tuple, kwargs: dict, wait_for=None) -> list[Future]:
    """Every `Future` found in ``args``, ``kwargs`` (searching lists, tuples, sets, and dicts), and ``wait_for``."""
    found: list[Future] = []

    def visit(v):
        if _is_future(v):
            found.append(v)
        elif isinstance(v, (list, tuple, set)):
            for x in v:
                visit(x)
        elif isinstance(v, dict):
            for x in v.values():
                visit(x)

    for a in args:
        visit(a)
    for a in kwargs.values():
        visit(a)
    for f in wait_for or []:
        if _is_future(f):
            found.append(f)
        elif isinstance(f, (list, tuple)):
            found.extend(x for x in f if _is_future(x))
    return found


def resolve_futures(value: Any) -> Any:
    """Replace every `Future` in ``value`` (recursively through lists, tuples, and dicts) with its result."""
    if _is_future(value):
        return value.result()
    if isinstance(value, list):
        return [resolve_futures(v) for v in value]
    if isinstance(value, tuple):
        return tuple(resolve_futures(v) for v in value)
    if isinstance(value, dict):
        return {k: resolve_futures(v) for k, v in value.items()}
    return value


class BaseRunner:
    """Base class for runners that execute submitted tasks concurrently.

    Args:
        max_workers: Concurrent task runs allowed; defaults to the CPU count.
    """
    max_workers: int

    def __init__(self, max_workers: int | None = None) -> None:
        self.max_workers = max(1, int(max_workers or (os.cpu_count() or 4)))
        self._active = 0
        self._lock = threading.Lock()
        self._warned = False
        self.logger = None

    def start(self) -> None:
        """Prepare the runner at the start of a run."""
        pass

    def shutdown(self) -> None:
        """Release the runner's workers at the end of a run."""
        pass

    def check_deadlock(self) -> None:
        """Warn once when waiting inside a task while every worker is busy."""
        from . import context

        if context.current_task_run() is None:
            return
        with self._lock:
            saturated = self._active >= self.max_workers
            if saturated and not self._warned:
                self._warned = True
                import logging

                logging.getLogger("cereyan.run").warning(
                    "deadlock risk: a task is waiting on a child task while all %d runner workers are busy; "
                    "raise max_workers or avoid nested waits",
                    self.max_workers,
                )

    def submit(self, fn: Callable[[], Any]) -> concurrent.futures.Future:
        """Schedule ``fn`` for execution and return a ``concurrent.futures.Future`` for its result."""
        raise NotImplementedError

    def run_function(self, task, args: tuple, kwargs: dict, timeout: float | None):
        """Execute the task body for a submitted task; subclasses choose where."""
        raise NotImplementedError


class ThreadRunner(BaseRunner):
    """Runs submitted tasks on a bounded set of threads with the run context
    propagated. A task that waits on a child while every worker is busy gets a
    warning and a temporary extra worker so the wait cannot deadlock."""

    def __init__(self, max_workers: int | None = None) -> None:
        super().__init__(max_workers)
        self._queue: list = []
        self._cv = threading.Condition(self._lock)
        self._threads = 0
        self._idle = 0
        self._blocked = 0
        self._closed = False

    def start(self) -> None:
        """Open the runner for submissions."""
        self._closed = False

    def shutdown(self) -> None:
        """Stop accepting work and let idle threads exit."""
        with self._cv:
            self._closed = True
            self._cv.notify_all()

    def _capacity(self) -> int:
        return self.max_workers + self._blocked

    def _spawn_locked(self) -> None:
        self._threads += 1
        threading.Thread(target=self._worker, daemon=True, name="cereyan-task").start()

    def _worker(self) -> None:
        while True:
            with self._cv:
                while not self._queue and not self._closed:
                    self._idle += 1
                    self._cv.wait(timeout=1.0)
                    self._idle -= 1
                    if not self._queue and self._threads > self.max_workers:
                        self._threads -= 1
                        return
                if not self._queue:
                    self._threads -= 1
                    return
                fn, future, ctx = self._queue.pop(0)
                self._active += 1
            try:
                future.set_result(ctx.run(fn))
            except BaseException as exc:  # noqa: BLE001
                future.set_exception(exc)
            finally:
                with self._cv:
                    self._active -= 1

    def submit(self, fn: Callable[[], Any]) -> concurrent.futures.Future:
        """Queue ``fn`` on the thread pool, spawning a thread when none is idle and capacity remains."""
        future: concurrent.futures.Future = concurrent.futures.Future()
        ctx = contextvars.copy_context()
        with self._cv:
            self._queue.append((fn, future, ctx))
            if self._idle == 0 and self._threads < self._capacity():
                self._spawn_locked()
            self._cv.notify()
        return future

    def check_deadlock(self) -> None:
        from . import context

        if context.current_task_run() is None:
            return
        with self._cv:
            saturated = self._active >= self._capacity() and self._queue
            if not saturated:
                return
            if not self._warned:
                self._warned = True
                import logging

                logging.getLogger("cereyan.run").warning(
                    "deadlock risk: a task is waiting on a child task while all %d runner workers are busy; "
                    "raise max_workers or avoid nested waits (a temporary worker was added)",
                    self.max_workers,
                )
            self._blocked += 1
            self._spawn_locked()
            self._cv.notify()

    def run_function(self, task, args, kwargs, timeout):
        if timeout is None:
            return task.fn(*args, **kwargs)
        holder: dict = {}
        done = threading.Event()

        def body():
            try:
                holder["value"] = task.fn(*args, **kwargs)
            except BaseException as exc:  # noqa: BLE001
                holder["error"] = exc
            finally:
                done.set()

        t = threading.Thread(target=body, daemon=True, name="cereyan-task-body")
        t.start()
        if not done.wait(timeout):
            raise TimeoutError(f"task exceeded {timeout} s")
        if "error" in holder:
            raise holder["error"]
        return holder.get("value")


def _process_entry(task, args, kwargs, conn):
    try:
        fn = task.fn if hasattr(task, "fn") else task
        result = fn(*args, **kwargs)
        conn.send(("ok", result))
    except BaseException as exc:  # noqa: BLE001
        conn.send(("error", exc))
    finally:
        conn.close()


class ProcessRunner(BaseRunner):
    """Runs each submitted task in a spawned process; arguments and results
    must be picklable and the task function importable. Timeouts terminate
    the worker."""

    def __init__(self, max_workers: int | None = None) -> None:
        super().__init__(max_workers)
        self._slots = threading.Semaphore(self.max_workers)
        self._threads: list[threading.Thread] = []

    def submit(self, fn: Callable[[], Any]) -> concurrent.futures.Future:
        """Run ``fn`` on a helper thread that holds one of the process slots."""
        future: concurrent.futures.Future = concurrent.futures.Future()
        ctx = contextvars.copy_context()

        def wrapped():
            with self._slots:
                with self._lock:
                    self._active += 1
                try:
                    future.set_result(ctx.run(fn))
                except BaseException as exc:  # noqa: BLE001
                    future.set_exception(exc)
                finally:
                    with self._lock:
                        self._active -= 1

        t = threading.Thread(target=wrapped, daemon=True, name="cereyan-process-task")
        self._threads.append(t)
        t.start()
        return future

    def run_function(self, task, args, kwargs, timeout):
        mp = multiprocessing.get_context("spawn")
        parent, child = mp.Pipe(duplex=False)
        proc = mp.Process(target=_process_entry, args=(task, args, kwargs, child), daemon=True)
        proc.start()
        child.close()
        if parent.poll(timeout):
            status, payload = parent.recv()
            proc.join()
            if status == "error":
                raise payload
            return payload
        proc.terminate()
        proc.join(5)
        if proc.is_alive():
            proc.kill()
        raise TimeoutError(f"task exceeded {timeout} s and was killed")

    def shutdown(self) -> None:
        """Wait for outstanding helper threads to finish."""
        for t in self._threads:
            t.join(timeout=30)
        self._threads.clear()
