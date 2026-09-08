import json
import os

PIPELINE = '''
from cereyan import flow, task
from datetime import date

@task
def ok():
    return 1

@task
def bad():
    raise RuntimeError("nope")

@flow
def etl(day: date, n: int = 1):
    ok()
    return str(day)

@flow
def mixed():
    ok()
    ok()
    bad()
'''


def test_run_with_parameters_exits_zero(run_cli, write_module):
    path = write_module("proj", PIPELINE)
    result = run_cli("run", f"{path}:etl", "--param", "day=2026-09-06")
    assert result.returncode == 0, result.stderr
    assert "run completed" in result.stdout
    assert "1  Completed" in result.stdout
    assert "started" in result.stderr


def test_unknown_flow_lists_flows_and_exits_3(run_cli, write_module):
    path = write_module("proj", PIPELINE)
    result = run_cli("run", f"{path}:missing")
    assert result.returncode == 3
    assert "etl" in result.stderr and "mixed" in result.stderr


def test_bad_parameter_exits_3(run_cli, write_module):
    path = write_module("proj", PIPELINE)
    result = run_cli("run", f"{path}:etl", "--param", "day=2026-09-06", "--param", "n=abc")
    assert result.returncode == 3
    assert "'n'" in result.stderr


def test_failed_run_exits_1_with_mixed_summary(run_cli, write_module):
    path = write_module("proj", PIPELINE)
    result = run_cli("run", f"{path}:mixed")
    assert result.returncode == 1
    assert "2  Completed" in result.stdout
    assert "1  Failed" in result.stdout
    assert "run failed" in result.stdout


def test_module_target_from_cwd(run_cli, write_module):
    path = write_module("proj", PIPELINE)
    result = run_cli("run", "pipeline:etl", "-p", "day=2026-01-01", cwd=str(path.parent))
    assert result.returncode == 0, result.stderr


def test_runs_ls_filters_and_json(run_cli, write_module):
    path = write_module("proj", PIPELINE)
    assert run_cli("run", f"{path}:etl", "-p", "day=2026-09-06").returncode == 0
    assert run_cli("run", f"{path}:mixed").returncode == 1
    table = run_cli("runs", "ls")
    assert table.returncode == 0
    assert "NAME" in table.stdout and "proj/etl" in table.stdout and "proj/mixed" in table.stdout
    failed = run_cli("runs", "ls", "--state", "Failed", "--json")
    items = json.loads(failed.stdout)
    assert [i["flow_name"] for i in items] == ["mixed"]
    by_flow = run_cli("runs", "ls", "--flow", "etl", "--json")
    assert [i["flow_name"] for i in json.loads(by_flow.stdout)] == ["etl"]
    by_project = run_cli("runs", "ls", "--project", "proj", "--json")
    assert len(json.loads(by_project.stdout)) == 2
    limited = run_cli("runs", "ls", "--limit", "1", "--json")
    assert len(json.loads(limited.stdout)) == 1
    assert run_cli("runs", "ls", "--project", "other", "--json").stdout.strip() == "[]"


def test_home_flag_beats_environment(run_cli, write_module, tmp_path):
    path = write_module("proj", PIPELINE)
    flag_home = tmp_path / "flag-home"
    env_home = tmp_path / "env-home"
    result = run_cli("--home", str(flag_home), "run", f"{path}:etl", "-p", "day=2026-09-06", home=env_home)
    assert result.returncode == 0, result.stderr
    assert (flag_home / "db.sqlite").exists()
    assert not env_home.exists()


def test_environment_beats_default(run_cli, write_module, tmp_path):
    path = write_module("proj", PIPELINE)
    fake_user_home = tmp_path / "user"
    fake_user_home.mkdir()
    result = run_cli(
        "run", f"{path}:etl", "-p", "day=2026-09-06",
        env={"HOME": str(fake_user_home)}, home=tmp_path / "env-home",
    )
    assert result.returncode == 0, result.stderr
    assert (tmp_path / "env-home" / "db.sqlite").exists()
    assert not (fake_user_home / ".cereyan").exists()


def test_default_home_under_user_home(run_cli, write_module, tmp_path):
    path = write_module("proj", PIPELINE)
    fake_user_home = tmp_path / "user"
    fake_user_home.mkdir()
    env = dict(os.environ)
    # `dirs::home_dir()` reads HOME on Unix and USERPROFILE on Windows, so faking
    # the user's home needs whichever the platform actually consults.
    env["HOME"] = str(fake_user_home)
    env["USERPROFILE"] = str(fake_user_home)
    env.pop("CEREYAN_HOME", None)
    import subprocess, sys

    result = subprocess.run(
        [sys.executable, "-m", "cereyan", "run", f"{path}:etl", "-p", "day=2026-09-06"],
        env=env, capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    assert (fake_user_home / ".cereyan" / "db.sqlite").exists()


def test_store_table_in_toml_is_rejected(run_cli, write_module):
    path = write_module("proj", PIPELINE)
    (path.parent / "cereyan.toml").write_text('[store]\nhome = "/elsewhere"\n')
    result = run_cli("run", f"{path}:etl", "-p", "day=2026-09-06")
    assert result.returncode == 3
    assert "CEREYAN_HOME" in result.stderr
