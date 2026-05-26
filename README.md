<div align="center">

```
   ____    ___    _  __  _____   _  __  ___
  / ___|  / _ \  | |/ / | ____| | |/ / |_ _|
 | |  _  | | | | | ' /  |  _|   | ' /   | |
 | |_| | | |_| | | . \  | |___  | . \   | |
  \____|  \___/  |_|\_\ |_____| |_|\_\ |___|
```

**A high-performance REST facade over the 7TV emote API,
backed by PostgreSQL, Redis, and Azure Blob Storage.**

![Rust](https://img.shields.io/badge/rust-2021_edition-DEA584?style=flat-square&logo=rust&logoColor=white)
![Axum](https://img.shields.io/badge/axum-0.7-5C7AEA?style=flat-square)
![PostgreSQL](https://img.shields.io/badge/postgres-sqlx_0.8-336791?style=flat-square&logo=postgresql&logoColor=white)
![Redis](https://img.shields.io/badge/redis-7-DC382D?style=flat-square&logo=redis&logoColor=white)
![Azure](https://img.shields.io/badge/azure-blob_storage-0078D4?style=flat-square&logo=microsoftazure&logoColor=white)

</div>

---

## What it is

`rust-gokeki` is a cache-and-mirror layer in front of the [7TV GraphQL API](https://7tv.app/). It exposes a small, predictable REST surface and absorbs three things the upstream doesn't solve well for downstream products: **latency, durability, and access control**.

- **Latency** — emote searches and trending lists are served from Redis with TTLs tuned per endpoint, so a viral channel doesn't tip you over the upstream's rate limit.
- **Durability** — every emote image fetched is mirrored into your own Azure Blob container. When 7TV blips (and it does), your consumers don't notice.
- **Access control** — an admin surface protected by Bearer tokens or Basic Auth lets you trigger backfills, manage users, and inspect the dashboard, while the public read endpoints stay open.

---

## Features

- **Stable REST over a moving GraphQL.** Wraps 7TV's GraphQL surface in a small set of predictable JSON endpoints; clients don't have to relearn `defaultName` vs `name` every release.
- **Two-tier read path.** Redis fronts every public read; cold reads fall through to 7TV with bounded concurrency.
- **Local image mirror.** Every emote fetched is uploaded to your Azure Blob container, so consumers stay up when 7TV doesn't.
- **Atomic syncs.** Trending and per-user emote sets are written to Postgres with a single bulk `INSERT`, wrapped in a transaction; partial failures roll back cleanly.
- **TOCTOU-safe uploads.** Azure `PUT` with `If-None-Match: *` — two concurrent callers can't both overwrite a blob.
- **Two-mode admin auth.** Bearer token for scripts (`curl`, cron); Basic Auth for the browser dashboard. Both compared in constant time.
- **Fail-closed config.** The process refuses to start without `ADMIN_TOKEN`, `ADMIN_USER`, and `ADMIN_PASSWORD` set.
- **Bounded parallelism everywhere.** Image downloads, blob uploads, and prefix deletes all flow through `buffer_unordered` so the upstream isn't hammered.

---

## Architecture

```
                       ┌──────────────────┐
                       │  7TV GraphQL API │
                       └────────┬─────────┘
                                │  fetch
                                ▼
  ┌────────────┐         ┌──────────────────┐         ┌──────────────────┐
  │  Clients   │────────▶│   Axum router    │────────▶│  Azure Blob      │
  │            │         │                  │         │  Storage         │
  │  browsers  │         │  ┌────────────┐  │         │                  │
  │  curl      │◀────────│  │  public    │  │         │  emotes/         │
  │  scripts   │         │  │  /api/*    │  │         │  trending/       │
  │            │         │  ├────────────┤  │         │  <user-folder>/  │
  └────────────┘         │  │  admin     │  │         └──────────────────┘
                         │  │  /admin/*  │  │
                         │  │  Bearer or │  │
                         │  │  Basic     │  │
                         │  └────────────┘  │
                         └────┬────────┬────┘
                              │        │
                  ┌───────────┘        └────────────┐
                  ▼                                 ▼
        ┌────────────────────┐               ┌──────────────────┐
        │   PostgreSQL       │               │      Redis       │
        │                    │               │                  │
        │   users            │               │   search:*       │
        │   stickers         │               │   trending:*     │
        │   (GIN on tags)    │               │   user_emotes:*  │
        └────────────────────┘               └──────────────────┘
```

### Request lifecycle — public search

```
   client ──▶ POST /api/search-emotes
                       │
                       ▼
              check Redis (emote_search:<q>:<limit>:<animated>)
                       │
              ┌────────┴────────┐
            hit                miss
              │                  │
              │                  ▼
              │           POST gql to 7TV
              │                  │
              │                  ▼
              │           map → EmoteResponse[]
              │                  │
              │                  ▼
              │           write cache (TTL)
              │                  │
              └────────┬─────────┘
                       ▼
                  JSON to client
```

### Sync lifecycle — admin trending snapshot

```
   admin ──▶ POST /api/admin/sync-trending     (Bearer or Basic)
                       │
                       ▼
              7TV GraphQL: trending emotes
                       │
                       ▼
              stream images ─▶ Azure blobs
                 (buffer_unordered = 5,
                  If-None-Match: * upload)
                       │
                       ▼
              BEGIN
                DELETE FROM stickers WHERE folder_name = :db_folder
                INSERT INTO stickers VALUES (…),(…),(…)   ← single bulk insert
              COMMIT
                       │
                       ▼
              cache the manifest in Redis (24h TTL)
```

---

## Stack

| Layer        | Tech                       |
|--------------|----------------------------|
| HTTP         | `axum` 0.7                 |
| Async        | `tokio` 1                  |
| Database     | `sqlx` 0.8 + PostgreSQL    |
| Cache        | `redis` 0.27               |
| Object store | `azure_storage_blobs` 0.21 |
| HTTP client  | `reqwest` 0.12 (rustls)    |

---

<div align="center">

Built with care over a not-so-stable third party.

</div>
