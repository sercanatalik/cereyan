"""Type stubs for the compiled ``cereyan._core`` extension."""

from __future__ import annotations

__version__: str

class StoreLocked(RuntimeError):
    """The store is locked by another process."""

class TransitionRejected(RuntimeError):
    """A state transition was rejected by the rules."""

class Store:
    @staticmethod
    def open(home: str | None = None) -> Store: ...
    @staticmethod
    def resolve_home(home: str | None = None) -> str: ...
    @property
    def home(self) -> str: ...
    def upsert_flow(
        self,
        project: str,
        name: str,
        module: str,
        source_dir: str,
        description: str | None = None,
        tags: str = "[]",
        parameter_schema: str = "{}",
        options: str = "{}",
        group: str | None = None,
    ) -> int: ...
    def get_flow(self, flow_id: int) -> str | None: ...
    def set_flow_error(self, flow_id: int, error: str | None) -> None: ...
    def query_logs(self, filter: str = "{}") -> str: ...
    def create_run(
        self, flow_id: int, name: str, parameters: str = "{}", tags: str = "[]"
    ) -> tuple[int, str]: ...
    def transition(
        self,
        run_id: int,
        state_type: str,
        name: str | None = None,
        message: str | None = None,
        details: str | None = None,
        force: bool = False,
    ) -> str: ...
    def create_task_run(
        self,
        run_id: int,
        name: str,
        task_key: str,
        dynamic_key: str,
        parents: list[str] | None = None,
    ) -> tuple[int, str]: ...
    def transition_task_run(
        self,
        task_run_id: int,
        state_type: str,
        name: str | None = None,
        message: str | None = None,
        details: str | None = None,
        force: bool = False,
    ) -> str: ...
    def append_logs(
        self, logs: list[tuple[int, int | None, int, str, int, str]]
    ) -> int: ...
    def list_runs(self, filter: str = "{}") -> str: ...
    def get_run(self, run_id: int) -> str | None: ...
    def task_runs(self, run_id: int) -> str: ...
    def logs(self, run_id: int, after_id: int = 0, limit: int = 1000) -> str: ...
    def list_flows(self, project: str | None = None) -> str: ...
    def append_event(
        self,
        name: str,
        payload: str = "{}",
        run_id: int | None = None,
        flow_id: int | None = None,
        resource: str | None = None,
        task_run_external_id: str | None = None,
    ) -> int: ...
    def query_events(self, filter: str = "{}") -> str: ...
    def upsert_artifact(
        self,
        run_id: int,
        kind: str,
        data: str,
        key: str | None = None,
        task_run_id: int | None = None,
    ) -> int: ...
    def artifacts(self, run_id: int) -> str: ...
    def set_variable(
        self, name: str, value: str, tags: str = "[]", secret: bool = False
    ) -> None: ...
    def get_variable(self, name: str) -> str | None: ...
    def delete_variable(self, name: str) -> bool: ...
    def list_variables(self) -> str: ...
    def list_rules(self) -> str: ...
    def upsert_rule(
        self,
        name: str,
        spec: str,
        source: str = "code",
        module: str | None = None,
        id: int | None = None,
        enabled: bool = True,
    ) -> int: ...
    def prune_code_rules(self, keep: list[int]) -> int: ...
    def record_firing(
        self,
        rule_id: int,
        event_id: int | None = None,
        run_id: int | None = None,
        outcomes: str = "[]",
    ) -> int: ...
    def run_name_exists(self, name: str) -> bool: ...
    def delete_run(self, run_id: int) -> bool: ...
    def flush(self) -> None: ...

def now_micros() -> int: ...
def new_id() -> str: ...
def state_types() -> list[str]: ...
def state_names() -> list[tuple[str, str]]: ...
def is_terminal(state_type: str) -> bool: ...
def decrypt_secret(home: str, text: str) -> str: ...
def render_rule_action(action: str, context: str) -> str: ...
def rule_matches(when: str, event: str, run: str) -> bool: ...

class Server:
    @staticmethod
    def start(
        store: Store,
        config: str,
        dispatcher: object | None = None,
        rule_dispatcher: object | None = None,
    ) -> Server: ...
    @property
    def port(self) -> int: ...
    @property
    def url(self) -> str: ...
    def wait(self, timeout: float) -> bool: ...
    def stop(self) -> None: ...

class Client:
    def __init__(self, base_url: str, engine_id: str, token: str | None = None) -> None: ...
    @property
    def base_url(self) -> str: ...
    def get(self, path: str) -> tuple[int, str]: ...
    def post(self, path: str, body: str) -> tuple[int, str]: ...
    def get_work(
        self,
        pid: int,
        source_dir: str,
        module: str,
        isolated: bool = False,
        wait_ms: int = 30000,
        nice: int = 0,
    ) -> str: ...
    def begin_run(self, run_id: int) -> None: ...
    def end_run(self, run_id: int) -> None: ...
    def transition_run(
        self,
        run_id: int,
        state_type: str,
        name: str | None = None,
        message: str | None = None,
        details: str | None = None,
        force: bool = False,
    ) -> tuple[bool, str]: ...
    def task_run_created(
        self,
        run_id: int,
        external_id: str,
        name: str,
        task_key: str,
        dynamic_key: str,
        parents: list[str] | None = None,
    ) -> None: ...
    def emit_event(
        self,
        run_id: int,
        name: str,
        payload: str = "{}",
        task_run_external_id: str | None = None,
    ) -> None: ...
    def artifact(
        self,
        run_id: int,
        kind: str,
        data: str,
        key: str | None = None,
        task_run_external_id: str | None = None,
        external_id: str | None = None,
    ) -> str: ...
    def task_run_transition(
        self,
        run_id: int,
        external_id: str,
        state_type: str,
        name: str | None = None,
        message: str | None = None,
        details: str | None = None,
        force: bool = False,
    ) -> str: ...
    def log(
        self,
        run_id: int,
        task_run_external_id: str | None,
        level: int,
        logger: str,
        timestamp: int,
        message: str,
    ) -> None: ...
    def flush(self, run_id: int) -> bool: ...
    def cancel_requested(self, run_id: int) -> bool: ...
    def heartbeat(self, run_id: int) -> bool: ...
    def dropped_logs(self, run_id: int) -> int: ...
    def report_failed(
        self, source_dir: str, module: str, isolated: bool, traceback: str, nice: int = 0
    ) -> None: ...
    def close(self) -> None: ...
