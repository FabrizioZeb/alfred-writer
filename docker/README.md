# Telemetry log viewer (Grafana + Loki + Alloy)

Alfred Writer records one JSON line per grammar check to
`%LOCALAPPDATA%\local\AlfredWriter\data\telemetry\checks.jsonl` (see `src/telemetry/`).
Nothing leaves your machine — this stack exists so you can *look at* those numbers.

The app itself cannot run in Docker (it's a Win32 GUI that hooks UI Automation).
Only the log pipeline is containerized:

```
checks.jsonl (host) ──▶ Alloy (tails file) ──▶ Loki (stores) ──▶ Grafana (dashboards)
```

## Run it

1. Check `.env` — `TELEMETRY_DIR` must point at your telemetry folder. The folder is
   created the first time the app records a check; if it doesn't exist yet, launch the
   app and trigger one check first (Docker errors on binding a missing host path).
2. From this folder:

   ```powershell
   docker compose up -d
   ```

3. Open <http://localhost:3000> → dashboard **Alfred Writer — Grammar Checks**
   (anonymous admin login is enabled — this stack is for localhost only; don't expose
   port 3000).

## What the fields mean

| Field | Meaning |
|---|---|
| `cache_path` | `full_hit` = exact text seen before (no provider call); `segments_hit` = every paragraph individually cached (no provider call); `provider` = at least one new paragraph was sent. |
| `segments_sent` / `segments_cached` | How much of the document actually went to the provider vs. was answered from the paragraph cache. |
| `provider_ms` | Wall-clock time inside the provider call (subprocess spawn-to-exit, or HTTP round trip). Only present when `cache_path=provider`. |
| `outcome` | `issues`, `clean`, `error`, `cancelled`, or `stale` (result arrived after the field changed again — a high stale rate means checks fire too eagerly). |

## Useful LogQL

```logql
# p95 provider latency
quantile_over_time(0.95, {job="alfred-writer"} | json | unwrap provider_ms [15m])

# cache hit ratio: everything not hitting the provider
sum(count_over_time({job="alfred-writer", cache_path!="provider"}[1h]))
  /
sum(count_over_time({job="alfred-writer"}[1h]))

# recent errors with their messages
{job="alfred-writer", outcome="error"} | json | line_format "{{.error}}"
```

## Teardown

```powershell
docker compose down        # keep stored logs
docker compose down -v     # wipe Loki/Grafana data too
```

