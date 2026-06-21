# Bridge API Reference

The `vibe-bridge` binary exposes a minimal HTTP API that serves session state and receives
hook events from Claude Code. All endpoints live on the same host/port pair configured in
`bridge/config.toml`.

---

## Base URL

```
http://{host}:{port}
```

Default (from `config.example.toml`):

```
http://127.0.0.1:5151
```

Override `host` and `port` in your local `bridge/config.toml`:

```toml
token = "your-secret-here"
host  = "0.0.0.0"    # bind on all interfaces for WiFi firmware mode
port  = 5151
```

---

## Authentication

Every endpoint requires the `X-VibeMonitor-Token` header. The value must exactly match
the `token` field in `config.toml`. Requests with a missing or incorrect token receive
`401 Unauthorized`.

Generate a strong token:

```sh
openssl rand -hex 32
```

---

## Response Format

All response bodies are JSON. Successful responses carry `Content-Type: application/json`.
Error responses carry a JSON object with a single `"error"` key.

---

## Endpoints

### `GET /state`

Returns a snapshot of all tracked sessions active within the last 4 hours, along with
usage information and collector health metadata. Sessions are sorted
Waiting → Working → Idle within the response array.

**Request**

```http
GET /state HTTP/1.1
Host: 127.0.0.1:5151
X-VibeMonitor-Token: your-secret-here
```

**Response 200 — `StateResponse`**

```json
{
  "ts": 1750000000,
  "sessions": [
    {
      "id": "abc123def456",
      "tool": "claude",
      "project": "cydrust",
      "status": "working",
      "ageSec": 12,
      "waiting": false,
      "waitingSec": null
    },
    {
      "id": "fed987cba654",
      "tool": "claude",
      "project": "myapp",
      "status": "waiting",
      "ageSec": 305,
      "waiting": true,
      "waitingSec": 42
    }
  ],
  "usage": {
    "claude": {
      "ok": false,
      "pct": null,
      "resetSec": null
    },
    "codex": {
      "ok": false,
      "pct": null,
      "resetSec": null
    }
  },
  "staleSec": 1
}
```

**Response 401**

```json
{"error": "unauthorized"}
```

**Field reference — `StateResponse`**

| Field | Type | Description |
|---|---|---|
| `ts` | `i64` | Unix timestamp (seconds) when this response was generated. |
| `sessions` | `SessionRow[]` | Active sessions, sorted by status priority. |
| `usage.claude.ok` | `bool` | `true` when Claude usage data is available (reserved for future use). |
| `usage.claude.pct` | `f64 \| null` | Claude token usage as a 0–1 fraction. |
| `usage.claude.resetSec` | `u64 \| null` | Seconds until Claude usage resets. |
| `usage.codex.*` | same | Same fields for Codex. |
| `staleSec` | `i64` | Seconds since the collector last completed a scan. `-1` if never run. |

**Field reference — `SessionRow`**

| Field | Type | Description |
|---|---|---|
| `id` | `string` | The `.jsonl` file stem (session UUID assigned by Claude Code). |
| `tool` | `string` | Always `"claude"` for now (reserved for future tools). |
| `project` | `string` | Derived from the parent directory name (last hyphen-delimited segment). |
| `status` | `"working" \| "idle" \| "waiting"` | See Status Values below. |
| `ageSec` | `u64` | Seconds since `last_activity` (the `.jsonl` mtime). |
| `waiting` | `bool` | `true` if a hook event has flagged this session as waiting for attention. |
| `waitingSec` | `u64 \| null` | Seconds since the session entered the waiting state. `null` if not waiting. |

---

### `POST /ack`

Clears the `waiting` flag for a session. Call this after acknowledging a notification
or resuming work on a session that was flagged by a hook event.

**Request**

```http
POST /ack HTTP/1.1
Host: 127.0.0.1:5151
X-VibeMonitor-Token: your-secret-here
Content-Type: application/json

{"id": "abc123def456"}
```

**Body**

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | `string` | Yes | Session ID to acknowledge. Must match an `id` from `/state`. |

**Response 200**

Empty body. The session's `waiting` flag and `waitingSec` are now cleared.

**Response 401**

```json
{"error": "unauthorized"}
```

**Behaviour notes:**
- Calling `POST /ack` on an ID that does not exist in the store is a no-op (returns 200).
- Calling `POST /ack` on a session that is not waiting is a no-op (returns 200).
- The store does not persist between bridge restarts; acks are in-memory only.

---

### `POST /hook`

Receives Claude Code hook events. This endpoint is designed to be registered as a Claude
Code hook handler (see [Claude Code hooks documentation](https://docs.anthropic.com/en/docs/claude-code/hooks)).

When the bridge receives a `Notification` or `Stop` event, it sets the corresponding
session to `waiting = true`, which lights up the amber indicator on the display.

**Request**

```http
POST /hook HTTP/1.1
Host: 127.0.0.1:5151
X-VibeMonitor-Token: your-secret-here
Content-Type: application/json

{
  "sessionId": "abc123def456",
  "hook_event_name": "Notification"
}
```

The endpoint accepts two equivalent field naming conventions (Claude Code versions differ):

| Accepted field | Meaning |
|---|---|
| `id` or `sessionId` | Session identifier |
| `event` or `hook_event_name` | Event type string |

**Body (Claude Code hook payload — relevant fields)**

| Field | Type | Notes |
|---|---|---|
| `id` | `string` | Session ID (alternative naming) |
| `sessionId` | `string` | Session ID (preferred naming) |
| `event` | `string` | Event type (alternative naming) |
| `hook_event_name` | `string` | Event type (`"Notification"`, `"Stop"`, etc.) |

**Events that trigger `waiting`:**

| Event | Effect |
|---|---|
| `Notification` | Marks session waiting (Claude needs attention) |
| `Stop` | Marks session waiting (Claude stopped, may need review) |
| Any other event | Accepted and silently ignored (returns 200) |

**Response 200**

Empty body.

**Response 401**

```json
{"error": "unauthorized"}
```

**Claude Code hook registration** (in `~/.claude/settings.json` or a project
`.claude/settings.json`):

```json
{
  "hooks": {
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://127.0.0.1:5151/hook -H 'X-VibeMonitor-Token: your-secret-here' -H 'Content-Type: application/json' -d '{\"sessionId\":\"$SESSION_ID\",\"hook_event_name\":\"Notification\"}'"
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://127.0.0.1:5151/hook -H 'X-VibeMonitor-Token: your-secret-here' -H 'Content-Type: application/json' -d '{\"sessionId\":\"$SESSION_ID\",\"hook_event_name\":\"Stop\"}'"
          }
        ]
      }
    ]
  }
}
```

---

## Status Values

| Value | Condition | Display |
|---|---|---|
| `"working"` | `ageSec < 60` and not waiting | `>>` symbol in green |
| `"idle"` | `ageSec >= 60` and not waiting | `z` symbol in grey |
| `"waiting"` | `waiting == true` (regardless of age) | `!` symbol in amber, card background darkened |

Sort order in `/state` response: `waiting` first, then `working`, then `idle`.

---

## Session Expiry

Sessions are **not** stored on disk. The in-memory store retains every `.jsonl` file it
has seen since the bridge started. The `/state` endpoint filters out sessions where:

```
now - last_activity > 14400  (4 hours)
```

Sessions are never explicitly deleted from the store; they simply stop appearing in
`/state` responses once expired. Restarting the bridge resets the store to empty.

---

## Rate Limiting

No rate limiting is implemented. The bridge is designed for local-network use only
(`127.0.0.1` or a private LAN). Do not expose port 5151 to the public internet.

---

## cURL Examples

**Get current state:**

```sh
curl -s \
  -H "X-VibeMonitor-Token: your-secret-here" \
  http://127.0.0.1:5151/state | jq .
```

**Acknowledge a waiting session:**

```sh
curl -s -X POST \
  -H "X-VibeMonitor-Token: your-secret-here" \
  -H "Content-Type: application/json" \
  -d '{"id": "abc123def456"}' \
  http://127.0.0.1:5151/ack
```

**Simulate a hook Notification event:**

```sh
curl -s -X POST \
  -H "X-VibeMonitor-Token: your-secret-here" \
  -H "Content-Type: application/json" \
  -d '{"sessionId": "abc123def456", "hook_event_name": "Notification"}' \
  http://127.0.0.1:5151/hook
```

**Pretty-print sessions with jq:**

```sh
curl -s \
  -H "X-VibeMonitor-Token: your-secret-here" \
  http://127.0.0.1:5151/state \
  | jq '.sessions[] | {project, status, ageSec}'
```

**Watch state every 2 seconds:**

```sh
watch -n 2 'curl -s -H "X-VibeMonitor-Token: your-secret-here" \
  http://127.0.0.1:5151/state | jq ".sessions"'
```

---

## Serial Bridge Mini Payload

When the `serial_bridge` binary serialises state for the ESP32, it strips the full
`/state` response down to a compact format (~80 bytes) that fits inside the ESP32's
128-byte UART FIFO:

```json
{
  "sessions": [
    {"project": "cydrust", "status": "working", "tool": "claude"}
  ],
  "claude": {"pct": 0.0},
  "codex":  {"pct": null}
}
```

The firmware parses only these three keys. `pct` is a 0–1 fraction; the display
multiplies by 100 to render `"42%"`. `null` for `codex.pct` means Codex is inactive
(not shown in the usage bar).
