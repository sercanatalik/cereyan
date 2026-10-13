"""`CereyanDurability`: a Pydantic AI capability that records an agent's work in a cereyan run."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, ClassVar, Literal, get_args

from pydantic_ai.agent import EventStreamHandler, ParallelExecutionMode
from pydantic_ai.capabilities import WrapRunHandler
from pydantic_ai.durable_exec import JSON_CODEC, BaseDurabilityCapability, DurabilityEngineSpec
from pydantic_ai.models import Model
from pydantic_ai.run import AgentRunResult
from pydantic_ai.tools import AgentDepsT, RunContext

from .. import context
from ._backend import CereyanOperationBackend

CereyanParallelExecutionMode = Literal["sequential", "parallel_ordered_events"]
"""Tool-call execution modes that replay faithfully. ``'parallel'`` is left out: it emits
tool events in completion order, which can differ between a run and its replay."""


@dataclass(init=False)
class CereyanDurability(BaseDurabilityCapability[AgentDepsT]):
    """Make an agent durable by recording each model request and tool call as a task run.

    Run the agent from a flow body. Every model request, function or MCP tool call, and
    other durable operation becomes a task run of the flow's run, with its result
    checkpointed, so a retry, a resume after a pause, or a crash rerun replays the
    completed ones instead of asking the model again. Inside a `@task`, and outside any
    run, the capability does nothing and the agent runs as usual.

    Example:
        ```python {test="skip"}
        from pydantic_ai import Agent
        from cereyan import flow
        from cereyan.pydantic_ai import CereyanDurability

        agent = Agent("openai:gpt-5", name="analyst", capabilities=[CereyanDurability()])

        @flow(crash_retries=3)
        def analyse(question: str) -> str:
            return agent.run_sync(question).output
        ```

    Args:
        models: Extra models by id, for `agent.run(model="<id>")`; the id is part of each
            model request's name, so a replay resolves the same model.
        event_stream_handler: Handler for the agent's stream events. Model events are handled
            inside the model request's task run; each tool event in a task run of its own.
        name: The agent name every task-run name starts with. Defaults to the agent's `name`.
            Changing it stops replay for runs recorded under the old name.
        parallel_execution_mode: ``'sequential'`` (the default) or ``'parallel_ordered_events'``.

    Raises:
        ValueError: When ``parallel_execution_mode`` is not one of the two allowed modes.
    """

    engine_spec: ClassVar = DurabilityEngineSpec(
        engine_name="cereyan",
        durable_unit_noun="task run",
        durable_container_noun="flow run",
        codec=JSON_CODEC,
        wrapped_toolset_kinds=frozenset({"function", "mcp"}),
        toolset_lifecycles={"function": "enter-never", "mcp": "enter-never"},
        unsupported_runtime_toolset_kinds=frozenset({"function", "mcp", "dynamic"}),
    )

    def __init__(
        self,
        *,
        models: Mapping[str, Model] | None = None,
        event_stream_handler: EventStreamHandler[AgentDepsT] | None = None,
        name: str | None = None,
        parallel_execution_mode: CereyanParallelExecutionMode = "sequential",
    ) -> None:
        allowed = get_args(CereyanParallelExecutionMode)
        if parallel_execution_mode not in allowed:
            raise ValueError(
                f"parallel_execution_mode must be one of {', '.join(repr(m) for m in allowed)}, "
                f"not {parallel_execution_mode!r}: plain 'parallel' orders tool events by completion, "
                "which a replay does not repeat"
            )
        super().__init__(models=models, event_stream_handler=event_stream_handler, name=name)
        self._parallel_execution_mode: ParallelExecutionMode = parallel_execution_mode

    @property
    def in_durable_context(self) -> bool:
        """True in a flow body with a run in progress; false inside a task run or outside a run."""
        run = context.current_run()
        return run is not None and run.backend is not None and context.current_task_run() is None

    def get_durable_operation_backend(self) -> CereyanOperationBackend:
        return CereyanOperationBackend(agent_name=self.name, default_model_id=self.default_model_id)

    async def wrap_run(self, ctx: RunContext[AgentDepsT], *, handler: WrapRunHandler) -> AgentRunResult[Any]:
        """Apply the execution mode to every run, inside a flow or not."""
        agent = self.agent
        assert agent is not None
        with agent.parallel_tool_call_execution_mode(self._parallel_execution_mode):
            return await handler()
