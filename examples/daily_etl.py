# ---
# title: Daily ETL
# description: A scheduled daily flow whose reruns and backfills are idempotent through targets.
# order: 2
# fixture: offline
# ---
#
# A daily flow: extract a day's data, write it to a file, and never do the same day
# twice. The target on `build` makes reruns skip finished days, `bulk_complete` lets a
# backfill skip them before runs are even created, and the schedule fires the flow
# every morning once a server is running.

from datetime import date, timedelta

from cereyan import Cron, LocalTarget, exponential, flow, get_run_logger, task


def output_for(day: date) -> LocalTarget:
    return LocalTarget(f"out/daily/{day}.csv")


# ## Skipping finished days
#
# `bulk_complete` receives every value a backfill is about to create and returns
# the ones already done; those runs are recorded as Skipped without dispatching.


def already_built(values: list[date]) -> set[date]:
    return {d for d in values if output_for(d).exists()}


# ## The task
#
# `output=` names the target; when it exists the task run ends Skipped and the body
# does not execute. Retries with an exponential delay cover transient failures.


@task(output=output_for, retries=2, retry_delay=exponential(base=0.5, maximum=30))
def build(day: date) -> str:
    target = output_for(day)
    get_run_logger().info("building %s", target.path)
    with target.open("w") as fh:
        fh.write("id,amount\n")
        for i in range(3):
            fh.write(f"{i},{(i + 1) * 10}\n")
    return target.path


# ## The flow
#
# Fires at 06:30 Istanbul time every day when served, never overlaps itself, and
# defaults to yesterday so a manual run does the most recent complete day.


@flow(
    schedule=Cron("30 6 * * *", timezone="Europe/Istanbul"),
    max_concurrent=1,
    on_overlap="skip",
    bulk_complete=already_built,
    run_name="daily-{day}",
)
def daily_etl(day: date = date.today() - timedelta(days=1)) -> str:
    build(day)
    return output_for(day).path


# ## Run it twice
#
# The second call for the same day skips `build` because its file exists; a skipped
# task returns `None`, so the flow returns the path itself.

if __name__ == "__main__":
    day = date(2026, 1, 15)
    first = daily_etl(day)
    second = daily_etl(day)
    assert first == second
    assert output_for(day).exists()
    print("built", first)
