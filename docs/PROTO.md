# Application Protocol Design

## Overview

Components communicate via **ZeroMQ Pub/Sub**, exchanging binary messages serialized with **rkyv** on specific topics. Subscribers filter by topic and deserialize binary payloads to `Vec<Message>`.

This replaces plain JSON messaging with efficient zero-copy serialization, reducing message size and CPU overhead on the wire.

> **Note:** The actual wire format is rkyv binary, not JSON. JSON examples below are illustrative only.

## Transport

Each ZMQ message is a multipart message:

```
[topic][rkyv_bytes]
```

- `topic` — UTF-8 string, see [Topics](#topics).
- `rkyv_bytes` — serialized `Vec<Message>`.

## Topics

`src/zmq/topic.rs`

| Variant | String | Usage |
|---|---|---|
| `Auth` | `auth` | Hysteria2 token distribution and deletes. Consumed by the `auth` binary. |
| `Metrics` | `metrics` | `MetricEnvelope` batches from nodes/auth to the API. |
| `Updates(Env)` | `updates-<env>` | Protocol state changes targeted at nodes in a specific environment (`production`, `experimental`, `dev`, `ru`, `wl`, `custom…`). |
| `Init(uuid)` | `init-<uuid>` | Per-node initial sync requested by `POST /connections/sync`. |

A node subscribes to `updates-<node_env>` and `init-<node_uuid>`. The `auth` service subscribes to `auth` and `init-<auth_uuid>`.

## Message struct

`src/zmq/message.rs`

```rust
pub struct Message {
    pub conn_id: uuid::Uuid,
    pub action: Action,
    pub tag: ProtoTag,
    pub wg: Option<WgParam>,
    pub password: Option<String>,
    pub token: Option<uuid::Uuid>,
    pub expires_at: Option<RkyvDateTime>,
    pub subscription_id: Option<uuid::Uuid>,
}
```

- `conn_id` — connection UUID, primary key.
- `action` — `Create`, `Update`, `Delete`, `ResetStat`.
- `tag` — protocol type (`ProtoTag`).
- `wg` — WireGuard/AmneziaWG parameters (`keys` + `address`). Populated only for `Wireguard`/`AmneziaWg`.
- `password` — Shadowsocks password.
- `token` — Hysteria2 token (UUID).
- `expires_at` — wrapped as `RkyvDateTime` for rkyv compatibility.
- `subscription_id` — optional owning subscription UUID.

## Action enum

```rust
pub enum Action {
    Create,
    Update,
    Delete,
    ResetStat,
}
```

## ProtoTag / Tag enum

`src/memory/tag.rs`

```rust
pub enum ProtoTag {
    VlessTcpReality,
    VlessGrpcReality,
    VlessXhttpReality,
    VlessXhttpCdn,
    Vmess,
    Shadowsocks,
    Wireguard,
    AmneziaWg,
    Hysteria2,
    Mtproto,
}
```

String form on the wire matches the variant name exactly (`"Wireguard"`, `"VlessTcpReality"`, etc.).

## Payload mapping

| Protocol | `tag` | Payload field | Notes |
|---|---|---|---|
| WireGuard | `Wireguard` | `wg` | `keys.privkey` + `address`. Pubkey is derived from the private key. |
| AmneziaWG | `AmneziaWg` | `wg` | Same shape as WireGuard; applied via netlink. |
| Shadowsocks | `Shadowsocks` | `password` | Handled by the Xray handler. |
| Hysteria2 | `Hysteria2` | `token` | External auth provider validates this token. Messages use the `auth` topic. |
| MTProto | `Mtproto` | `secret` | Stored in the connection; no per-message ZMQ payload is currently sent. |
| VLESS/Vmess (Xray) | `VlessTcpReality`, `VlessGrpcReality`, `VlessXhttpReality`, `VlessXhttpCdn`, `Vmess` | `tag` only | The Xray inbound tag selects the handler. No per-connection keys. |

## JSON examples (illustrative)

These are the logical contents of the rkyv structs.

### WireGuard

```json
// Create
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Create",
  "tag": "Wireguard",
  "wg": {
    "keys": { "privkey": "LY8D/CyB/JT1uiFhK1yVKxBB3VMZeA0DzOAJEvgQw50=" },
    "address": { "address": "10.10.0.24", "cidr": 32 }
  },
  "password": null,
  "token": null,
  "expires_at": "2026-07-29T18:21:42Z",
  "subscription_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
}

// Update
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Update",
  "tag": "Wireguard",
  "wg": {
    "keys": { "privkey": "NEW_PRIVATE_KEY_BASE64" },
    "address": { "address": "10.10.0.25", "cidr": 32 }
  }
}

// Delete
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Delete",
  "tag": "Wireguard"
}

// ResetStat
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "ResetStat",
  "tag": "Wireguard"
}
```

Expected display output for a Create:

```text
17865be5-e18b-40d6-b5af-e1c4d51ff50a | Create | Wireguard | 10.10.0.24/32 | LY8D/CyB/JT1uiFhK1yVKxBB3VMZeA0DzOAJEvgQw50= | - | - | -
```

### AmneziaWG

```json
// Create
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Create",
  "tag": "AmneziaWg",
  "wg": {
    "keys": { "privkey": "LY8D/CyB/JT1uiFhK1yVKxBB3VMZeA0DzOAJEvgQw50=" },
    "address": { "address": "10.20.0.24", "cidr": 32 }
  }
}

// Update
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Update",
  "tag": "AmneziaWg",
  "wg": {
    "keys": { "privkey": "NEW_PRIVATE_KEY_BASE64" },
    "address": { "address": "10.20.0.25", "cidr": 32 }
  }
}

// Delete
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Delete",
  "tag": "AmneziaWg"
}

// ResetStat
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "ResetStat",
  "tag": "AmneziaWg"
}
```

### Shadowsocks

```json
// Create
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Create",
  "tag": "Shadowsocks",
  "password": "random-password-15"
}

// Update
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Update",
  "tag": "Shadowsocks",
  "password": "new-password"
}

// Delete
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Delete",
  "tag": "Shadowsocks"
}

// ResetStat
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "ResetStat",
  "tag": "Shadowsocks"
}
```

### Hysteria2

```json
// Create
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Create",
  "tag": "Hysteria2",
  "token": "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
}

// Update
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Update",
  "tag": "Hysteria2",
  "token": "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
}

// Delete
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Delete",
  "tag": "Hysteria2"
}

// ResetStat
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "ResetStat",
  "tag": "Hysteria2"
}
```

### MTProto

```json
// Create
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Create",
  "tag": "Mtproto",
  "secret": "random-secret-15"
}

// Update
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Update",
  "tag": "Mtproto",
  "secret": "new-secret"
}

// Delete
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Delete",
  "tag": "Mtproto"
}

// ResetStat
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "ResetStat",
  "tag": "Mtproto"
}
```

### Xray protocols (VLESS / Vmess)

```json
// Create
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Create",
  "tag": "VlessTcpReality"
}

// Update
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Update",
  "tag": "VlessTcpReality"
}

// Delete
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "Delete",
  "tag": "VlessTcpReality"
}

// ResetStat
{
  "conn_id": "17865be5-e18b-40d6-b5af-e1c4d51ff50a",
  "action": "ResetStat",
  "tag": "VlessTcpReality"
}
```

## Wire serialization

Do **not** send plain JSON to the ZeroMQ socket. Use rkyv to serialize `Vec<Message>`:

```rust
use rkyv::to_bytes;

let messages: Vec<Message> = vec![msg];
let bytes = to_bytes::<_, 1024>(&messages)?;
publisher.send_binary(&topic, bytes.as_ref()).await?;
```

Subscribers deserialize with:

```rust
use rkyv::{check_archived_root, Archive, Deserialize, Infallible};

let aligned = rkyv::AlignedVec::from(&bytes);
let archived = check_archived_root::<Vec<Message>>(&aligned)?;
let messages: Vec<Message> = archived.deserialize(&mut Infallible)?;
```

## Debugging binary messages

For debugging, the `Message` type implements `Display`. Example output:

```text
17865be5-e18b-40d6-b5af-e1c4d51ff50a | Create | Wireguard | 10.10.0.24/32 | LY8D... | - | - | -
```

Fields in display order: `conn_id | action | tag | address | privkey | password | token | expires_at`.
