"""Where a run's transitions and logs go: straight into the store (offline)
or through the reporter to a server (engine child)."""

from __future__ import annotations

import json

from .. import _core
from ..exceptions import CereyanError


class RunRejected(CereyanError):
    """The rules rejected a flow-level transition; the run adopts the server's state."""

    def __init__(self, reason: str, current: dict | None) -> None:
        self.reason = reason
        self.current = current
        super().__init__(f"transition rejected: {reason}")


class Backend:
    run_id: int
    offline: bool = False

    def transition_run(self, state_type: str, name: str | None = None, message: str | None = None,
                       details: dict | None = None) -> dict:
        raise NotImplementedError

    def create_task_run(self, name: str, task_key: str, dynamic_key: str, parents: list[str] | None = None) -> tuple[str, int | None]:
        raise NotImplementedError

    def acquire_resources(self, resources: dict, logger):
        return None

    def release_resources(self, lease) -> None:
        pass

    def emit_event(self, name: str, payload_json: str, task_run_external_id: str | None, resource: dict | None) -> None:
        raise NotImplementedError

    def artifact(self, kind: str, data_json: str, key: str | None, task_run_external_id: str | None) -> str:
        raise NotImplementedError

    def transition_task_run(self, external_id: str, state_type: str, name: str | None = None,
                            message: str | None = None, details: dict | None = None) -> dict:
        raise NotImplementedError

    def log(self, task_run_external_id: str | None, level: int, logger: str, timestamp: int, message: str) -> None:
        raise NotImplementedError

    def flush_logs(self) -> None:
        pass

    def flush(self) -> None:
        pass

    def close(self) -> None:
        pass

    def cancel_requested(self) -> bool:
        return False

    def get_input(self):
        """The answer a resumed run was given, or None."""
        return None


def _details(details: dict | None) -> str | None:
    return json.dumps(details) if details else None


_local_semaphores: dict[str, "threading.BoundedSemaphore"] = {}
_local_lock = __import__("threading").Lock()


class StoreBackend(Backend):
    """Direct writes under the advisory lock."""

    offline = True

    def __init__(self, store: _core.Store, run_id: int, batch_size: int = 200) -> None:
        self.store = store
        self.run_id = run_id
        self.batch_size = batch_size
        self._rows: dict[str, int] = {}
        self._logs: list[tuple[int, int | None, int, str, int, str]] = []

    def acquire_resources(self, resources: dict, logger):
        # Offline there is one process: resources are local semaphores.
        import threading

        acquired = []
        for name, amount in sorted(resources.items()):
            with _local_lock:
                sem = _local_semaphores.get(name)
                if sem is None:
                    sem = _local_semaphores[name] = threading.BoundedSemaphore(1)
            for _ in range(max(1, int(amount))):
                sem.acquire()
                acquired.append(sem)
        return acquired

    def release_resources(self, lease) -> None:
        for sem in lease or []:
            sem.release()

    _RUN_EVENTS = {
        ("Scheduled", "Late"): "run.late", ("Running", "Retrying"): "run.retrying", ("Completed", "Skipped"): "run.skipped",
        "Scheduled": "run.scheduled", "Pending": "run.pending", "Running": "run.running", "Completed": "run.completed",
        "Failed": "run.failed", "Crashed": "run.crashed", "Cancelled": "run.cancelled",
    }

    def transition_run(self, state_type, name=None, message=None, details=None) -> dict:
        try:
            state = json.loads(self.store.transition(self.run_id, state_type, name, message, _details(details)))
        except _core.TransitionRejected as exc:
            raise RunRejected(str(exc), None) from None
        event = self._RUN_EVENTS.get((state["type"], state["name"])) or (
            self._RUN_EVENTS.get(state["type"]) if state["name"] not in ("AwaitingRetry", "AwaitingResource") else None
        )
        if event:
            self.store.append_event(event, json.dumps({"state": state["name"], "message": state.get("message")}), self.run_id, None, None)
            from ..rules import evaluate_offline, settle_expectations_offline

            evaluate_offline(self.store, event)
            if _core.is_terminal(state["type"]):
                settle_expectations_offline(self.store, self.run_id)
        return state

    def create_task_run(self, name, task_key, dynamic_key, parents=None):
        row_id, external_id = self.store.create_task_run(self.run_id, name, task_key, dynamic_key, list(parents or []))
        self._rows[external_id] = row_id
        return external_id, row_id

    _TASK_EVENTS = {("Completed", "Skipped"): "task_run.skipped", ("Completed", "Cached"): "task_run.cached",
                    "Running": "task_run.running", "Completed": "task_run.completed", "Failed": "task_run.failed"}

    def transition_task_run(self, external_id, state_type, name=None, message=None, details=None) -> dict:
        row_id = self._rows[external_id]
        state = json.loads(self.store.transition_task_run(row_id, state_type, name, message, _details(details)))
        event = self._TASK_EVENTS.get((state["type"], state["name"])) or self._TASK_EVENTS.get(state["type"])
        if event and state["name"] not in ("Retrying", "AwaitingRetry"):
            self.store.append_event(
                event, json.dumps({"state": state["name"], "task_run": external_id}), self.run_id, None,
                json.dumps({"kind": "task_run", "id": external_id, "name": ""}),
            )
        return state

    def log(self, task_run_external_id, level, logger, timestamp, message) -> None:
        row_id = self._rows.get(task_run_external_id) if task_run_external_id else None
        self._logs.append((self.run_id, row_id, level, logger, timestamp, message))
        if len(self._logs) >= self.batch_size:
            self.flush_logs()

    def emit_event(self, name, payload_json, task_run_external_id, resource):
        self.store.append_event(name, payload_json, self.run_id, None, json.dumps(resource) if resource else None, task_run_external_id)
        from ..rules import evaluate_offline

        evaluate_offline(self.store, name)

    def artifact(self, kind, data_json, key, task_run_external_id):
        row_id = self._rows.get(task_run_external_id) if task_run_external_id else None
        aid = self.store.upsert_artifact(self.run_id, kind, data_json, key, row_id)
        return str(aid)

    def flush_logs(self) -> None:
        if self._logs:
            batch, self._logs = self._logs, []
            self.store.append_logs(batch)

    def flush(self) -> None:
        self.flush_logs()
        self.store.flush()

    def close(self) -> None:
        self.flush()


class ReporterBackend(Backend):
    """Batched reporting to a server through the Rust client."""

    def __init__(self, client: _core.Client, run_id: int) -> None:
        self.client = client
        self.run_id = run_id
        self._artifact_ids: dict = {}
        client.begin_run(run_id)

    def transition_run(self, state_type, name=None, message=None, details=None) -> dict:
        accepted, body = self.client.transition_run(self.run_id, state_type, name, message, _details(details))
        data = json.loads(body) if body else {}
        if not accepted:
            raise RunRejected(data.get("reason", "rejected"), data.get("current"))
        return data.get("state", data)

    def create_task_run(self, name, task_key, dynamic_key, parents=None):
        external_id = _core.new_id()
        self.client.task_run_created(self.run_id, external_id, name, task_key, dynamic_key, list(parents or []))
        return external_id, None

    def acquire_resources(self, resources: dict, logger):
        body = json.dumps({"run_id": self.run_id, "resources": resources, "wait_ms": 30000})
        while True:
            status, text = self.client.post("/api/resources/acquire", body)
            data = json.loads(text) if text else {}
            if status < 300 and "lease" in data:
                return data["lease"]
            if status >= 300:
                raise CereyanError(f"resource acquire failed: {status} {text}")
            logger.info("waiting for resource %s", data.get("waiting"))

    def release_resources(self, lease) -> None:
        try:
            self.client.post("/api/resources/release", json.dumps({"run_id": self.run_id, "lease": lease}))
        except Exception:  # noqa: BLE001
            pass

    def transition_task_run(self, external_id, state_type, name=None, message=None, details=None) -> dict:
        return json.loads(
            self.client.task_run_transition(self.run_id, external_id, state_type, name, message, _details(details))
        )

    def log(self, task_run_external_id, level, logger, timestamp, message) -> None:
        self.client.log(self.run_id, task_run_external_id, level, logger, timestamp, message)

    def emit_event(self, name, payload_json, task_run_external_id, resource):
        self.client.emit_event(self.run_id, name, payload_json, task_run_external_id)

    def artifact(self, kind, data_json, key, task_run_external_id):
        ext = self._artifact_ids.get((key, task_run_external_id)) if key else None
        new_ext = self.client.artifact(self.run_id, kind, data_json, key, task_run_external_id, ext)
        if key:
            self._artifact_ids[(key, task_run_external_id)] = new_ext
        return new_ext

    def flush(self) -> None:
        self.client.flush(self.run_id)

    def close(self) -> None:
        self.client.end_run(self.run_id)

    def cancel_requested(self) -> bool:
        return self.client.cancel_requested(self.run_id)

    def get_input(self):
        status, text = self.client.get(f"/api/runs/{self.run_id}/input")
        if status >= 300 or not text:
            return None
        return json.loads(text).get("input")
