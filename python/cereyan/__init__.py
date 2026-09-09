"""Cereyan: a minimal, local-first orchestrator for Python data pipelines."""

from __future__ import annotations

from ._core import StoreLocked, TransitionRejected
from . import client, runtime
from .apps import App, get_default_app
from .exceptions import CereyanError, ParameterError
from .flows import Flow, flow
from .logging import get_run_logger
from . import artifacts
from .events import emit_event
from .inputs import wait_for_input
from .results import INPUTS, SOURCE, CachePolicy
from .routes import HTTPError, Request, Response
from .runners import Future, ProcessRunner, ThreadRunner
from .schedules import Cron, Interval, RRule, exponential
from .targets import LocalTarget, Target
from .tasks import Task, task
from .variables import Variable

__version__ = "1.11.0"

__all__ = [
    "wait_for_input",
    "App",
    "CachePolicy",
    "CereyanError",
    "Cron",
    "Future",
    "INPUTS",
    "Interval",
    "LocalTarget",
    "ProcessRunner",
    "RRule",
    "SOURCE",
    "Target",
    "ThreadRunner",
    "Variable",
    "artifacts",
    "emit_event",
    "exponential",
    "Flow",
    "HTTPError",
    "Request",
    "Response",
    "client",
    "ParameterError",
    "StoreLocked",
    "Task",
    "TransitionRejected",
    "flow",
    "get_default_app",
    "get_run_logger",
    "runtime",
    "task",
]


def __getattr__(name: str):
    if name == "app":
        return get_default_app()
    raise AttributeError(name)
