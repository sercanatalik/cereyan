"""`settle`: ask about the in-doubt tool calls an agent run handed back, in the flow body."""

from __future__ import annotations

from pydantic_ai import DeferredToolRequests, DeferredToolResults, ToolDenied

from ..effects import IN_DOUBT_SCHEMA, _action
from ..exceptions import CereyanError, TaskInDoubt
from ..inputs import wait_for_input


def settle(requests: DeferredToolRequests, results: DeferredToolResults | None = None) -> DeferredToolResults:
    """Answer the in-doubt calls among an agent run's deferred tool requests.

    A once tool that a crash interrupted, and that has no probe, comes back from the agent
    run as an approval request. For each one, ask with `wait_for_input` whether its effect
    happened, then build the results for the next ``agent.run``: ``happened`` denies the call
    with a message carrying its result, so the model knows; ``run_again`` approves it, so it
    executes; ``fail`` raises. Call it from the flow body, where asking can pause the run.

    Args:
        requests: The ``DeferredToolRequests`` the agent run ended with.
        results: Results the flow already decided, such as approvals its own tools asked
            for; they are kept, and the in-doubt answers are added.

    Returns:
        DeferredToolResults: ``results`` with an approval for every in-doubt call.

    Raises:
        TaskInDoubt: When the answer for a call is ``fail``.
    """
    out = results if results is not None else DeferredToolResults()
    metadata = requests.metadata or {}
    for call in requests.approvals:
        meta = metadata.get(call.tool_call_id) or {}
        mark = meta.get("cereyan") if isinstance(meta, dict) else None
        if not (isinstance(mark, dict) and mark.get("in_doubt")):
            continue
        tool = mark.get("tool") or call.tool_name
        answer = wait_for_input(
            f"{tool} ({call.tool_call_id}) was interrupted in an earlier run and may have taken effect. "
            "Did its effect happen?",
            IN_DOUBT_SCHEMA,
        )
        action, result = _action(answer)
        if action == "happened":
            out.approvals[call.tool_call_id] = ToolDenied(
                f"Not executed again: the call had already taken effect. Result: {result!r}"
            )
        elif action == "run_again":
            out.approvals[call.tool_call_id] = True
        elif action == "fail":
            raise TaskInDoubt(f"{tool} ({call.tool_call_id}) was interrupted and may have taken effect; "
                              "the answer was to fail")
        else:
            raise CereyanError(f"an in-doubt answer needs action happened, run_again or fail, not {answer!r}")
    return out
