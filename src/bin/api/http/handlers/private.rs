use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warp::http::StatusCode;
use warp::reply::Reply;

use fcore::{
    http::{helpers as http, IdResponse, ResponseMessage},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, ConnectionStorageApiOperations,
    Env, IpAddrMask, MetricEnvelope, MetricStorage, Node, NodeStatus, NodeStorageOperations,
    NodeType, PlanKind, Status, Subscription, SubscriptionOperations,
    SubscriptionStorageOperations, Tag,
};

use super::super::{
    super::{
        postgres::install_token::MAX_ACTIVE_INSTALL_TOKENS,
        sync::{
            tasks::{SyncOp, DELETE_REASON_MANUAL},
            MemSync,
        },
    },
    request::NodeRequest,
};

#[derive(Debug, Deserialize)]
pub struct InstallTokenRequest {
    pub subscription_id: Uuid,
}

/// Pool of a private node — the pool the account itself declared
/// (`[node] pool` on the machine).
///
/// A shard holds **several** pools: the pool is the account's own grouping (a
/// country, a pair of machines) and every pool is one card in the app, while a
/// node without a pool keeps a card of its own. It is not an api-chosen
/// singleton — stamping one name on every node of the shard collapsed the whole
/// account into a single card.
///
/// `stored` is what the shard already keeps for this node: a refresh that does
/// not declare a pool must not wipe it (the DB upsert is `COALESCE`, the
/// in-memory row is not).
fn private_pool(raw: Option<String>, stored: Option<String>) -> Result<Option<String>, String> {
    match crate::http::request::normalized_pool(raw) {
        None => Ok(stored),
        Some(pool) => {
            if pool.chars().count() > 64 {
                return Err("pool name is too long (max 64)".to_string());
            }
            if pool.chars().any(char::is_control) {
                return Err("pool name contains control characters".to_string());
            }
            Ok(Some(pool))
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InstallTokenResponse {
    pub token: String,
    pub scope_env: String,
    /// Address block of the account's shard (`10.89.0.0/16`). The installer
    /// brings the peer interface up inside it (`<block>.1/16`) and NATs exactly
    /// that range: without it a node answers on a subnet the api never
    /// allocates from, and every handshake dies silently.
    pub env_block: String,
    /// Node build the api is running with (`v0.7.47`): the installer downloads
    /// `node-<arch>` of this release instead of a pinned old tag.
    pub node_version: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub install_hint: String,
}

/// What an install token means for its node, resolved before anything is
/// installed: the shard env the node must declare (the bus topic is derived
/// from it) and the address block its peer interface must live in.
#[derive(Debug, Serialize)]
pub struct InstallTokenInfoResponse {
    pub scope_env: String,
    pub env_block: String,
    pub node_version: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct PrivateRegisterResponse {
    pub id: Uuid,
    pub node_token: String,
    pub scope_env: String,
}

/// Response of the public account-creation endpoint: everything the client
/// needs to open its (empty) selfhosted account.
#[derive(Debug, Serialize)]
pub struct PrivateAccountResponse {
    pub subscription_id: Uuid,
    pub plan_kind: String,
    pub scope_env: String,
}

#[derive(Debug, Serialize)]
pub struct PrivateNodeResponse {
    pub uuid: Uuid,
    pub hostname: String,
    pub address: String,
    pub label: String,
    pub country: String,
    pub status: NodeStatus,
    pub env: String,
    pub inbounds: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PrivateNodesQuery {
    pub subscription_id: Uuid,
}

/// Used / cap / free slots of one selfhosted resource. `used` counts what the
/// account holds right now (nodes offline included; child subscriptions that
/// are not deleted).
#[derive(Debug, Serialize)]
pub struct LimitUsage {
    pub used: i64,
    pub limit: i64,
    pub remaining: i64,
}

impl LimitUsage {
    pub fn new(used: i64, limit: i64) -> Self {
        Self {
            used,
            limit,
            remaining: (limit - used).max(0),
        }
    }
}

/// `GET /private/state?subscription_id=` — the cabinet reads it to render
/// "N of M" for servers (own nodes) and users (child subscriptions).
#[derive(Debug, Serialize)]
pub struct PrivateStateResponse {
    pub subscription_id: Uuid,
    pub plan_kind: String,
    /// Own-node quota of the account: the only gate of the install-token
    /// endpoint and of the cabinet's own-node block (`> 0` shows it).
    pub own_nodes_limit: i64,
    pub servers: LimitUsage,
    pub users: LimitUsage,
}

/// Payload of a 409 rejection: a machine-readable `code`
/// (`server_limit_reached` / `user_limit_reached`) plus the same counters as
/// `/private/state`, so a client can render the full "3 of 3" message without
/// a second request.
#[derive(Debug, Serialize)]
pub struct LimitErrorResponse {
    pub code: String,
    pub used: i64,
    pub limit: i64,
    pub remaining: i64,
}

fn limit_reached(code: &str, used: i64, limit: i64) -> warp::reply::WithStatus<warp::reply::Json> {
    let body = ResponseMessage {
        status: StatusCode::CONFLICT.as_u16(),
        message: format!(
            "{} reached ({}/{})",
            match code {
                "user_limit_reached" => "user limit",
                _ => "server limit",
            },
            used,
            limit
        ),
        response: Some(LimitErrorResponse {
            code: code.to_string(),
            used,
            limit,
            remaining: (limit - used).max(0),
        }),
    };
    warp::reply::with_status(warp::reply::json(&body), StatusCode::CONFLICT)
}

#[derive(Debug, Deserialize)]
pub struct PrivateFeedRequest {
    pub subscription_id: Uuid,
    pub include: bool,
}

/// Body of `POST /private/users` — a selfhosted account invites a user, who
/// gets its own child subscription on the account's nodes. Either every
/// current *and future* node of the account (`all_servers: true`) or an
/// explicit non-empty subset (`node_ids`), never both.
#[derive(Debug, Deserialize)]
pub struct PrivateUserCreateRequest {
    pub subscription_id: Uuid,
    #[serde(default)]
    pub all_servers: bool,
    #[serde(default)]
    pub node_ids: Vec<Uuid>,
}

/// One config handed to a user: a connection of the child subscription,
/// pinned to a single node (subset mode) or unpinned (all-servers mode, any
/// node of the account's env serves it).
#[derive(Debug, Serialize)]
pub struct PrivateUserConnection {
    pub id: Uuid,
    pub proto: String,
    pub node_id: Option<Uuid>,
}

/// `POST /private/users` response: the child subscription and the link the
/// owner shares with the user (`frkn://sub/<child-uuid>`).
#[derive(Debug, Serialize)]
pub struct PrivateUserResponse {
    pub subscription_id: Uuid,
    pub subscription_url: String,
    pub all_servers: bool,
    pub node_ids: Vec<Uuid>,
    pub connections: Vec<PrivateUserConnection>,
    pub users: LimitUsage,
}

/// One row of `GET /private/users`.
#[derive(Debug, Serialize)]
pub struct PrivateUserListItem {
    pub subscription_id: Uuid,
    pub subscription_url: String,
    pub all_servers: bool,
    pub node_ids: Vec<Uuid>,
    pub connections: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// `DELETE /private/users/{child}` response: the revoked child and how many
/// of its connections were cut (each delete is published to the nodes, so no
/// orphan peers stay behind).
#[derive(Debug, Serialize)]
pub struct PrivateUserRevokeResponse {
    pub subscription_id: Uuid,
    pub deleted: bool,
    pub connections_deleted: usize,
}

/// One connection to mint for a user: the proto plus the node it is pinned
/// to (`None` = served by every node of the account's personal env).
struct UserConnPlan {
    proto: Tag,
    node_id: Option<Uuid>,
}

/// The link shape the owner forwards to the user.
fn user_subscription_url(child_id: Uuid) -> String {
    format!("frkn://sub/{}", child_id)
}

/// Node-selection rules of `POST /private/users`: all servers XOR a non-empty
/// subset without duplicates. Rejects both at once (ambiguous), an empty
/// subset (a user with no servers) and duplicates (they would mint two
/// identical configs).
fn validate_user_selection(all_servers: bool, node_ids: &[Uuid]) -> Result<(), &'static str> {
    if all_servers && !node_ids.is_empty() {
        return Err("node_ids must not be sent together with all_servers");
    }
    if !all_servers && node_ids.is_empty() {
        return Err("node_ids must not be empty when all_servers is false");
    }
    let mut seen = std::collections::HashSet::new();
    for id in node_ids {
        if !seen.insert(*id) {
            return Err("node_ids contains duplicates");
        }
    }
    Ok(())
}

/// AWG-family inbounds of a node, in a stable order.
///
/// Hysteria2 is deliberately out of scope for users: per-node isolation on
/// the node side is not reliable there yet, so a user only ever gets AWG
/// (including AWG mobile) configs.
fn awg_protos(node: &fcore::Node) -> Vec<Tag> {
    [Tag::AmneziaWg, Tag::AmneziaWgMobile]
        .into_iter()
        .filter(|tag| node.inbounds.values().any(|i| i.tag == *tag))
        .collect()
}

/// Reverse of the creation plan: reads a user's connections and tells whether
/// it is in all-servers mode (at least one unpinned connection) or which
/// nodes it is pinned to.
fn user_node_selection(conns: &[(Option<Uuid>, bool)]) -> (bool, Vec<Uuid>) {
    let mut all_servers = false;
    let mut nodes: Vec<Uuid> = Vec::new();
    for (pin, deleted) in conns {
        if *deleted {
            continue;
        }
        match pin {
            None => all_servers = true,
            Some(node) => {
                if !nodes.contains(node) {
                    nodes.push(*node);
                }
            }
        }
    }
    if all_servers {
        nodes.clear();
    }
    (all_servers, nodes)
}

fn unauthorized(msg: &str) -> warp::reply::WithStatus<warp::reply::Json> {
    let resp = ResponseMessage::<Option<Uuid>> {
        status: StatusCode::UNAUTHORIZED.as_u16(),
        message: msg.to_string(),
        response: None,
    };
    warp::reply::with_status(warp::reply::json(&resp), StatusCode::UNAUTHORIZED)
}

fn forbidden(msg: &str) -> warp::reply::WithStatus<warp::reply::Json> {
    let resp = ResponseMessage::<Option<Uuid>> {
        status: StatusCode::FORBIDDEN.as_u16(),
        message: msg.to_string(),
        response: None,
    };
    warp::reply::with_status(warp::reply::json(&resp), StatusCode::FORBIDDEN)
}

fn too_many_requests(msg: &str) -> warp::reply::WithStatus<warp::reply::Json> {
    let resp = ResponseMessage::<Option<Uuid>> {
        status: StatusCode::TOO_MANY_REQUESTS.as_u16(),
        message: msg.to_string(),
        response: None,
    };
    warp::reply::with_status(warp::reply::json(&resp), StatusCode::TOO_MANY_REQUESTS)
}

fn personal_env(subscription_id: Uuid) -> Env {
    Env::personal_for(subscription_id)
}

/// The subscription that owns a personal shard (`custompersonal<uuid>`).
///
/// A node re-registering with its node token carries only its env, and the
/// rights of the account must be re-applied on that path too. The env name is
/// derived from the subscription id, so it is the subscription id.
pub(crate) fn subscription_from_personal_env(env: &Env) -> Option<Uuid> {
    let name = env.to_string();
    let suffix = name
        .strip_prefix("custompersonal")
        .or_else(|| name.strip_prefix("personal"))?;
    Uuid::parse_str(suffix).ok()
}

/// Env whose nodes belong to this account: a regular account owns nodes on its
/// personal env, a premium account owns them on its premium env instead. Asking
/// for the personal env of a premium subscription returns nothing, which is why
/// the cabinet servers card used to hide itself for premium accounts.
fn owned_env<S: SubscriptionOperations>(sub: &S, subscription_id: Uuid) -> Env {
    sub.scope_env()
        .filter(|env| env.is_personal() || env.is_premium())
        .cloned()
        .unwrap_or_else(|| personal_env(subscription_id))
}

fn refresh_env(existing: Option<&Env>, requested: &Env) -> Result<Env, &'static str> {
    if let Some(env) = existing {
        if !env.is_personal() {
            return Err("refusing shared env for private node");
        }
        return Ok(env.clone());
    }
    if !requested.is_personal() {
        return Err("personal env required");
    }
    Ok(requested.clone())
}

#[cfg(test)]
mod refresh_env_tests {
    use super::refresh_env;
    use fcore::Env;

    fn personal(tag: &str) -> Env {
        // в БД это `custompersonal…`, Display добавляет "custom" к inner-имени
        Env::Custom(format!("personal{tag}"))
    }

    /// Инцидент: в конфиге self-hosted ноды остался `env = "dev"` от старого
    /// установщика, а в базе она в персональном env. Зарегистрированный env
    /// побеждает, в dev нода не уезжает.
    #[test]
    fn registered_personal_env_wins_over_requested_dev() {
        let registered = personal("a");
        assert_eq!(refresh_env(Some(&registered), &Env::Dev), Ok(registered));
    }

    /// Нода, зарегистрированная в общем env, приватной быть не может.
    #[test]
    fn shared_registered_env_is_refused() {
        assert_eq!(
            refresh_env(Some(&Env::Dev), &Env::Dev),
            Err("refusing shared env for private node")
        );
    }

    /// Новая нода без строки в базе: только персональный env.
    #[test]
    fn unknown_node_must_ask_for_personal_env() {
        assert_eq!(refresh_env(None, &Env::Dev), Err("personal env required"));
        let env = personal("a");
        assert_eq!(refresh_env(None, &env), Ok(env));
    }
}

/// Отметка «нода только что зарегистрировалась».
///
/// Теги обязаны совпадать с тегами настоящих heartbeat от агента: ключ серии
/// в MetricStorage — хеш (имя + теги), и пустой набор тегов создавал вторую,
/// навсегда замороженную серию `sys.heartbeat` рядом с живой.
fn note_heartbeat(metrics: &MetricStorage, node_id: Uuid, tags: BTreeMap<String, String>) {
    metrics.insert_envelope(MetricEnvelope {
        node_id,
        name: "sys.heartbeat".to_string(),
        value: 1.0,
        timestamp: chrono::Utc::now().timestamp_millis(),
        tags,
    });
}

fn private_seed_tags(
    existing_private_nodes: i64,
    env: &Env,
    present: &std::collections::HashSet<(Env, Tag)>,
) -> Vec<Tag> {
    if existing_private_nodes != 0 {
        return Vec::new();
    }
    [Tag::AmneziaWg, Tag::AmneziaWgMobile, Tag::Hysteria2]
        .into_iter()
        .filter(|tag| !present.contains(&(env.clone(), *tag)))
        .collect()
}

async fn seed_private_connections<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: Uuid,
    env: &Env,
    existing_private_nodes: i64,
    wg_network: &IpAddrMask,
    awg_network: &IpAddrMask,
    awg_mobile_network: &Option<IpAddrMask>,
) where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if existing_private_nodes != 0 {
        return;
    }

    let present = {
        let mem = memory.memory.read().await;
        let conns: Vec<(Uuid, Env, Tag)> = mem
            .connections
            .get_by_subscription_id(&subscription_id)
            .unwrap_or_default()
            .iter()
            .map(|(conn_id, conn)| (*conn_id, conn.get_env(), conn.get_proto().proto()))
            .collect();
        super::connection::existing_default_pairs(&conns, &mem.conn_labels, &mem.share_conns)
    };

    for tag in private_seed_tags(existing_private_nodes, env, &present) {
        if let Err(err) = super::connection::create_connection_inner(
            env,
            tag,
            Some(subscription_id),
            None,
            None,
            None,
            None,
            true,
            memory,
            wg_network,
            awg_network,
            awg_mobile_network,
        )
        .await
        {
            tracing::error!(
                "private connection seed failed for {} {:?}: {}",
                subscription_id,
                tag,
                err
            );
        }
    }
}

async fn ensure_personal_scope<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: Uuid,
) -> Result<Env, warp::reply::WithStatus<warp::reply::Json>>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    // An account that already owns a shard keeps it: `sf0001` for the accounts
    // created since the numbered scheme, `custompersonal<uuid>` for older ones.
    // Allocating on every call would hand out a new shard per install token.
    let (existing, existing_token) = {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&subscription_id) else {
            return Err(http::not_found("subscription not found"));
        };
        if sub.is_deleted() {
            return Err(http::not_found("subscription not found"));
        }
        let existing = sub.scope_env().filter(|env| env.is_personal()).cloned();
        (existing, sub.premium_token().map(|t| t.to_string()))
    };

    if let Some(env) = existing {
        return Ok(env);
    }

    // An account created before `scope_env` existed still owns its machines in
    // the legacy `custompersonal<uuid>` env. Adopt that name instead of
    // allocating a fresh `sfNNNN`: a new shard would strand every node the
    // account already runs in a shard the cabinet no longer shows.
    let legacy = personal_env(subscription_id);
    let legacy_owned = {
        let mem = memory.memory.read().await;
        let has_nodes = mem
            .nodes
            .get_by_env(&legacy)
            .is_some_and(|nodes| !nodes.is_empty());
        let has_grant = mem
            .peers_state
            .access
            .get(&subscription_id, &legacy)
            .is_some();
        has_nodes || has_grant
    };
    if legacy_owned {
        if let Err(e) = memory
            .db
            .sub()
            .set_premium_fields(&subscription_id, Some(&legacy), existing_token.as_deref())
            .await
        {
            tracing::error!("private: cannot record legacy scope {}: {}", legacy, e);
        }
        return Ok(legacy);
    }

    // No shard yet: this is where an own-node account is born. `sfNNNN` comes
    // from a sequence, so two accounts registering at the same moment cannot
    // collide, and a number is never reused — its nodes may still be running.
    let personal = memory
        .db
        .peers()
        .next_selfhosted_shard()
        .await
        .map_err(|_| http::internal_error("failed to allocate a shard"))?;

    memory
        .db
        .sub()
        .set_premium_fields(&subscription_id, Some(&personal), existing_token.as_deref())
        .await
        .map_err(|_| http::internal_error("failed to set personal scope"))?;

    // Register the shard itself, so `shard_overview`, `env_capacity.kind` and
    // the address block allocation all know it exists. `kind = selfhosted`
    // follows from the plan, but the row has to be there first.
    if let Err(e) = memory
        .db
        .peers()
        .upsert_shard(&personal, "selfhosted", None, "selfhosted", None, true)
        .await
    {
        tracing::warn!(
            subscription = %subscription_id,
            env = %personal,
            error = %e,
            "private: shard row was not registered"
        );
    }

    {
        let mut mem = memory.memory.write().await;
        if let Some(sub) = mem.subscriptions.find_by_id_mut(&subscription_id) {
            sub.set_scope_env(personal.clone());
        }
    }

    tracing::info!(
        subscription = %subscription_id,
        env = %personal,
        "private: account got its own shard"
    );

    Ok(personal)
}

/// Why `POST /private/install-token` refuses. Kept as data so the handler
/// only maps it to a status and the rules stay unit-testable without a DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallTokenDenial {
    /// A user of an own-node account never hosts nodes of its own: it lives
    /// on the owner's nodes only.
    ChildAccount,
    /// The account holds no own-node quota.
    NotEnabled,
    /// Every own-node slot is taken (offline nodes count too).
    ServerLimit { used: i64, limit: i64 },
    /// Too many install tokens are still usable.
    TooManyTokens,
}

/// The gate of `POST /private/install-token`. Hosting a node is a capability
/// of any non-child account, not a plan: `plan_kind = selfhosted` (the old
/// reseller concept) is deliberately not consulted. Only the parent link, the
/// account's own-node quota and the two caps decide.
pub(crate) fn install_token_denial(
    parent_id: Option<Uuid>,
    own_nodes_limit: i64,
    servers_used: i64,
    active_tokens: i64,
) -> Option<InstallTokenDenial> {
    if parent_id.is_some() {
        return Some(InstallTokenDenial::ChildAccount);
    }
    if own_nodes_limit <= 0 {
        return Some(InstallTokenDenial::NotEnabled);
    }
    if servers_used >= own_nodes_limit {
        return Some(InstallTokenDenial::ServerLimit {
            used: servers_used,
            limit: own_nodes_limit,
        });
    }
    if active_tokens >= MAX_ACTIVE_INSTALL_TOKENS {
        return Some(InstallTokenDenial::TooManyTokens);
    }
    None
}

/// Node build the api ships with (`v0.7.47`): the installer asks for it instead
/// of pinning a tag that drifts away from the api every release.
pub(crate) fn node_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// The address block of one shard, allocated on first use.
///
/// `env_networks` is the api's own statement of which range a shard holds: peers
/// are numbered out of it, and a node whose peer interface sits outside it would
/// answer nobody. Account-owned shards are not listed in the api config (nobody
/// knows their number in advance), so the block is carved out of the auto pool
/// on the first mint and returned to the installer, which writes
/// `<block>.1/16` on the interface. Idempotent: an env that already holds a
/// block keeps it, however many tokens are minted.
async fn ensure_env_block<N, C, S>(
    memory: &MemSync<N, C, S>,
    env: &Env,
    auto_pool: Option<&IpAddrMask>,
    auto_prefix: u8,
) -> Option<String>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    if let Some(block) = {
        let mem = memory.memory.read().await;
        mem.peers_state
            .block_masks(env)
            .first()
            .map(|block| block.to_string())
    } {
        return Some(block);
    }

    let Some(pool) = auto_pool else {
        tracing::error!(
            "install-token: shard {} has no block and [service.peers] auto_subnet_pool is unset",
            env
        );
        return None;
    };

    match memory
        .db
        .peers()
        .allocate_env_block(env, pool, auto_prefix)
        .await
    {
        Ok(Some(block)) => {
            tracing::info!("install-token: shard {} got {} from the auto pool", env, block);
            memory
                .memory
                .write()
                .await
                .peers_state
                .env_networks
                .entry(env.clone())
                .or_default()
                .push(block.to_string());
            Some(block.to_string())
        }
        Ok(None) => {
            tracing::error!("install-token: the auto pool is exhausted, {} got no block", env);
            None
        }
        Err(e) => {
            tracing::error!("install-token: cannot allocate a block for {}: {}", env, e);
            None
        }
    }
}

/// Tags of a private node that turn into cards of its own shard: the peer
/// plane plus hysteria2. The node announces them; the api derives the grant
/// from the announcement, never from anything the installer asked for.
fn private_card_tags(node: &fcore::Node) -> Vec<Tag> {
    let mut tags: Vec<Tag> = node
        .inbounds
        .keys()
        .copied()
        .filter(|tag| tag.is_peer() || *tag == Tag::Hysteria2)
        .collect();
    tags.sort_by_key(|tag| tag.to_string());
    tags.dedup();
    tags
}

/// Make the node's peer interface and the shard's block agree.
///
/// Two jobs, both fail-closed: a shard nobody gave a block takes the one the
/// node brought (`10.89.0.1/16` -> `10.89.0.0/16`, the older shards' habit), and
/// a peer interface outside the block is refused — the api would number peers
/// from its block while the node answers on another subnet, and every handshake
/// dies without a single log line.
async fn settle_private_block<N, C, S>(
    memory: &MemSync<N, C, S>,
    node: &fcore::Node,
) -> Result<(), String>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let declared = {
        let mem = memory.memory.read().await;
        mem.peers_state.block_masks(&node.env)
    };

    let mut blocks = declared;
    if blocks.is_empty() {
        if let Some(block) = super::node::adopted_peer_block(node) {
            match memory.db.peers().insert_env_block(&node.env, &block).await {
                Ok(crate::postgres::peers::Inserted::Done) => {
                    tracing::info!(
                        "peers: own shard {} adopted {} from node {} ({})",
                        node.env,
                        block,
                        node.uuid,
                        node.hostname
                    );
                    memory
                        .memory
                        .write()
                        .await
                        .peers_state
                        .env_networks
                        .entry(node.env.clone())
                        .or_default()
                        .push(block.to_string());
                    blocks = vec![block];
                }
                Ok(crate::postgres::peers::Inserted::Conflict) => {
                    return Err(format!(
                        "peer subnet {} is already held by another shard — pick another range",
                        block
                    ));
                }
                Err(e) => tracing::error!("peers: cannot store the subnet of {}: {}", node.env, e),
            }
        }
    }

    if blocks.is_empty() {
        // No peer interface at all (v1-only install) or an api without an auto
        // pool: nothing to check. `/v1/peers` answers `env_not_served` later.
        return Ok(());
    }

    for (tag, address) in super::node::peer_interface_addresses(node) {
        if !super::node::address_in_blocks(&address, &blocks) {
            return Err(format!(
                "peer interface {} ({:?}) sits outside the shard blocks of {} ({})",
                address,
                tag,
                node.env,
                blocks
                    .iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    Ok(())
}

/// Rights and peer set of an own node, applied on every registration.
///
/// Without the grant `/v1/locations` has no card to render, so the node the user
/// just installed simply never appears. Without the `peers-init` push the node's
/// interface stays empty until somebody creates a tunnel — and a node that lost
/// its snapshot keeps an empty interface forever, because a 200 on re-register
/// used to send nothing. Both are idempotent, so the refresh path runs them too.
async fn seed_private_shard<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: Uuid,
    node: &fcore::Node,
) where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    let tags = private_card_tags(node);
    if tags.is_empty() {
        return;
    }

    // One grant per (subscription, env): `own` shard, no term, no quota — the
    // account runs the machine.
    match memory
        .db
        .peers()
        .upsert_grant(subscription_id, &node.env, &tags, "install", None, None)
        .await
    {
        Ok(row) => {
            memory
                .memory
                .write()
                .await
                .peers_state
                .access
                .insert(subscription_id, row.clone());
            tracing::info!(
                "private: grant for {} on {} ({:?})",
                subscription_id,
                row.env,
                tags
            );
        }
        Err(e) => tracing::error!(
            "private: cannot grant {} its own shard {}: {}",
            subscription_id,
            node.env,
            e
        ),
    }

    let peer_tags: Vec<Tag> = node
        .inbounds
        .keys()
        .copied()
        .filter(|tag| tag.is_peer())
        .collect();
    if peer_tags.is_empty() {
        return;
    }
    super::peers::publish_peers_init(
        memory,
        node.uuid,
        &node.env,
        node.pool.as_deref(),
        &peer_tags,
    )
    .await;
}

pub async fn mint_install_token_handler<N, C, S>(
    req: InstallTokenRequest,
    memory: MemSync<N, C, S>,
    auto_pool: Option<IpAddrMask>,
    auto_prefix: u8,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    // A user of an own-node account must not get an install token (and no
    // personal env of its own either — checked before any scope side effect).
    let parent_id = {
        let mem = memory.memory.read().await;
        match mem.subscriptions.find_by_id(&req.subscription_id) {
            Some(sub) => {
                if sub.parent_id().is_some() {
                    return Ok(forbidden(
                        "a user of a selfhosted account cannot host nodes",
                    )
                    .into_response());
                }
                sub.parent_id()
            }
            None => None,
        }
    };

    let scope_env = match ensure_personal_scope(&memory, req.subscription_id).await {
        Ok(env) => env,
        Err(reply) => return Ok(reply.into_response()),
    };

    // Own-node quota of the account. Refuse to mint an install token once
    // every slot is taken: the registration itself would be rejected anyway
    // (409 `server_limit_reached`), and this keeps the cabinet honest.
    let (limit, used) = {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&req.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        let used = mem
            .nodes
            .get_by_env(&scope_env)
            .map(|nodes| nodes.len() as i64)
            .unwrap_or(0);
        (sub.own_nodes_limit(), used)
    };

    let active = match memory
        .db
        .install_token()
        .count_active(&req.subscription_id)
        .await
    {
        Ok(n) => n,
        Err(_) => return Ok(http::internal_error("db error").into_response()),
    };

    match install_token_denial(parent_id, limit, used, active) {
        None => {}
        Some(InstallTokenDenial::ChildAccount) => {
            return Ok(forbidden("a user of a selfhosted account cannot host nodes").into_response())
        }
        Some(InstallTokenDenial::NotEnabled) => {
            return Ok(super::share::forbidden(
                "own nodes are not enabled for this account",
            ))
        }
        Some(InstallTokenDenial::ServerLimit { used, limit }) => {
            return Ok(limit_reached("server_limit_reached", used, limit).into_response())
        }
        Some(InstallTokenDenial::TooManyTokens) => {
            return Ok(too_many_requests(&format!(
                "too many active install tokens (max {})",
                MAX_ACTIVE_INSTALL_TOKENS
            ))
            .into_response())
        }
    }

    let token = format!("inst_{}", Uuid::new_v4().simple());

    // The block is part of the token, not a script constant: the installer
    // writes `<block>.1/16` on the peer interface and NATs exactly that range.
    let Some(env_block) = ensure_env_block(&memory, &scope_env, auto_pool.as_ref(), auto_prefix).await
    else {
        return Ok(http::internal_error("install token: no address block for the shard").into_response());
    };

    let row = match memory
        .db
        .install_token()
        .create(&token, req.subscription_id, &scope_env)
        .await
    {
        Ok(row) => row,
        Err(_) => return Ok(http::internal_error("db error").into_response()),
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(InstallTokenResponse {
                token: row.token.clone(),
                scope_env: scope_env.to_string(),
                env_block,
                node_version: node_version(),
                expires_at: row.expires_at,
                install_hint: format!(
                    "curl -fsSL https://frkn.org/install | bash -s -- --token {}",
                    row.token
                ),
            }),
        }),
        StatusCode::OK,
    )
    .into_response())
}

/// `GET /private/install-token/<token>`: what the installer has to know before
/// it touches the machine — the shard env (the bus topic is derived from it) and
/// the address block. Read-only: the token stays usable for the registration
/// that follows, so the installer can be re-run safely.
pub async fn inspect_install_token_handler<N, C, S>(
    token: String,
    memory: MemSync<N, C, S>,
    auto_pool: Option<IpAddrMask>,
    auto_prefix: u8,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + PartialEq + 'static,
{
    if !token.starts_with("inst_") {
        return Ok(http::bad_request("install token required").into_response());
    }

    let row = match memory.db.install_token().find_usable(&token).await {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(unauthorized("invalid or expired install token").into_response()),
        Err(_) => return Ok(http::internal_error("db error").into_response()),
    };

    let Some(env_block) = ensure_env_block(&memory, &row.scope_env, auto_pool.as_ref(), auto_prefix).await
    else {
        return Ok(http::internal_error("install token: no address block for the shard").into_response());
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(InstallTokenInfoResponse {
                scope_env: row.scope_env.to_string(),
                env_block,
                node_version: node_version(),
                expires_at: row.expires_at,
            }),
        }),
        StatusCode::OK,
    )
    .into_response())
}

/// Creates a brand-new empty selfhosted account: a subscription with
/// `plan_kind = selfhosted`, `expires_at = NULL`, a personal `scope_env`
/// derived from its own id and zero connections. Public and per-IP
/// rate-limited — anyone may open an account, but not at will.
pub async fn create_private_account_handler<N, C, S>(
    memory: MemSync<N, C, S>,
    remote: Option<std::net::SocketAddr>,
    x_forwarded_for: Option<String>,
    rate_limiter: Arc<super::share::RateLimiter>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let ip = super::share::client_ip(remote, x_forwarded_for.as_deref());
    if !rate_limiter.check(ip) {
        return Ok(
            too_many_requests("too many account creations, try again later").into_response(),
        );
    }

    let sub_id = Uuid::new_v4();
    let ref_code = fcore::utils::get_uuid_last_octet_simple(&sub_id);
    // The account is born with its own numbered shard (`sf0001`). Allocating
    // here rather than lazily on the first install token means the cabinet can
    // show the shard before a node exists, and `scope_env` is set once.
    let scope_env = match memory.db.peers().next_selfhosted_shard().await {
        Ok(env) => env,
        Err(e) => {
            tracing::error!("private: cannot allocate a shard: {}", e);
            return Ok(http::internal_error("cannot allocate a shard").into_response());
        }
    };
    if let Err(e) = memory
        .db
        .peers()
        .upsert_shard(&scope_env, "selfhosted", None, "selfhosted", None, true)
        .await
    {
        tracing::warn!(env = %scope_env, error = %e, "private: shard row was not registered");
    }
    let sub = Subscription::new_selfhosted(sub_id, ref_code, scope_env.clone());

    match SyncOp::add_sub(&memory, sub).await {
        Ok(Status::Ok(_)) | Ok(Status::Updated(_)) | Ok(Status::AlreadyExist(_)) => {
            // The account is born on v2: its shard serves peers only, there is
            // no v1 side to migrate. Best-effort — a failed marker read back as
            // `v1` is a display issue, not a reason to fail the creation.
            if let Err(e) = memory.db.sub().mark_v2(&sub_id).await {
                tracing::warn!(subscription = %sub_id, error = %e, "private: v2 marker not set");
            }
            Ok(warp::reply::with_status(
                warp::reply::json(&ResponseMessage {
                    status: StatusCode::OK.as_u16(),
                    message: "Ok".to_string(),
                    response: Some(PrivateAccountResponse {
                        subscription_id: sub_id,
                        plan_kind: PlanKind::Selfhosted.to_string(),
                        scope_env: scope_env.to_string(),
                    }),
                }),
                StatusCode::OK,
            )
            .into_response())
        }
        Ok(Status::BadRequest(_, msg)) => {
            Ok(http::bad_request(&format!("failed to create account: {}", msg)).into_response())
        }
        Ok(_) => Ok(http::bad_request("operation not supported").into_response()),
        Err(e) => {
            tracing::error!("private account create failed: {}", e);
            Ok(http::internal_error("create failed").into_response())
        }
    }
}

pub async fn register_private_node_handler<N, C, S>(
    auth_header: Option<String>,
    mut node_req: NodeRequest,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
    wg_network: IpAddrMask,
    awg_network: IpAddrMask,
    awg_mobile_network: Option<IpAddrMask>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let token = auth_header
        .as_deref()
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");

    if token.starts_with("node_") {
        return refresh_known_private_node(token, node_req, memory, metrics).await;
    }

    if token.is_empty() || !token.starts_with("inst_") {
        return Ok(unauthorized("install token required").into_response());
    }

    let row = match memory.db.install_token().find_usable(token).await {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(unauthorized("invalid or expired install token").into_response()),
        Err(_) => return Ok(http::internal_error("db error").into_response()),
    };

    // A private node may only live in an own-node shard. The declared kind
    // wins; legacy `custompersonal…` envs have no `env_capacity` row and fall
    // back to the name.
    let scope_is_personal = {
        let mem = memory.memory.read().await;
        match mem.peers_state.shard_kind(&row.scope_env) {
            Some(kind) => kind.is_private(),
            None => row.scope_env.is_personal(),
        }
    };
    if !scope_is_personal {
        return Ok(http::bad_request("invalid personal scope").into_response());
    }

    // The shard the account owns: `sf0001` for new accounts, the legacy
    // `custompersonal<uuid>` for older ones. The token's scope must match it —
    // comparing against a re-derived name would reject every `sf` account.
    let expected = row.scope_env.clone();

    if let Err(e) = node_req.validate() {
        return Ok(http::bad_request(&e.to_string()).into_response());
    }

    node_req.env = expected.clone();
    node_req.r#type = Some(NodeType::SelfHosted);
    node_req.cluster = None;
    // A shard may hold several pools: the pool is the account's own grouping
    // (a country, a pair of machines) and each pool is one card. A node that
    // declares none keeps a card of its own, and nothing here invents a name —
    // stamping one pool on every node of the shard collapsed the account into a
    // single card.
    node_req.pool = match private_pool(node_req.pool.clone(), None) {
        Ok(pool) => pool,
        Err(msg) => return Ok(http::bad_request(&msg).into_response()),
    };
    // A numbered shard is one node per pool: the hostname is the pool name when
    // the machine declared none. Every node then gets only its own peers, and
    // the card id stays stable across re-registrations.
    if node_req.pool.is_none() && node_req.env.selfhosted_number().is_some() {
        node_req.pool = Some(node_req.hostname.clone());
    }

    let node = node_req.clone().as_node();
    let node_is_personal = {
        let mem = memory.memory.read().await;
        match mem.peers_state.shard_kind(&node.env) {
            Some(kind) => kind.is_private(),
            None => node.env.is_personal(),
        }
    };
    if !node_is_personal {
        return Ok(http::bad_request("refusing shared env for private node").into_response());
    }

    let node_id = node_req.uuid;

    // The peer interface must live in the shard's block: a node answering
    // outside it would serve nobody. Adopts the block when the shard has none.
    if let Err(msg) = settle_private_block(&memory, &node).await {
        return Ok(http::bad_request(&msg).into_response());
    }

    // Server cap of the owning account, applied transactionally (lock the
    // account row -> count the nodes already in its env -> insert), so two
    // concurrent registrations can't both take the last free slot. `used`
    // counts offline nodes too, and re-registering a known node is free.
    let slot = match memory
        .db
        .node()
        .insert_private_node_with_limit(&expected, node_id, node.clone())
        .await
    {
        Ok(Some(slot)) => slot,
        Ok(None) => {
            tracing::error!("no subscription owns personal env {}", expected);
            return Ok(http::internal_error("personal account not found").into_response());
        }
        Err(e) => {
            tracing::error!("private node reserve failed: {}", e);
            return Ok(http::internal_error("register failed").into_response());
        }
    };
    if !slot.inserted {
        return Ok(limit_reached("server_limit_reached", slot.used, slot.limit).into_response());
    }
    let existing_private_nodes = (slot.used - 1).max(0);

    let status = SyncOp::add_node(&memory, &node_id, node.clone()).await;

    match status {
        Ok(Status::Ok(id)) | Ok(Status::AlreadyExist(id)) | Ok(Status::NotModified(id)) => {
            if let Ok(false) = memory.db.install_token().mark_used(token).await {
                tracing::warn!("install token already burned after node {}", id);
            }

            let node_token = format!("node_{}_{}", id.as_simple(), Uuid::new_v4().simple());
            if let Err(e) = memory
                .db
                .node_access_token()
                .upsert(&node_token, node_id)
                .await
            {
                tracing::error!("failed to store node_token: {}", e);
                return Ok(http::internal_error("failed to issue node token").into_response());
            }

            let _ =
                SyncOp::update_node_status(&memory, &node_id, &node.env, NodeStatus::Online).await;
            note_heartbeat(&metrics, node_id, node.get_base_tags());
            seed_private_connections(
                &memory,
                row.subscription_id,
                &expected,
                existing_private_nodes,
                &wg_network,
                &awg_network,
                &awg_mobile_network,
            )
            .await;
            // Card rights + the authoritative peer set. Runs on every
            // registration, including a re-register answered 200: a node whose
            // snapshot was lost used to stay with an empty interface forever.
            seed_private_shard(&memory, row.subscription_id, &node).await;

            Ok(warp::reply::with_status(
                warp::reply::json(&ResponseMessage {
                    status: StatusCode::OK.as_u16(),
                    message: "Ok".to_string(),
                    response: Some(PrivateRegisterResponse {
                        id,
                        node_token,
                        scope_env: expected.to_string(),
                    }),
                }),
                StatusCode::OK,
            )
            .into_response())
        }
        Ok(_) => Ok(http::bad_request("operation not supported").into_response()),
        Err(e) => {
            tracing::error!("private node register failed: {}", e);
            Ok(http::internal_error("register failed").into_response())
        }
    }
}

pub async fn list_private_nodes_handler<N, C, S>(
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let env = {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&query.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        if sub.is_deleted() {
            return Ok(http::not_found("subscription not found").into_response());
        }
        owned_env(sub, query.subscription_id)
    };
    let mem = memory.memory.read().await;
    let nodes: Vec<PrivateNodeResponse> = mem
        .nodes
        .get_by_env(&env)
        .unwrap_or_default()
        .into_iter()
        .map(|n| {
            let res = n.as_node_response();
            PrivateNodeResponse {
                uuid: res.uuid,
                hostname: res.hostname,
                address: res.address.to_string(),
                label: res.label,
                country: res.country,
                status: res.status,
                env: res.env,
                inbounds: res.inbounds.iter().map(|i| i.tag.to_string()).collect(),
            }
        })
        .collect();

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(nodes),
        }),
        StatusCode::OK,
    )
    .into_response())
}

/// Live gauges of one own node plus its inbound totals. The cabinet shows them
/// as "my server" statistics — the same numbers the admin dashboard renders,
/// scoped to nodes the subscription actually owns.
#[derive(Debug, Serialize)]
pub struct PrivateNodeTraffic {
    /// Cumulative bytes passed through the node's inbounds.
    pub uplink: u64,
    pub downlink: u64,
    /// Connections reported by the inbounds right now (AWG reports peers).
    pub connections: u64,
}

#[derive(Debug, Serialize)]
pub struct PrivateNodeStats {
    #[serde(flatten)]
    pub node: PrivateNodeResponse,
    pub metrics: super::admin::NodeMetricsSnapshot,
    pub traffic: PrivateNodeTraffic,
}

#[derive(Debug, Deserialize)]
pub struct PrivateNodeSeriesQuery {
    pub subscription_id: Uuid,
    pub from: Option<i64>,
    pub to: Option<i64>,
}

/// Downsampled history for one node: averages per time bucket, so a 24h chart
/// is a few hundred points instead of every raw sample.
#[derive(Debug, Default, Serialize)]
pub struct PrivateNodeSeries {
    pub cpu_percent: Vec<(i64, f64)>,
    pub memory_percent: Vec<(i64, f64)>,
    pub network_rx_bps: Vec<(i64, f64)>,
    pub network_tx_bps: Vec<(i64, f64)>,
}

#[derive(Default)]
struct BucketAcc {
    sum: f64,
    count: usize,
}

impl BucketAcc {
    fn add(&mut self, value: f64) {
        self.sum += value;
        self.count += 1;
    }

    fn avg(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            (self.sum / self.count as f64 * 100.0).round() / 100.0
        }
    }
}

/// Node of this subscription, or `None` when the uuid is not one of its own.
/// Ownership is env-based: a personal env holds exactly the account's nodes.
async fn owned_node<N, C, S>(
    memory: &MemSync<N, C, S>,
    subscription_id: Uuid,
    node_id: Uuid,
) -> Option<Node>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let env = {
        let mem = memory.memory.read().await;
        let sub = mem.subscriptions.find_by_id(&subscription_id)?;
        if sub.is_deleted() {
            return None;
        }
        owned_env(sub, subscription_id)
    };
    let mem = memory.memory.read().await;
    mem.nodes
        .get_by_env(&env)
        .unwrap_or_default()
        .into_iter()
        .find(|n| n.uuid == node_id)
}

fn inbound_totals(metrics: &MetricStorage, node_id: &Uuid) -> PrivateNodeTraffic {
    let (mut up, mut down, mut conns) = (0.0_f64, 0.0_f64, 0.0_f64);
    if let Some(node_map) = metrics.inner.get(node_id) {
        for entry in node_map.iter() {
            let Some(meta) = metrics.metadata.get(entry.key()) else {
                continue;
            };
            let name = &meta.value().0;
            if !name.starts_with("net.inbound.") {
                continue;
            }
            let Some(last) = entry.value().back() else {
                continue;
            };
            if name.ends_with(".uplink") {
                up += last.value;
            } else if name.ends_with(".downlink") {
                down += last.value;
            } else if name.ends_with(".connections") {
                conns += last.value;
            }
        }
    }
    PrivateNodeTraffic {
        uplink: up.max(0.0) as u64,
        downlink: down.max(0.0) as u64,
        connections: conns.max(0.0) as u64,
    }
}

fn snapshot(metrics: &MetricStorage, node_id: &Uuid) -> super::admin::NodeMetricsSnapshot {
    let (mem_used, mem_total) = metrics.node_memory(node_id);
    let (net_rx, net_tx) = metrics.node_network_traffic(node_id);
    let cpu_percent = metrics.node_cpu_avg(node_id);
    let memory_percent = match (mem_used, mem_total) {
        (Some(used), Some(total)) if total > 0 => {
            Some(((used as f64 / total as f64) * 10000.0).round() / 100.0)
        }
        _ => None,
    };
    let disk_usage_percent = metrics.node_disk_usage(node_id, "root");
    let load_percent = [cpu_percent, memory_percent, disk_usage_percent]
        .iter()
        .filter_map(|v| *v)
        .reduce(f64::max);
    super::admin::NodeMetricsSnapshot {
        memory_used_bytes: mem_used,
        memory_total_bytes: mem_total,
        memory_percent,
        cpu_percent,
        disk_usage_percent,
        network_rx_bps: net_rx,
        network_tx_bps: net_tx,
        load_percent,
    }
}

/// `GET /private/nodes/stats?subscription_id=` — own nodes with live gauges.
/// Same ownership rule as the list above; nothing about shared envs leaks.
pub async fn private_node_stats_handler<N, C, S>(
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let env = {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&query.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        if sub.is_deleted() {
            return Ok(http::not_found("subscription not found").into_response());
        }
        owned_env(sub, query.subscription_id)
    };
    let nodes: Vec<PrivateNodeStats> = {
        let mem = memory.memory.read().await;
        mem.nodes
            .get_by_env(&env)
            .unwrap_or_default()
            .into_iter()
            .map(|n| {
                let res = n.as_node_response();
                let node_id = res.uuid;
                PrivateNodeStats {
                    node: PrivateNodeResponse {
                        uuid: res.uuid,
                        hostname: res.hostname,
                        address: res.address.to_string(),
                        label: res.label,
                        country: res.country,
                        status: res.status,
                        env: res.env,
                        inbounds: res.inbounds.iter().map(|i| i.tag.to_string()).collect(),
                    },
                    metrics: snapshot(&metrics, &node_id),
                    traffic: inbound_totals(&metrics, &node_id),
                }
            })
            .collect()
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(nodes),
        }),
        StatusCode::OK,
    )
    .into_response())
}

/// `GET /private/nodes/{uuid}/series?subscription_id=&from=&to=` — CPU, RAM and
/// network history of an own node, downsampled to ~240 buckets.
pub async fn private_node_series_handler<N, C, S>(
    node_id: Uuid,
    query: PrivateNodeSeriesQuery,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if owned_node(&memory, query.subscription_id, node_id)
        .await
        .is_none()
    {
        return Ok(http::not_found("node not found").into_response());
    }

    let now = chrono::Utc::now().timestamp_millis();
    let to = query.to.unwrap_or(now).min(now);
    // A week is the metrics retention; anything wider is clamped, not refused.
    let from = query
        .from
        .unwrap_or(to - 24 * 60 * 60 * 1000)
        .max(to - 7 * 24 * 60 * 60 * 1000);
    let series = node_series(&metrics, &node_id, from, to);

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(series),
        }),
        StatusCode::OK,
    )
    .into_response())
}

/// Bucketed CPU / RAM / network history of one node. Pure (metrics only), so
/// the downsampling can be tested without a running api.
fn node_series(metrics: &MetricStorage, node_id: &Uuid, from: i64, to: i64) -> PrivateNodeSeries {
    let bucket_ms = ((to - from) / 240).max(10_000);
    let bucket_of = |ts: i64| from + (ts - from) / bucket_ms * bucket_ms;

    let mut cpu: BTreeMap<i64, BucketAcc> = BTreeMap::new();
    let mut mem_used: BTreeMap<i64, f64> = BTreeMap::new();
    let mut mem_total: BTreeMap<i64, f64> = BTreeMap::new();
    // Interfaces are summed per timestamp, buckets then average those sums.
    let mut rx_ts: BTreeMap<i64, f64> = BTreeMap::new();
    let mut tx_ts: BTreeMap<i64, f64> = BTreeMap::new();

    if let Some(node_map) = metrics.inner.get(node_id) {
        for entry in node_map.iter() {
            let Some(meta) = metrics.metadata.get(entry.key()) else {
                continue;
            };
            let name = meta.value().0.as_str();
            let is_cpu = name.starts_with("sys.cpu.");
            let is_rx = name.starts_with("net.") && name.ends_with(".rx_bps");
            let is_tx = name.starts_with("net.") && name.ends_with(".tx_bps");
            let is_mem_used = name == "sys.mem_used";
            let is_mem_total = name == "sys.mem_total";
            if !(is_cpu || is_rx || is_tx || is_mem_used || is_mem_total) {
                continue;
            }
            for point in entry.value().iter() {
                if point.timestamp < from || point.timestamp > to {
                    continue;
                }
                let key = bucket_of(point.timestamp);
                if is_cpu {
                    cpu.entry(key).or_default().add(point.value);
                } else if is_rx {
                    *rx_ts.entry(point.timestamp).or_insert(0.0) += point.value;
                } else if is_tx {
                    *tx_ts.entry(point.timestamp).or_insert(0.0) += point.value;
                } else if is_mem_used {
                    mem_used.insert(key, point.value);
                } else if is_mem_total {
                    mem_total.insert(key, point.value);
                }
            }
        }
    }

    let memory_percent = mem_used
        .iter()
        .filter_map(|(ts, used)| {
            let total = mem_total.get(ts)?;
            if *total <= 0.0 {
                return None;
            }
            Some((*ts, ((used / total) * 10000.0).round() / 100.0))
        })
        .collect();

    let into_buckets = |points: BTreeMap<i64, f64>| -> Vec<(i64, f64)> {
        let mut buckets: BTreeMap<i64, BucketAcc> = BTreeMap::new();
        for (ts, value) in points {
            buckets.entry(bucket_of(ts)).or_default().add(value);
        }
        buckets.iter().map(|(ts, acc)| (*ts, acc.avg())).collect()
    };

    PrivateNodeSeries {
        cpu_percent: cpu.iter().map(|(ts, acc)| (*ts, acc.avg())).collect(),
        memory_percent,
        network_rx_bps: into_buckets(rx_ts),
        network_tx_bps: into_buckets(tx_ts),
    }
}

/// `GET /private/state?subscription_id=` — used / limit / remaining for the
/// account's servers (own nodes, offline included) and users (child
/// subscriptions that are not deleted). Public like the rest of the cabinet
/// API: the subscription UUID is the credential.
pub async fn private_state_handler<N, C, S>(
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let (servers, users, plan_kind, own_nodes_limit) = {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&query.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        if sub.is_deleted() {
            return Ok(http::not_found("subscription not found").into_response());
        }

        let limits = sub.selfhosted_limits();
        let env = owned_env(sub, query.subscription_id);
        let servers_used = mem
            .nodes
            .get_by_env(&env)
            .map(|nodes| nodes.len() as i64)
            .unwrap_or(0);
        // Users = child subscriptions that still exist; a deleted child
        // frees its slot, lowering a cap never deletes anything.
        let users_used = mem
            .subscriptions
            .find_by_parent_id(&query.subscription_id)
            .into_iter()
            .filter(|child| !child.is_deleted())
            .count() as i64;

        (
            LimitUsage::new(servers_used, limits.server_limit),
            LimitUsage::new(users_used, limits.user_limit),
            sub.plan_kind().to_string(),
            sub.own_nodes_limit(),
        )
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(PrivateStateResponse {
                subscription_id: query.subscription_id,
                plan_kind,
                own_nodes_limit,
                servers,
                users,
            }),
        }),
        StatusCode::OK,
    )
    .into_response())
}

pub async fn set_private_feed_handler<N, C, S>(
    node_uuid: Uuid,
    req: PrivateFeedRequest,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&req.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        if sub.is_deleted() {
            return Ok(http::not_found("subscription not found").into_response());
        }
        // The shard this account owns, as recorded: `sf0001` or the legacy
        // `custompersonal<uuid>`. Re-deriving it would not match `sf`.
        let env = sub
            .scope_env()
            .filter(|env| env.is_personal())
            .cloned()
            .unwrap_or_else(|| personal_env(req.subscription_id));
        let Some(node) = mem.nodes.get_by_id(&node_uuid) else {
            return Ok(http::not_found("node not found").into_response());
        };
        if node.env != env || !node.env.is_personal() {
            return Ok(forbidden("node is not owned by this subscription").into_response());
        }
    }

    let saved = if req.include {
        memory.db.private_feed().insert(node_uuid).await
    } else {
        memory.db.private_feed().delete(node_uuid).await
    };
    if let Err(e) = saved {
        tracing::error!("private feed flag failed: {}", e);
        return Ok(http::internal_error("db error").into_response());
    }

    {
        let mut mem = memory.memory.write().await;
        if req.include {
            mem.main_feed_nodes.insert(node_uuid);
        } else {
            mem.main_feed_nodes.remove(&node_uuid);
        }
    }

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage::<Option<IdResponse>> {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(IdResponse { id: node_uuid }),
        }),
        StatusCode::OK,
    )
    .into_response())
}

pub async fn delete_private_node_handler<N, C, S>(
    node_uuid: Uuid,
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&query.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        if sub.is_deleted() {
            return Ok(http::not_found("subscription not found").into_response());
        }
        // The recorded shard (`sf0001` or legacy `custompersonal<uuid>`).
        let env = sub
            .scope_env()
            .filter(|env| env.is_personal())
            .cloned()
            .unwrap_or_else(|| personal_env(query.subscription_id));

        let Some(node) = mem.nodes.get_by_id(&node_uuid) else {
            return Ok(http::not_found("node not found").into_response());
        };
        if node.env != env || !node.env.is_personal() {
            return Ok(forbidden("node is not owned by this subscription").into_response());
        }
    }

    match SyncOp::delete_node(&memory, &node_uuid).await {
        Ok(Status::Ok(_)) | Ok(Status::NotFound(_)) => Ok(warp::reply::with_status(
            warp::reply::json(&ResponseMessage::<Option<IdResponse>> {
                status: StatusCode::OK.as_u16(),
                message: "Ok".to_string(),
                response: Some(IdResponse { id: node_uuid }),
            }),
            StatusCode::OK,
        )
        .into_response()),
        Ok(_) => Ok(http::bad_request("operation not supported").into_response()),
        Err(e) => {
            tracing::error!("private node delete failed: {}", e);
            Ok(http::internal_error("delete failed").into_response())
        }
    }
}

/// `issued_via` marker of connections minted for a selfhosted user.
const ISSUED_VIA_USER: &str = "user";

/// Shared gate of the `/private/users*` endpoints: the subscription must
/// exist, be a selfhosted account and not be a user itself (a user cannot
/// invite users — nested accounts are refused).
async fn check_user_owner<N, C, S>(
    memory: &MemSync<N, C, S>,
    owner_id: Uuid,
) -> Result<(), warp::reply::Response>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let mem = memory.memory.read().await;
    let Some(owner) = mem.subscriptions.find_by_id(&owner_id) else {
        return Err(http::not_found("subscription not found").into_response());
    };
    if owner.is_deleted() {
        return Err(http::not_found("subscription not found").into_response());
    }
    if owner.parent_id().is_some() {
        return Err(
            http::bad_request("a user of a selfhosted account cannot invite users").into_response(),
        );
    }
    // Своих юзеров может заводить любой аккаунт, а не только selfhosted:
    // «своё железо + свои люди» не должно требовать второго аккаунта. Границу
    // задаёт лимит (`user_limit`), его поднимает админ.
    if owner.user_limit() <= 0 {
        return Err(http::bad_request("user limit is 0 for this account").into_response());
    }
    Ok(())
}

/// Best-effort rollback of a half-built user: delete the connections already
/// minted (each delete publishes a tombstone to its node, so no orphan AWG
/// peer stays behind) and drop the child subscription row, which also frees
/// the user slot.
async fn rollback_private_user<N, C, S>(
    memory: &MemSync<N, C, S>,
    child_id: Uuid,
    created: &[(Uuid, Tag, Option<Uuid>)],
) where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    for (conn_id, _, _) in created {
        let conn = {
            let mem = memory.memory.read().await;
            mem.connections.get(conn_id).cloned()
        };
        if let Some(conn) = conn {
            if let Err(e) =
                SyncOp::delete_connection(memory, conn_id, &conn, Some(DELETE_REASON_MANUAL)).await
            {
                tracing::error!(
                    "private user rollback: connection {} delete failed: {}",
                    conn_id,
                    e
                );
            }
        }
    }

    {
        let mut mem = memory.memory.write().await;
        if let Some(child) = mem.subscriptions.find_by_id_mut(&child_id) {
            child.mark_deleted();
        }
    }
    if let Err(e) = memory.db.sub().delete(&child_id).await {
        tracing::error!(
            "private user rollback: subscription {} delete failed: {}",
            child_id,
            e
        );
    }
}

/// `POST /private/users` — a selfhosted account invites one user: a child
/// subscription that carries exactly the account's chosen nodes.
///
/// The user gets its own AWG keys and addresses (own connections inside the
/// owner's personal env), so traffic is per-user and a revoke kills only that
/// user. `node_ids` mints one connection per selected node (pinned);
/// `all_servers` mints unpinned connections, which every node of the env
/// serves — including nodes registered later.
pub async fn create_private_user_handler<N, C, S>(
    req: PrivateUserCreateRequest,
    memory: MemSync<N, C, S>,
    wg_network: IpAddrMask,
    awg_network: IpAddrMask,
    awg_mobile_network: Option<IpAddrMask>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if let Err(msg) = validate_user_selection(req.all_servers, &req.node_ids) {
        return Ok(http::bad_request(msg).into_response());
    }

    let owner_id = req.subscription_id;
    if let Err(reply) = check_user_owner(&memory, owner_id).await {
        return Ok(reply);
    }

    // Resell is a flag: only partners may create users on their own nodes.
    {
        let mem = memory.memory.read().await;
        if let Some(owner) = mem.subscriptions.find_by_id(&owner_id) {
            if !owner.can_resell() {
                return Ok(super::share::forbidden(
                    "resell is not enabled for this account",
                ));
            }
        }
    }

    // The account's own nodes, and the (proto, node) pairs to mint for the
    // user. Only the owner's personal env is ever touched: a uuid of a node
    // from another env is refused, an unknown uuid is a 404.
    let env = {
        let mem = memory.memory.read().await;
        mem.subscriptions
            .find_by_id(&owner_id)
            .and_then(|sub| sub.scope_env().filter(|env| env.is_personal()).cloned())
            .unwrap_or_else(|| personal_env(owner_id))
    };
    let mobile_net_configured = awg_mobile_network.is_some();

    let (plans, node_ids) = {
        let mem = memory.memory.read().await;
        let env_nodes: Vec<Node> = mem.nodes.get_by_env(&env).unwrap_or_default();

        let protos_of = |node: &Node| -> Vec<Tag> {
            awg_protos(node)
                .into_iter()
                .filter(|tag| *tag != Tag::AmneziaWgMobile || mobile_net_configured)
                .collect()
        };

        let mut plans: Vec<UserConnPlan> = Vec::new();
        let mut node_ids: Vec<Uuid> = Vec::new();

        if req.all_servers {
            let mut protos: Vec<Tag> = Vec::new();
            for node in &env_nodes {
                for tag in protos_of(node) {
                    if !protos.contains(&tag) {
                        protos.push(tag);
                    }
                }
            }
            plans = protos
                .into_iter()
                .map(|proto| UserConnPlan {
                    proto,
                    node_id: None,
                })
                .collect();
        } else {
            for id in &req.node_ids {
                let Some(node) = env_nodes.iter().find(|node| node.uuid == *id) else {
                    return Ok(match mem.nodes.get_by_id(id) {
                        Some(_) => forbidden("node belongs to another account").into_response(),
                        None => http::not_found("node not found").into_response(),
                    });
                };
                let protos = protos_of(node);
                if protos.is_empty() {
                    return Ok(http::bad_request("node has no amnezia-wg inbound").into_response());
                }
                node_ids.push(*id);
                for proto in protos {
                    plans.push(UserConnPlan {
                        proto,
                        node_id: Some(*id),
                    });
                }
            }
        }

        if plans.is_empty() {
            return Ok(
                http::bad_request("the account has no amnezia-wg servers yet").into_response(),
            );
        }

        (plans, node_ids)
    };

    // Reserve the user slot under the account's cap: lock the account row,
    // count its live children and insert the child in one transaction, so two
    // concurrent invites cannot both take the last free slot.
    let child = Subscription::new_user_child(owner_id);
    let child_id = child.id;

    let users = match memory
        .db
        .sub()
        .insert_child_within_user_limit(&owner_id, &child)
        .await
    {
        Ok(Some(slot)) if slot.inserted => LimitUsage::new(slot.used, slot.limit),
        Ok(Some(slot)) => {
            return Ok(limit_reached("user_limit_reached", slot.used, slot.limit).into_response())
        }
        Ok(None) => return Ok(http::not_found("subscription not found").into_response()),
        Err(e) => {
            tracing::error!("private user insert failed: {}", e);
            return Ok(http::internal_error("create failed").into_response());
        }
    };

    // The row went into PG first (the slot had to be taken inside the same
    // transaction); mirror it into the memory cache so the config endpoints
    // can serve the user right away.
    {
        let mut mem = memory.memory.write().await;
        mem.subscriptions.add(child.clone().into());
    }

    let mut created: Vec<(Uuid, Tag, Option<Uuid>)> = Vec::new();
    for plan in &plans {
        match super::connection::create_connection_inner(
            &env,
            plan.proto,
            Some(child_id),
            None,
            None,
            plan.node_id,
            Some(ISSUED_VIA_USER.to_string()),
            true,
            &memory,
            &wg_network,
            &awg_network,
            &awg_mobile_network,
        )
        .await
        {
            Ok((conn_id, _)) => created.push((conn_id, plan.proto, plan.node_id)),
            Err(e) => {
                tracing::error!(
                    "private user mint failed (proto {:?}, node {:?}): {}",
                    plan.proto,
                    plan.node_id,
                    e
                );
                rollback_private_user(&memory, child_id, &created).await;
                return Ok(http::internal_error("create failed").into_response());
            }
        }
    }

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(PrivateUserResponse {
                subscription_id: child_id,
                subscription_url: user_subscription_url(child_id),
                all_servers: req.all_servers,
                node_ids,
                connections: created
                    .iter()
                    .map(|(id, proto, node_id)| PrivateUserConnection {
                        id: *id,
                        proto: proto.to_string(),
                        node_id: *node_id,
                    })
                    .collect(),
                users,
            }),
        }),
        StatusCode::OK,
    )
    .into_response())
}

/// `GET /private/users?subscription_id=` — the account's users with their
/// share links and current node selection.
pub async fn list_private_users_handler<N, C, S>(
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if let Err(reply) = check_user_owner(&memory, query.subscription_id).await {
        return Ok(reply);
    }

    let users: Vec<PrivateUserListItem> = {
        let mem = memory.memory.read().await;
        mem.subscriptions
            .find_by_parent_id(&query.subscription_id)
            .into_iter()
            .filter(|child| !child.is_deleted())
            .map(|child| {
                let conns: Vec<(Option<Uuid>, bool)> = mem
                    .connections
                    .get_by_subscription_id(&child.id())
                    .unwrap_or_default()
                    .iter()
                    .map(|(id, conn)| (mem.conn_nodes.get(id).copied(), conn.get_deleted()))
                    .collect();
                let (all_servers, node_ids) = user_node_selection(&conns);
                PrivateUserListItem {
                    subscription_id: child.id(),
                    subscription_url: user_subscription_url(child.id()),
                    all_servers,
                    node_ids,
                    connections: conns.iter().filter(|(_, deleted)| !*deleted).count(),
                    created_at: child.created_at(),
                }
            })
            .collect()
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(users),
        }),
        StatusCode::OK,
    )
    .into_response())
}

/// `DELETE /private/users/{user}?subscription_id=<owner>` — revokes one user:
/// cuts every connection of the child (tombstones go out to the nodes, so its
/// AWG peers disappear there) and marks the child subscription deleted, which
/// frees its slot in the account's user cap.
pub async fn delete_private_user_handler<N, C, S>(
    child_id: Uuid,
    query: PrivateNodesQuery,
    memory: MemSync<N, C, S>,
) -> Result<impl Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if let Err(reply) = check_user_owner(&memory, query.subscription_id).await {
        return Ok(reply);
    }

    // Only a user of *this* account can be revoked: a foreign uuid, a premium
    // child or an already deleted one answers a uniform 404.
    let conns: Vec<(Uuid, C)> = {
        let mem = memory.memory.read().await;
        let Some(child) = mem.subscriptions.find_by_id(&child_id) else {
            return Ok(http::not_found("user not found").into_response());
        };
        if child.is_deleted()
            || !child.is_selfhosted_user()
            || child.parent_id() != Some(query.subscription_id)
        {
            return Ok(http::not_found("user not found").into_response());
        }
        mem.connections
            .get_by_subscription_id(&child_id)
            .unwrap_or_default()
            .iter()
            .filter(|(_, conn)| !conn.get_deleted())
            .map(|(id, conn)| (*id, conn.clone()))
            .collect()
    };

    let mut deleted = 0usize;
    for (conn_id, conn) in &conns {
        match SyncOp::delete_connection(&memory, conn_id, conn, Some(DELETE_REASON_MANUAL)).await {
            Ok(_) => deleted += 1,
            Err(e) => {
                tracing::error!(
                    "private user revoke: connection {} delete failed: {}",
                    conn_id,
                    e
                );
                return Ok(http::internal_error("delete failed").into_response());
            }
        }
    }

    {
        let mut mem = memory.memory.write().await;
        if let Some(child) = mem.subscriptions.find_by_id_mut(&child_id) {
            child.mark_deleted();
        }
    }
    if let Err(e) = memory.db.sub().delete(&child_id).await {
        tracing::error!(
            "private user revoke: subscription {} delete failed: {}",
            child_id,
            e
        );
        return Ok(http::internal_error("delete failed").into_response());
    }

    Ok(warp::reply::with_status(
        warp::reply::json(&ResponseMessage {
            status: StatusCode::OK.as_u16(),
            message: "Ok".to_string(),
            response: Some(PrivateUserRevokeResponse {
                subscription_id: child_id,
                deleted: true,
                connections_deleted: deleted,
            }),
        }),
        StatusCode::OK,
    )
    .into_response())
}

async fn refresh_known_private_node<N, C, S>(
    token: &str,
    mut node_req: NodeRequest,
    memory: MemSync<N, C, S>,
    metrics: Arc<MetricStorage>,
) -> Result<warp::reply::Response, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let known = match memory.db.node_access_token().find_by_token(token).await {
        Ok(Some(id)) => id,
        Ok(None) => return Ok(unauthorized("invalid node token").into_response()),
        Err(_) => return Ok(http::internal_error("db error").into_response()),
    };
    if known != node_req.uuid {
        return Ok(forbidden("node uuid does not match token").into_response());
    }

    let (existing_env, existing_pool) = {
        let mem = memory.memory.read().await;
        match mem.nodes.get_by_id(&known) {
            Some(node) => (Some(node.env.clone()), node.pool.clone()),
            None => (None, None),
        }
    };
    if existing_env.is_none() && !node_req.env.is_personal() {
        return Ok(http::bad_request("personal env required").into_response());
    }

    let env = match refresh_env(existing_env.as_ref(), &node_req.env) {
        Ok(env) => env,
        Err(msg) => return Ok(http::bad_request(msg).into_response()),
    };

    if let Err(e) = node_req.validate() {
        return Ok(http::bad_request(&e.to_string()).into_response());
    }

    node_req.env = env.clone();
    node_req.uuid = known;
    node_req.r#type = Some(NodeType::SelfHosted);
    node_req.cluster = None;
    // The pool stays the account's own grouping: a node that declares one keeps
    // it, a node that declares none keeps whatever the shard already stores (a
    // refresh must not wipe a pool because the config was edited).
    node_req.pool = match private_pool(node_req.pool.clone(), existing_pool) {
        Ok(pool) => pool,
        Err(msg) => return Ok(http::bad_request(&msg).into_response()),
    };
    // Same rule as the first registration: on a numbered shard a node with no
    // pool of its own is its own pool.
    if node_req.pool.is_none() && node_req.env.selfhosted_number().is_some() {
        node_req.pool = Some(node_req.hostname.clone());
    }

    let node = node_req.as_node();

    // Same contract as the first registration: the interface must sit in the
    // shard's block, and the shard takes the block the node brings when it has
    // none (a token minted before the block existed).
    if let Err(msg) = settle_private_block(&memory, &node).await {
        return Ok(http::bad_request(&msg).into_response());
    }

    if existing_env.is_none() {
        // A node we have never seen (its row was deleted while the node token
        // survived): reserve a slot under the account's server cap, through
        // the same transactional path as the install-token registration.
        match memory
            .db
            .node()
            .insert_private_node_with_limit(&env, known, node.clone())
            .await
        {
            Ok(Some(slot)) if slot.inserted => {}
            Ok(Some(slot)) => {
                return Ok(
                    limit_reached("server_limit_reached", slot.used, slot.limit).into_response()
                )
            }
            Ok(None) => return Ok(http::bad_request("unknown personal scope").into_response()),
            Err(e) => {
                tracing::error!("private node reserve failed: {}", e);
                return Ok(http::internal_error("register failed").into_response());
            }
        }
    }

    let status = SyncOp::add_node(&memory, &known, node.clone()).await;
    match status {
        Ok(Status::Ok(id)) | Ok(Status::AlreadyExist(id)) | Ok(Status::NotModified(id)) => {
            let _ =
                SyncOp::update_node_status(&memory, &known, &node.env, NodeStatus::Online).await;
            note_heartbeat(&metrics, known, node.get_base_tags());
            // A refresh is a registration: the node gets its card rights and
            // the authoritative peer set again, so a lost snapshot heals on a
            // plain `systemctl restart fnode`.
            let shard_owner = {
                // `sfNNNN` carries no subscription id: the owner is the account
                // whose recorded scope is this shard. The legacy
                // `custompersonal<uuid>` spelling still parses straight from the
                // name, which covers accounts created before the numbered scheme.
                let from_name = subscription_from_personal_env(&node.env);
                if from_name.is_some() {
                    from_name
                } else {
                    let mem = memory.memory.read().await;
                    mem.subscriptions
                        .all()
                        .into_iter()
                        .find(|sub| sub.scope_env() == Some(&node.env))
                        .map(|sub| sub.id())
                }
            };
            if let Some(subscription_id) = shard_owner {
                seed_private_shard(&memory, subscription_id, &node).await;
            } else {
                tracing::warn!(
                    "private: cannot derive the subscription of shard {}; card rights skipped",
                    node.env
                );
            }
            Ok(warp::reply::with_status(
                warp::reply::json(&ResponseMessage {
                    status: StatusCode::OK.as_u16(),
                    message: "Ok".to_string(),
                    response: Some(PrivateRegisterResponse {
                        id,
                        node_token: token.to_string(),
                        scope_env: env.to_string(),
                    }),
                }),
                StatusCode::OK,
            )
            .into_response())
        }
        Ok(_) => Ok(http::bad_request("operation not supported").into_response()),
        Err(e) => {
            tracing::error!("private node refresh failed: {}", e);
            Ok(http::internal_error("register failed").into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shard holds several pools: the declared name is kept, an absent one
    /// keeps what the shard already stores, junk is refused.
    #[test]
    fn private_pool_keeps_what_the_account_declared() {
        // Declared pool wins, trimmed.
        assert_eq!(
            private_pool(Some("  norway  ".into()), None),
            Ok(Some("norway".into()))
        );
        // A different declaration replaces the stored one.
        assert_eq!(
            private_pool(Some("germany".into()), Some("norway".into())),
            Ok(Some("germany".into()))
        );
        // Nothing declared on a fresh node: no pool, the node keeps its own card.
        assert_eq!(private_pool(None, None), Ok(None));
        // A refresh that does not declare a pool must not wipe the stored one.
        assert_eq!(
            private_pool(None, Some("norway".into())),
            Ok(Some("norway".into()))
        );
        // Blank is "no pool", not a pool named " ".
        assert_eq!(private_pool(Some("   ".into()), None), Ok(None));
        assert!(private_pool(Some("x".repeat(65)), None).is_err());
        assert!(private_pool(Some("bad\u{7}name".into()), None).is_err());
    }

    #[test]
    fn personal_env_is_isolated_from_shared() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let env = Env::personal_for(id);
        assert!(env.is_personal());
        assert!(!env.is_frkn_shared());
        assert!(env.to_string().starts_with("custompersonal"));
        assert!(!Env::Dev.is_personal());
        assert!(Env::Dev.is_frkn_shared());
    }

    /// (b) A standard account is born with a quota and mints a token for its
    /// own shard — no `plan_kind = selfhosted` in the gate.
    #[test]
    fn standard_account_with_quota_may_mint_an_install_token() {
        let id = Uuid::new_v4();
        let sub = Subscription::new(id, "ref".to_string(), None, None)
            .with_own_nodes_default(fcore::DEFAULT_OWN_NODES_LIMIT);
        assert_eq!(sub.plan_kind(), PlanKind::Standard);
        assert_eq!(sub.own_nodes_limit(), fcore::DEFAULT_OWN_NODES_LIMIT);

        assert_eq!(
            install_token_denial(sub.parent_id(), sub.own_nodes_limit(), 0, 0),
            None,
            "own nodes are a capability of a standard account"
        );

        // The token is scoped to the account's own shard, one per account.
        let env = personal_env(id);
        assert_eq!(env.to_string(), format!("custompersonal{}", id.as_simple()));
        assert_eq!(install_token_denial(None, 1, 0, 0), None);
    }

    /// (c) A user of an own-node account never hosts nodes.
    #[test]
    fn child_account_is_refused_whatever_its_quota() {
        let child = Subscription::new_user_child(Uuid::new_v4());
        assert!(child.parent_id().is_some());
        assert_eq!(
            install_token_denial(child.parent_id(), child.own_nodes_limit(), 0, 0),
            Some(InstallTokenDenial::ChildAccount)
        );
        // Even if a quota were forced onto a child row.
        assert_eq!(
            install_token_denial(child.parent_id(), 3, 0, 0),
            Some(InstallTokenDenial::ChildAccount)
        );
    }

    /// (d) A zero quota still refuses, with the same message as before.
    #[test]
    fn zero_quota_is_refused() {
        assert_eq!(
            install_token_denial(None, 0, 0, 0),
            Some(InstallTokenDenial::NotEnabled)
        );
        assert_eq!(
            install_token_denial(None, -1, 0, 0),
            Some(InstallTokenDenial::NotEnabled)
        );
    }

    #[test]
    fn caps_still_apply() {
        assert_eq!(
            install_token_denial(None, 3, 3, 0),
            Some(InstallTokenDenial::ServerLimit { used: 3, limit: 3 })
        );
        assert_eq!(
            install_token_denial(None, 3, 0, MAX_ACTIVE_INSTALL_TOKENS),
            Some(InstallTokenDenial::TooManyTokens)
        );
        // The server cap is checked before the token cap: a full account is
        // told about slots, not about tokens.
        assert_eq!(
            install_token_denial(None, 1, 1, MAX_ACTIVE_INSTALL_TOKENS),
            Some(InstallTokenDenial::ServerLimit { used: 1, limit: 1 })
        );
    }

    #[test]
    fn refresh_keeps_known_personal_env() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let personal = Env::personal_for(id);
        assert_eq!(refresh_env(Some(&personal), &Env::Dev).unwrap(), personal);
        assert!(refresh_env(Some(&Env::Dev), &personal).is_err());
        assert_eq!(refresh_env(None, &personal).unwrap(), personal);
        assert!(refresh_env(None, &Env::Dev).is_err());
    }

    #[test]
    fn seeds_three_protocols_only_when_no_private_nodes() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let env = Env::personal_for(id);
        let none = std::collections::HashSet::new();
        assert_eq!(
            private_seed_tags(0, &env, &none),
            vec![Tag::AmneziaWg, Tag::AmneziaWgMobile, Tag::Hysteria2]
        );
        assert!(private_seed_tags(1, &env, &none).is_empty());
        assert!(private_seed_tags(3, &env, &none).is_empty());

        let mut present = std::collections::HashSet::new();
        present.insert((env.clone(), Tag::Hysteria2));
        assert_eq!(
            private_seed_tags(0, &env, &present),
            vec![Tag::AmneziaWg, Tag::AmneziaWgMobile]
        );
    }

    #[test]
    fn user_selection_rejects_ambiguous_and_empty_input() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        // all_servers XOR a non-empty, duplicate-free subset.
        assert!(validate_user_selection(true, &[]).is_ok());
        assert!(validate_user_selection(false, &[a, b]).is_ok());
        assert!(validate_user_selection(true, &[a]).is_err());
        assert!(validate_user_selection(false, &[]).is_err());
        assert!(validate_user_selection(false, &[a, a]).is_err());
    }

    #[test]
    fn user_node_selection_reads_back_pins_and_modes() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        // Unpinned connections = all-servers mode, pins are not reported.
        assert_eq!(user_node_selection(&[(None, false)]), (true, vec![]));
        assert_eq!(
            user_node_selection(&[(Some(a), false), (None, false)]),
            (true, vec![])
        );
        // Pinned connections = subset mode, distinct nodes in order.
        assert_eq!(
            user_node_selection(&[(Some(a), false), (Some(a), false), (Some(b), false)]),
            (false, vec![a, b])
        );
        // Deleted connections are ignored.
        assert_eq!(
            user_node_selection(&[(Some(a), true), (Some(b), false)]),
            (false, vec![b])
        );
        assert_eq!(user_node_selection(&[(None, true)]), (false, vec![]));
    }

    #[test]
    fn user_subscription_url_shape() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        assert_eq!(user_subscription_url(id), format!("frkn://sub/{}", id));
    }

    /// Metrics fixture: one series per (name), points at `ts`.
    fn metrics_with(node_id: Uuid, series: &[(&str, i64, f64)]) -> MetricStorage {
        let storage = MetricStorage::new(1000, 604_800);
        {
            let node = storage.inner.entry(node_id).or_default();
            for (idx, (name, ts, value)) in series.iter().enumerate() {
                let hash = idx as u64 + 1;
                node.insert(
                    hash,
                    std::collections::VecDeque::from(vec![fcore::MetricPoint {
                        timestamp: *ts,
                        value: *value,
                    }]),
                );
                storage
                    .metadata
                    .insert(hash, (name.to_string(), BTreeMap::new()));
            }
        }
        storage
    }

    #[test]
    fn node_series_averages_cpu_and_sums_interfaces() {
        let node_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let ts = 1_700_000_000_000;
        let storage = metrics_with(
            node_id,
            &[
                ("sys.cpu.0", ts, 10.0),
                ("sys.cpu.1", ts, 30.0),
                ("sys.mem_used", ts, 512.0),
                ("sys.mem_total", ts, 1024.0),
                ("net.eth0.rx_bps", ts, 100.0),
                ("net.eth1.rx_bps", ts, 20.0),
                ("net.eth0.tx_bps", ts, 40.0),
                // Not a chart series: must not leak into the buckets.
                ("net.inbound.AmneziaWg.uplink", ts, 999.0),
            ],
        );

        let series = node_series(&storage, &node_id, ts - 1, ts + 1);
        let values = |points: &[(i64, f64)]| -> Vec<f64> { points.iter().map(|p| p.1).collect() };
        assert_eq!(values(&series.cpu_percent), vec![20.0]);
        assert_eq!(values(&series.memory_percent), vec![50.0]);
        assert_eq!(values(&series.network_rx_bps), vec![120.0]);
        assert_eq!(values(&series.network_tx_bps), vec![40.0]);
        // Buckets are anchored at the window start and stay inside it.
        assert!(series.cpu_percent[0].0 >= ts - 1 && series.cpu_percent[0].0 <= ts + 1);
    }

    #[test]
    fn node_series_ignores_points_outside_the_window() {
        let node_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let storage = metrics_with(
            node_id,
            &[("sys.cpu.0", 1_000, 5.0), ("sys.cpu.0", 5_000, 90.0)],
        );
        // Only the point inside the window survives the filter.
        let series = node_series(&storage, &node_id, 4_000, 6_000);
        let values: Vec<f64> = series.cpu_percent.iter().map(|p| p.1).collect();
        assert_eq!(values, vec![90.0]);
    }

    #[test]
    fn inbound_totals_sum_uplink_downlink_and_connections() {
        let node_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let storage = metrics_with(
            node_id,
            &[
                ("net.inbound.AmneziaWg.uplink", 1, 1_000.0),
                ("net.inbound.AmneziaWg.downlink", 1, 2_000.0),
                ("net.inbound.AmneziaWg.connections", 1, 3.0),
                ("net.inbound.Xray.uplink", 1, 500.0),
                ("net.inbound.Xray.downlink", 1, 700.0),
                ("net.inbound.Xray.connections", 1, 2.0),
                ("sys.cpu.0", 1, 42.0),
            ],
        );

        let totals = inbound_totals(&storage, &node_id);
        assert_eq!(totals.uplink, 1_500);
        assert_eq!(totals.downlink, 2_700);
        assert_eq!(totals.connections, 5);
    }
}
