# Skill Authoring Guide

Skills are self-contained programs that SafeClaw discovers, manages, and
runs alongside the agent. Each skill lives in its own directory under
`$DATA_DIR/skills/<name>/` and is defined by a `skill.toml` manifest.

## Quick start

Create a directory, add a manifest and an entrypoint:

```
skills/
└── hello-world/
    ├── skill.toml
    └── main.py
```

```toml
# skill.toml
name = "hello-world"
description = "Prints a greeting every 30 seconds"
version = "1.0.0"
skill_type = "daemon"
enabled = true
entrypoint = "main.py"
```

```python
# main.py
import time

while True:
    print("Hello from the skill!")
    time.sleep(30)
```

The skill will be discovered and started automatically on the next
reconciliation cycle (within 30 seconds).

---

## Manifest reference (`skill.toml`)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | **required** | Unique skill identifier |
| `description` | string | `""` | Human-readable description |
| `version` | string | `""` | Semantic version (enables snapshot/rollback) |
| `skill_type` | string | `"daemon"` | `"daemon"` (long-running) or `"oneshot"` (run once) |
| `enabled` | bool | `true` | Whether to auto-start |
| `entrypoint` | string | `"main.py"` | Script to run (relative to skill dir) |
| `venv` | string | `"auto"` | Python venv policy: `"auto"`, `"always"`, `"never"` |
| `env` | table | `{}` | Extra environment variables |
| `credentials` | array | `[]` | Declared credentials (see below) |
| `dependencies` | array | `[]` | Names of skills that must be running first |
| `sandbox` | table | (defaults) | Per-skill resource limits (see below) |

### `[[credentials]]`

```toml
[[credentials]]
name = "API_KEY"
label = "API Key"
description = "Third-party API key for the weather service"
required = true
```

Credentials are configured via the dashboard UI or REST API and injected
as environment variables when the skill starts.

### `[sandbox]`

```toml
[sandbox]
restrict_fs = true       # Restrict HOME/cwd to skill directory (default: true)
block_network = false     # Block outbound network (default: false)
max_memory_mib = 1024     # Max memory in MiB (default: 1024)
max_file_size_mib = 128   # Max file size in MiB (default: 128)
max_open_files = 128      # Max open file descriptors (default: 128)
```

### `dependencies`

```toml
dependencies = ["database-skill", "auth-skill"]
```

The skill will not start until all listed skills are running. The
reconciliation loop uses two-pass ordering to resolve the dependency
chain.

---

## Environment variables

Every skill receives these environment variables automatically:

| Variable | Description |
|----------|-------------|
| `SKILL_NAME` | The skill's name from the manifest |
| `SKILL_DIR` | Absolute path to the skill directory |
| `SKILL_DATA_DIR` | Absolute path to `<skill_dir>/data/` |
| `SKILLS_DIR` | Absolute path to the parent skills directory |
| `TELEGRAM_BOT_TOKEN` | Telegram bot token (if configured) |
| `TELEGRAM_CHAT_ID` | Telegram chat ID (if configured) |
| `TUNNEL_URL` | Ngrok public URL (if active) |
| `PUBLIC_URL` | Same as `TUNNEL_URL` |

Plus any `[env]` variables from the manifest and all configured credentials.

---

## Python skills

Python is the default skill runtime. The entrypoint should end in `.py`.

### Virtual environments

Set `venv = "auto"` (default) to automatically create a `.venv/` when
`requirements.txt` exists. The skill manager:

1. Creates `.venv/` via `python3 -m venv`
2. Upgrades pip
3. Installs `requirements.txt`
4. Runs the skill with the venv Python
5. Sets `PYTHONUNBUFFERED=1`

```
skills/
└── weather-bot/
    ├── skill.toml
    ├── main.py
    └── requirements.txt
```

```toml
# skill.toml
name = "weather-bot"
description = "Fetches weather data every hour"
version = "1.0.0"
entrypoint = "main.py"

[[credentials]]
name = "WEATHER_API_KEY"
label = "OpenWeather API Key"
required = true

[env]
CHECK_INTERVAL = "3600"
```

```python
# main.py
import os
import time
import requests

api_key = os.environ["WEATHER_API_KEY"]
interval = int(os.environ.get("CHECK_INTERVAL", "3600"))
data_dir = os.environ["SKILL_DATA_DIR"]

while True:
    resp = requests.get(
        "https://api.openweathermap.org/data/2.5/weather",
        params={"q": "London", "appid": api_key, "units": "metric"},
    )
    data = resp.json()
    temp = data["main"]["temp"]

    with open(os.path.join(data_dir, "latest.json"), "w") as f:
        import json
        json.dump(data, f)

    print(f"London: {temp}°C")
    time.sleep(interval)
```

```
# requirements.txt
requests>=2.31
```

### Oneshot Python skills

```toml
name = "data-migration"
skill_type = "oneshot"
entrypoint = "migrate.py"
```

Oneshot skills run once and exit. They are not restarted.

---

## Node.js skills

Use a `.js`, `.mjs`, or `.cjs` entrypoint. If `package.json` exists,
the manager runs `pnpm install` (or `npm install` as fallback) before
starting the skill.

```
skills/
└── discord-logger/
    ├── skill.toml
    ├── main.js
    └── package.json
```

```toml
# skill.toml
name = "discord-logger"
description = "Logs Discord messages to a file"
version = "1.0.0"
entrypoint = "main.js"

[[credentials]]
name = "DISCORD_TOKEN"
label = "Discord Bot Token"
required = true
```

```javascript
// main.js
const { Client, GatewayIntentBits } = require('discord.js');
const fs = require('fs');
const path = require('path');

const client = new Client({
  intents: [GatewayIntentBits.Guilds, GatewayIntentBits.GuildMessages, GatewayIntentBits.MessageContent]
});

const logPath = path.join(process.env.SKILL_DATA_DIR, 'messages.log');

client.on('messageCreate', (msg) => {
  const line = `[${new Date().toISOString()}] ${msg.author.tag}: ${msg.content}\n`;
  fs.appendFileSync(logPath, line);
});

client.login(process.env.DISCORD_TOKEN);
```

```json
{
  "name": "discord-logger",
  "dependencies": {
    "discord.js": "^14.0.0"
  }
}
```

---

## Shell skills

Any entrypoint that is not `.py`, `.js`, `.mjs`, `.cjs`, or `.rhai` is
run with `sh`. Make sure the script is executable or use a shebang.

```
skills/
└── backup-cron/
    ├── skill.toml
    └── backup.sh
```

```toml
# skill.toml
name = "backup-cron"
description = "Daily database backup"
skill_type = "oneshot"
entrypoint = "backup.sh"
```

```bash
#!/bin/bash
# backup.sh
set -euo pipefail

DB_PATH="${SKILL_DIR}/../safeclaw.db"
BACKUP_DIR="${SKILL_DATA_DIR}/backups"
mkdir -p "$BACKUP_DIR"

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
cp "$DB_PATH" "$BACKUP_DIR/safeclaw-${TIMESTAMP}.db"
echo "Backup created: safeclaw-${TIMESTAMP}.db"

# Clean up backups older than 7 days
find "$BACKUP_DIR" -name "*.db" -mtime +7 -delete
```

---

## Rhai skills

Rhai scripts (`.rhai` entrypoint) run in-process on a blocking thread.
They have zero startup overhead and access a rich API surface.

```
skills/
└── heartbeat/
    ├── skill.toml
    └── main.rhai
```

```toml
# skill.toml
name = "heartbeat"
description = "Sends a periodic Telegram heartbeat"
skill_type = "daemon"
entrypoint = "main.rhai"
```

```rhai
// main.rhai
loop {
    let now = now_utc();
    telegram_send("Agent heartbeat: " + now);
    log_info("heartbeat sent at " + now);
    sleep(300);  // 5 minutes (cooperative: checks cancel flag)
}
```

### Rhai skill API

| Category | Function | Description |
|----------|----------|-------------|
| HTTP | `http_get(url)` | GET request, returns parsed JSON or string |
| HTTP | `http_post(url, body)` | POST with string or map body |
| Files | `data_read(path)` | Read from skill's data directory |
| Files | `data_write(path, content)` | Write to skill's data directory |
| Files | `data_list(dir)` | List directory entries |
| Files | `data_exists(path)` | Check if a path exists |
| Env | `env_get(key)` | Read an environment variable |
| Telegram | `telegram_send(text)` | Send a Telegram message |
| Time | `now_utc()` | Current UTC time as ISO 8601 |
| Time | `now_epoch()` | Unix timestamp |
| Time | `sleep(secs)` | Cooperative sleep (checks cancel flag) |
| Logging | `log_info(msg)` | Write to skill log (info level) |
| Logging | `log_error(msg)` | Write to skill log (error level) |
| JSON | `json_parse(text)` | Parse JSON string |
| JSON | `json_stringify(val)` | Serialize to JSON |

---

## Skill extensions (dashboard UI + API)

Skills can extend the dashboard by registering Rhai HTTP endpoints and
serving custom HTML panels.

### Extension routes (`routes.rhai`)

Add a `routes.rhai` file to the skill directory:

```rhai
// routes.rhai
__routes.push(register_route("GET", "/status", "handle_status"));
__routes.push(register_route("POST", "/refresh", "handle_refresh"));

fn handle_status(req) {
    let data = data_read("status.json");
    let parsed = json_parse(data);
    json_response(#{
        last_check: parsed.last_check,
        items: parsed.items.len()
    })
}

fn handle_refresh(req) {
    let result = http_get("https://api.example.com/data");
    data_write("status.json", json_stringify(result));
    json_response(#{ ok: true, refreshed: now_utc() })
}
```

Routes are accessible at `/api/skills/{name}/ext/{path}`.

### Extension route API

| Function | Description |
|----------|-------------|
| `register_route(method, path, handler_fn)` | Register a route (push to `__routes`) |
| `json_response(data)` | 200 JSON response |
| `json_response(status, data)` | JSON response with status code |
| `html_response(html)` | 200 HTML response |
| `text_response(text)` | 200 plain text response |
| `error_response(status, message)` | JSON error response |
| `http_get(url)` | HTTP GET |
| `http_post(url, body)` | HTTP POST |
| `data_read(path)` | Read file |
| `data_write(path, content)` | Write file |
| `data_list(dir)` | List directory |
| `data_exists(path)` | Check existence |
| `json_parse(text)` | Parse JSON |
| `json_stringify(val)` | Serialize JSON |
| `json_stringify_pretty(val)` | Pretty-print JSON |
| `db_query(sql)` | SQL query, returns array of maps |
| `db_query(sql, params)` | Parameterized SQL query |
| `db_execute(sql)` | SQL execute |
| `db_execute(sql, params)` | Parameterized SQL execute |
| `env_get(key)` | Read environment variable |
| `now_utc()` | ISO 8601 timestamp |
| `now_epoch()` | Unix timestamp |

**Scope variables:**
- `__skill_name` — skill name
- `__skill_dir` — skill directory path
- `__data_dir` — skill data directory path

**Request object (`req`):**
- `req.method` — HTTP method
- `req.path` — matched path
- `req.body` — request body string
- `req.query` — map of query parameters
- `req.headers` — map of headers

### Dashboard panels (`[ui]` in skill.toml)

```toml
[ui]
panel = "panel.html"    # Shown in skill card (iframe)
page = "page.html"      # Full-page at /skills/{name}/page
style = "style.css"     # Injected CSS
script = "script.js"    # Injected JS
widget = "status"       # Widget type in card header
```

Create a `ui/` directory in the skill:

```html
<!-- ui/panel.html -->
<div id="status">Loading...</div>
<script>
  fetch(`/api/skills/${window.__skillName}/ext/status`, { credentials: 'same-origin' })
    .then(r => r.json())
    .then(data => {
      document.getElementById('status').textContent =
        `${data.items} items, last check: ${data.last_check}`;
    });
</script>
```

Static files are served at `/skills/{name}/ui/{path}`.

---

## Versioning and rollback

If your manifest includes a `version` field, snapshots are labeled with
that version. Otherwise, a timestamp-based label is generated.

**Create a snapshot** before making changes:
```
POST /api/skills/{name}/snapshot
→ { "ok": true, "version": "1.0.0" }
```

**List snapshots:**
```
GET /api/skills/{name}/versions
→ { "versions": ["1.0.0", "1.1.0"] }
```

**Rollback** to a previous version (automatically snapshots current state
first):
```
POST /api/skills/{name}/rollback
Body: { "version": "1.0.0" }
```

Snapshots are stored in `<skill_dir>/.versions/<version>/` and include
all source files except `.venv`, `data`, `node_modules`, `__pycache__`,
and `skill.log`.

---

## Hot reload

SafeClaw monitors `skill.toml`, the entrypoint file, and
`requirements.txt` for changes during every reconciliation cycle. When
a change is detected, the skill is automatically stopped and restarted.

No configuration is needed — hot reload is always active.

---

## Dependencies

Declare dependencies on other skills:

```toml
name = "api-server"
dependencies = ["database", "auth-service"]
```

The `api-server` skill will not start until both `database` and
`auth-service` are running. If a dependency stops, the dependent skill
continues running but will not be restarted if it crashes until the
dependency is running again.

---

## Process management

- Each skill runs in its own **Unix process group** (`setpgid(0, 0)`).
- Stop sends `SIGTERM` to the process group, waits 2 seconds, then
  sends `SIGKILL`.
- Resource limits (memory, file size, open files) are applied via
  `rlimit` in `pre_exec`, configurable per-skill via `[sandbox]`.
- `stdout` and `stderr` are redirected to `skill.log` in the skill
  directory.
- A `data/` directory is created automatically for persistent storage.

---

## Lifecycle

1. **Discovery** — the manager scans `$DATA_DIR/skills/` for directories
   containing `skill.toml`.
2. **Dependency check** — skills with unmet dependencies are deferred.
3. **Start** — venv setup (Python), `pnpm install` (Node.js), then the
   entrypoint is launched with environment variables and resource limits.
4. **Reconciliation** — every 30 seconds, the manager reaps finished
   processes, discovers new skills, hot-reloads changed skills, and
   starts/stops as needed.
5. **Stop** — `SIGTERM` → 2s grace → `SIGKILL` (process group).
6. **Shutdown** — all skills are stopped when the agent shuts down.

---

## Dashboard management

Skills can be managed via the dashboard UI or REST API:

| Action | API |
|--------|-----|
| List skills | `GET /api/skills` |
| Start | `POST /api/skills/{name}/start` |
| Stop | `POST /api/skills/{name}/stop` |
| Restart | `POST /api/skills/{name}/restart` |
| Enable/disable | `PUT /api/skills/{name}/enabled` |
| View logs | `GET /api/skills/{name}/log` |
| Edit manifest | `PUT /api/skills/{name}/manifest` |
| Set env var | `PUT /api/skills/{name}/env` |
| Set credential | `PUT /api/skills/{name}/credentials` |
| Snapshot | `POST /api/skills/{name}/snapshot` |
| Rollback | `POST /api/skills/{name}/rollback` |
| Import | `POST /api/skills/import` |
| Delete | `DELETE /api/skills/{name}` |

---

## Full example: RSS feed monitor

```
skills/
└── rss-monitor/
    ├── skill.toml
    ├── main.py
    └── requirements.txt
```

```toml
# skill.toml
name = "rss-monitor"
description = "Monitors RSS feeds and sends new items to Telegram"
version = "1.0.0"
skill_type = "daemon"
entrypoint = "main.py"

[env]
FEED_URL = "https://news.ycombinator.com/rss"
CHECK_INTERVAL = "300"

[sandbox]
max_memory_mib = 512
```

```python
# main.py
import os
import time
import json
import feedparser
import requests

feed_url = os.environ["FEED_URL"]
interval = int(os.environ.get("CHECK_INTERVAL", "300"))
data_dir = os.environ["SKILL_DATA_DIR"]
seen_file = os.path.join(data_dir, "seen.json")

telegram_token = os.environ.get("TELEGRAM_BOT_TOKEN")
chat_id = os.environ.get("TELEGRAM_CHAT_ID")

def load_seen():
    try:
        with open(seen_file) as f:
            return set(json.load(f))
    except (FileNotFoundError, json.JSONDecodeError):
        return set()

def save_seen(seen):
    with open(seen_file, "w") as f:
        json.dump(list(seen), f)

def notify(title, link):
    if telegram_token and chat_id:
        text = f"📰 {title}\n{link}"
        requests.post(
            f"https://api.telegram.org/bot{telegram_token}/sendMessage",
            json={"chat_id": chat_id, "text": text},
        )

seen = load_seen()

while True:
    try:
        feed = feedparser.parse(feed_url)
        for entry in feed.entries[:10]:
            guid = entry.get("id", entry.link)
            if guid not in seen:
                notify(entry.title, entry.link)
                seen.add(guid)
                print(f"New: {entry.title}")

        save_seen(seen)
    except Exception as e:
        print(f"Error: {e}")

    time.sleep(interval)
```

```
# requirements.txt
feedparser>=6.0
requests>=2.31
```
