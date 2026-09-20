---
title: cereyan
template: home.html
hide:
  - navigation
  - toc
---

```python
from datetime import date
from cereyan import flow, task

@task
def extract(day: date) -> list[int]:
    return [1, 2, 3]

@flow(run_name="etl-{day}")
def etl(day: date) -> int:
    return sum(extract(day))

assert etl(date(2026, 9, 6)) == 6   # recorded as a run in ~/.cereyan/db.sqlite
```
