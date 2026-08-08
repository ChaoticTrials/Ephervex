# Microsoft / Xbox / Minecraft Services authentication fixtures

Response **shapes** for the Microsoft, Xbox, and Minecraft Services token
chain, transcribed on **2026-08-08** from the current official documentation
rather than captured from a live session.

## Why these are transcribed, not captured

Every response in this chain is a credential or contains one. A real capture
would put a working Microsoft refresh token, an Xbox Live JWT, and a Minecraft
access token into the repository. ADR 0004 forbids that disclosure; a committed
bearer token is unsafe regardless of which account supplied it.

So each file below reproduces the documented **structure** — every field name,
every nesting level, every type — with the secret-bearing values replaced by
obvious placeholders. That is all the parsers under test read: `ephervex-auth`
never validates a signature, never decodes a JWT, and never inspects a token
beyond passing it to the next leg. A transcribed fixture is therefore exactly as
strong a test as a captured one, and safe to commit.

**No value in this directory is or ever was a real credential.** Anything that
looks like a token is the string `FAKE-…` or a documentation placeholder.

## Sources

| File | Documented by | Endpoint |
|---|---|---|
| `microsoft-token.json` | Microsoft Learn, *OAuth 2.0 authorization code flow* (updated 2026-01-09) | `POST login.microsoftonline.com/consumers/oauth2/v2.0/token` |
| `microsoft-token-rotated.json` | same, "Refresh the access token" | the same endpoint issuing a **replacement** refresh token |
| `microsoft-device-code.json` | Microsoft Learn, *OAuth 2.0 device authorization grant* (updated 2025-01-04) | `POST.../oauth2/v2.0/devicecode` |
| `microsoft-authorization-pending.json` | same, "Expected errors" | `POST.../oauth2/v2.0/token` while the user is still signing in |
| `microsoft-authorization-declined.json` | same | the same endpoint after the user refused |
| `microsoft-expired-token.json` | same | the same endpoint after the 15-minute window closed |
| `microsoft-invalid-grant.json` | *authorization code flow*, "Error response" | `POST.../oauth2/v2.0/token` with a dead refresh token |
| `xbox-authenticate.json` | Minecraft Wiki, *Microsoft authentication* | `POST user.auth.xboxlive.com/user/authenticate` |
| `xsts-authorize.json` | same | `POST xsts.auth.xboxlive.com/xsts/authorize` |
| `xsts-denied.json` | same, `XErr` table | the same endpoint answering `401` |
| `minecraft-login.json` | same | `POST api.minecraftservices.com/authentication/login_with_xbox` |
| `minecraft-entitlements.json` | same | `GET api.minecraftservices.com/entitlements/mcstore` |
| `minecraft-entitlements-game-pass.json` | same | the same endpoint, for a Game Pass subscriber |
| `minecraft-entitlements-none.json` | same | the same endpoint, for an account with no Minecraft entitlement |
| `minecraft-profile.json` | same | `GET api.minecraftservices.com/minecraft/profile` |
| `minecraft-profile-not-found.json` | same | the same endpoint answering `404` |

## What was removed or replaced

* **Every token value.** Microsoft access tokens, refresh tokens, device codes,
  Xbox Live and XSTS JWTs, and Minecraft access tokens are all placeholders.
* **Entitlement signatures.** The live document carries a JWT per item plus a
  document-level one. Ephervex reads only `items[].name` — it neither validates
  nor stores the signatures — so they are reduced to a marker.
* **Correlation and trace ids** from the Microsoft error documents. They
  identify one request to Microsoft support and mean nothing here.
* **`skins` and `capes`** from the profile, along with their CDN URLs. Ephervex
  reads `id` and `name`; a skin URL would be dead weight that goes stale.
* **`id_token`** from the token responses. Ephervex requests neither `openid`
  nor `profile`, so a real response would not carry one.

## The two fixtures that exist to prove a distinction

`minecraft-entitlements-game-pass.json` paired with
`minecraft-profile-not-found.json` is the case that is easy to get wrong: an
account that **is** entitled through Game Pass but has never run the official
launcher has no Java Edition profile. Reporting "you do not own Minecraft"
there would be false, and `minecraft-entitlements-none.json` is what that
sentence is actually true of.
