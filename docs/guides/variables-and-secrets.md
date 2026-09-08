# How to store variables and secrets

Keep configuration and credentials out of the code: set them once, read them at run time, and mark the sensitive ones secret.

## Set and read

```python
from cereyan import Variable, flow

Variable.set("batch_size", 500, tags=["tuning"])
Variable.set("regions", ["eu", "us"])

@flow
def load() -> int:
    size = Variable.get("batch_size", default=100)
    return size * len(Variable.get("regions"))

assert load() == 1000
```

Values are JSON: numbers, strings, lists, and objects. Names use lowercase letters, digits, `_`, `-`, `/`, and `.`, and values are limited to 64 KB. `Variable.unset(name)` deletes; `set(..., overwrite=False)` refuses to replace an existing value.

## Store a secret

```python
from cereyan import Variable

Variable.set("warehouse/password", "hunter2", secret=True)
assert Variable.get("warehouse/password") == "hunter2"
```

A secret is encrypted at rest with the key in `<home>/secret.key`, created on first use with mode 0600, and shown masked in the UI, the API, and the MCP tools. `Variable.get` decrypts it inside your process. Back up `secret.key` with the database, and treat access to the home directory as access to the secrets.

## From the UI, the API, and an agent

The Variables page adds, edits, and deletes variables; `POST /api/variables`, `PATCH /api/variables/{name}`, and `DELETE /api/variables/{name}` do the same; `Client.variables()` lists them with secrets masked; and the MCP `set_variable` tool lets an agent create or overwrite one:

```{.python fixture:served}
names = {v["name"] for v in served.client.variables()}
assert isinstance(names, set)
```

## Scope

Variables are global to the machine, not per project. Prefix names by project when two projects would otherwise collide, as in `warehouse/password` above.

## Offline and served

Offline, `Variable.set` and `get` use the store directly. While a server holds the store, the same calls go through its API and the running server sees the change at once, so a script can set a value that the next scheduled run reads.

Related: [Variables](../concepts/variables.md), [Configuration](../reference/configuration.md) for settings that belong in `cereyan.toml` instead.
