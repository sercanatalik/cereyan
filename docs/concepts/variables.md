# Variables

```python
from cereyan import Variable, flow

Variable.set("region", "eu", tags=["infra"])
Variable.set("warehouse/api_token", "s3cret", secret=True)

@flow
def where() -> str:
    return f"{Variable.get('region')}:{Variable.get('warehouse/api_token')}"

assert where() == "eu:s3cret"
assert Variable.get("missing", default="none") == "none"
```

A **variable** is a small named JSON value, up to 64 KB, that flows read at run time: a region, a feature flag, a threshold, a credential. Variables are shared by every project on the machine; prefix names with the project (`warehouse/api_token`) when they should not be.

## Names and values

Names match `[a-z0-9][a-z0-9_./-]*`. Values are anything JSON-serialisable; dates and dataclasses are converted. Tags are free-form and only used for display.

## Secrets

`secret=True` encrypts the value at rest with a key kept at `<home>/secret.key` (created on first use, mode 0600) and masks it in the API, the UI, and the MCP `set_variable` tool. `Variable.get` decrypts it for the running process. Secrets stay on the machine: there is no remote secret store, and the key never leaves the home.

## Where they live and who can change them

Variables are rows in the store. Offline, `Variable.set` writes to the store directly; while a server holds the store, the same call goes through `POST /api/variables`. The Variables page and the API create, edit, and delete them, and an agent can create or overwrite one through MCP. `overwrite=False` makes `set` fail instead of replacing an existing value.

Related: [Store variables and secrets](../guides/variables-and-secrets.md), [App and projects](app-and-projects.md) for what is global.
