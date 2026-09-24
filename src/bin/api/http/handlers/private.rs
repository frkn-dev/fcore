use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warp::http::StatusCode;
use warp::reply::Reply;

use fcore::{
    http::{helpers as http, IdResponse, ResponseMessage},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Env, NodeStatus,
    NodeStorageOperations, NodeType, Status, Subscription, SubscriptionOperations,
    SubscriptionStorageOperations,
};

use super::super::{
    super::{
        postgres::install_token::{MAX_ACTIVE_INSTALL_TOKENS, MAX_PRIVATE_NODES_PER_SUB},
        sync::{tasks::SyncOp, MemSync},
    },
    request::NodeRequest,
};

#[derive(Debug, Deserialize)]
pub struct InstallTokenRequest {
    pub subscription_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct InstallTokenResponse {
    pub token: String,
    pub scope_env: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub install_hint: String,
}

#[derive(Debug, Serialize)]
pub struct PrivateRegisterResponse {
    pub id: Uuid,
    pub node_token: String,
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
    let personal = personal_env(subscription_id);

    let (needs_set, existing_token) = {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&subscription_id) else {
            return Err(http::not_found("subscription not found"));
        };
        if sub.is_deleted() {
            return Err(http::not_found("subscription not found"));
        }
        let needs_set = sub.scope_env().is_none();
        let existing_token = sub.premium_token().map(|t| t.to_string());
        (needs_set, existing_token)
    };

    if needs_set {
        memory
            .db
            .sub()
            .set_premium_fields(
                &subscription_id,
                Some(&personal),
                existing_token.as_deref(),
            )
            .await
            .map_err(|_| http::internal_error("failed to set personal scope"))?;

        let mut mem = memory.memory.write().await;
        if let Some(sub) = mem.subscriptions.find_by_id_mut(&subscription_id) {
            sub.set_scope_env(personal.clone());
        }
    }

    Ok(personal)
}

pub async fn mint_install_token_handler<N, C, S>(
    req: InstallTokenRequest,
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
    let scope_env = match ensure_personal_scope(&memory, req.subscription_id).await {
        Ok(env) => env,
        Err(reply) => return Ok(reply.into_response()),
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

    if active >= MAX_ACTIVE_INSTALL_TOKENS {
        return Ok(too_many_requests(&format!(
            "too many active install tokens (max {})",
            MAX_ACTIVE_INSTALL_TOKENS
        ))
        .into_response());
    }

    let token = format!("inst_{}", Uuid::new_v4().simple());
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

pub async fn register_private_node_handler<N, C, S>(
    auth_header: Option<String>,
    mut node_req: NodeRequest,
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
    let token = auth_header
        .as_deref()
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");

    if token.is_empty() || !token.starts_with("inst_") {
        return Ok(unauthorized("install token required").into_response());
    }

    let row = match memory.db.install_token().find_usable(token).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Ok(unauthorized("invalid or expired install token").into_response())
        }
        Err(_) => return Ok(http::internal_error("db error").into_response()),
    };

    if !row.scope_env.is_personal() || row.scope_env.is_frkn_shared() {
        return Ok(http::bad_request("invalid personal scope").into_response());
    }

    let expected = personal_env(row.subscription_id);
    if row.scope_env != expected {
        return Ok(http::bad_request("scope mismatch").into_response());
    }

    {
        let mem = memory.memory.read().await;
        let count = mem
            .nodes
            .get_by_env(&expected)
            .map(|nodes| nodes.len() as i64)
            .unwrap_or(0);
        if count >= MAX_PRIVATE_NODES_PER_SUB {
            return Ok(too_many_requests(&format!(
                "private node limit reached (max {})",
                MAX_PRIVATE_NODES_PER_SUB
            ))
            .into_response());
        }
    }

    if let Err(e) = node_req.validate() {
        return Ok(http::bad_request(&e.to_string()).into_response());
    }

    node_req.env = expected.clone();
    node_req.r#type = Some(NodeType::Node);
    node_req.cluster = None;

    let node = node_req.clone().as_node();
    if node.env.is_frkn_shared() || !node.env.is_personal() {
        return Ok(http::bad_request("refusing shared env for private node").into_response());
    }

    let node_id = node_req.uuid;
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
    {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&query.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        if sub.is_deleted() {
            return Ok(http::not_found("subscription not found").into_response());
        }
    }

    let env = personal_env(query.subscription_id);
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
    let env = personal_env(query.subscription_id);

    {
        let mem = memory.memory.read().await;
        let Some(sub) = mem.subscriptions.find_by_id(&query.subscription_id) else {
            return Ok(http::not_found("subscription not found").into_response());
        };
        if sub.is_deleted() {
            return Ok(http::not_found("subscription not found").into_response());
        }

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
