# How to sign in with your identity provider

```python
from cereyan import App

app = App("reports")


@app.authenticator
def authenticate(credential: str) -> str | None:
    """The user's name for a valid credential, or None to reject it."""
    return {"demo-credential": "alice"}.get(credential)


assert authenticate("demo-credential") == "alice"
assert authenticate("anything else") is None
```

Register one authenticator per process. Cereyan passes it the credential a request carries and signs the request in as the name it returns. Your identity provider signs users in and sets the cookie; cereyan never redirects and has no login page of its own.

## Turn it on

```toml
[server]
enable_auth = true
auth_cookie = "SSO_SESSION"
login_url = "https://sso.example.com/login"
```

| Setting | Also | Meaning |
|---|---|---|
| `enable_auth` | `--enable-auth`, `CEREYAN_ENABLE_AUTH` | Call the authenticator. Default false. |
| `auth_cookie` | `--auth-cookie`, `CEREYAN_AUTH_COOKIE` | Cookie the credential comes from when there is no `Authorization: Bearer` header. |
| `auth_scope` | `--auth-scope`, `CEREYAN_AUTH_SCOPE` | `api` (default) or `all`; see below. |
| `login_url` | `--login-url`, `CEREYAN_LOGIN_URL` | Sign-in page linked from 401 responses and the UI. |

Registering an authenticator does not switch it on. Leave `enable_auth` off on your machine and set `CEREYAN_ENABLE_AUTH=true` where the server is shared. With `enable_auth` on and no authenticator registered, for instance because its module failed to import, the server refuses to start and names the modules that failed.

## Validate a JWT

<!-- notest: needs PyJWT and a real identity provider -->
```{.python notest}
import jwt  # PyJWT
from pipeline import app

keys = jwt.PyJWKClient("https://sso.example.com/.well-known/jwks.json", cache_keys=True)


@app.authenticator
def authenticate(credential: str) -> str | None:
    try:
        key = keys.get_signing_key_from_jwt(credential).key
        claims = jwt.decode(credential, key, algorithms=["RS256"], audience="cereyan")
    except jwt.PyJWTError:
        return None
    return claims["sub"]
```

The authenticator runs on a server thread for each request that does not carry the static token, so keep it fast: fetch the key set once and cache it, as `PyJWKClient` does. Returning `None`, returning anything but a non-empty string, or raising rejects the request with 401. A traceback goes to the server's output, never to the client.

## What is checked, in order

| Request | Result |
|---|---|
| Over the Unix socket | Allowed |
| `/api/health`, or a path outside the scope | Allowed |
| No bearer header and no `auth_cookie` cookie | 401; the authenticator is not called |
| Carrying the static token | Allowed, with no user |
| Carrying a credential the authenticator accepts | Allowed as that user |
| Anything else | 401 |

Engines send the static token, so they never reach the authenticator. With `enable_auth` on and no token configured, the server generates one for its engines at each start and never writes it down. The CLI sends a JWT as a bearer token, `cereyan --token <jwt> runs ls`, which on Windows, where there is no socket, is the way in besides a configured token.

## Cover the UI too

With `auth_scope = "all"` the check covers every path except `/api/health`: the UI, its assets, and custom routes outside `/api/`. A browser without the cookie gets a short "Not signed in" page linking to `login_url`. It needs `enable_auth`, because the static token alone would lock the browser out of the page that asks for it. With the default `api` scope, the UI loads and shows a "Not signed in" panel instead of the token prompt.

## Who started a run

A run started from the UI, the API, or MCP by a signed-in user records `created_by` as `user:<name>`, whatever the client sent. Runs started with the static token keep `api`, `client`, or `mcp:<client>`.

## Limits

- The live update stream is checked when it connects; a stream already open outlives the credential's expiry.
- The identity provider's cookie reaches cereyan only if its domain and path cover cereyan's URL, base path included.
- Everyone signed in can do everything a token holder can. There are no roles.

Related: [Secure the server](secure-the-server.md), [Configuration](../reference/configuration.md), [Custom routes](custom-routes.md)
