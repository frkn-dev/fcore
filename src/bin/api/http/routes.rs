use async_trait::async_trait;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;
use warp::{Filter, Reply};

use fcore::{
    http::filters::{with_param_bool, with_param_string, with_param_u64},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, Env, MetricStorage,
    NodeStorageOperations, Result, Subscription, SubscriptionOperations,
};

use super::{
    super::{iap::AppleIapClient, service::Service, sync::MemSync},
    crypto::{self, AesContext},
    filters::*,
    handlers::{
        admin::*, amnezia::*, catalog::*, cluster::*, connection::*, geo as geo_handlers,
        healthcheck_handler, iap::*, key::*, metrics::*, node::*, peers as peers_handlers,
        premium::*, premium_inventory::*, private::*, score::*, share::*, subscription::*,
    },
    param::*,
    rejection,
    request::*,
};

#[async_trait]
pub trait Http {
    async fn run_http(&self) -> Result<()>;
}

#[async_trait]
impl<N, C, S> Http for Service<N, C, S>
where
    C: ConnectionBaseOperations
        + ConnectionApiOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + serde::Serialize
        + PartialEq
        + Into<Connection>,

    N: NodeStorageOperations + Send + Sync + Clone + std::default::Default,
    Connection: From<C>,

    S: SubscriptionOperations
        + Send
        + Sync
        + Clone
        + 'static
        + PartialEq
        + From<Subscription>
        + std::default::Default,
    Vec<(Uuid, fcore::Connection)>: FromIterator<(Uuid, C)>,
{
    async fn run_http(&self) -> Result<()> {
        let admin_token = self
            .settings
            .service
            .admin_token
            .clone()
            .unwrap_or_default();
        let mgmt_auth = with_service_or_admin_auth(
            Arc::new(self.settings.service.token.clone()),
            admin_token.clone(),
        );
        let node_sync_auth = with_mgmt_auth(
            Arc::new(self.settings.service.token.clone()),
            admin_token.clone(),
            self.sync.clone(),
        );
        let connections_sync_auth = with_connections_sync_auth(
            Arc::new(self.settings.service.token.clone()),
            admin_token,
            self.sync.clone(),
        );

        let params = &self.settings.service;
        // Public base URL of THIS api (feed links). Falls back to base_url
        // (the site URL) when unset — but then feed links point at the site
        // host, which is almost never what you want; set api_url in config.
        let api_base = params
            .api_url
            .clone()
            .unwrap_or_else(|| params.base_url.clone());
        let cors_origins = params.cors_origins.clone();

        let mut cors_builder = warp::cors()
            .allow_methods(vec!["GET", "POST", "PUT", "DELETE", "OPTIONS"])
            .allow_headers(vec![
                "Content-Type",
                "Authorization",
                "X-Requested-With",
                "X-Trace-Id",
            ])
            .allow_credentials(true)
            .max_age(86400);

        for origin in &cors_origins {
            cors_builder = cors_builder.allow_origin(origin.as_str());
        }

        let cors = cors_builder.build();

        let agw_key = self.agw_private_key.clone();

        tracing::debug!("Cors: {:?}", cors);

        let get_healthcheck_route = warp::get()
            .and(warp::path("healthcheck"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and_then(healthcheck_handler);

        // Node routes
        let get_nodes_route = warp::get()
            .and(warp::path("nodes"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::query::<NodesQueryParams>())
            .and(with_sync(self.sync.clone()))
            .and_then(get_nodes_handler);

        let get_status_route = warp::get()
            .and(warp::path("status"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and_then(get_nodes_status_handler);

        let get_node_route = warp::path!("node" / Uuid)
            .and(warp::get())
            .and(mgmt_auth.clone())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and_then(get_node_handler);

        let delete_node_route = warp::path!("node" / Uuid)
            .and(warp::delete())
            .and(mgmt_auth.clone())
            .and(with_sync(self.sync.clone()))
            .and_then(delete_node_handler);

        let post_node_register_route = warp::post()
            .and(warp::path("node"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json::<NodeRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(params.pool_strict_inbounds))
            .and_then(post_node_handler);

        let private_mint_install_token_route = warp::post()
            .and(warp::path("private"))
            .and(warp::path("install-token"))
            .and(warp::path::end())
            .and(warp::body::json::<InstallTokenRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(mint_install_token_handler);

        // Public selfhosted account creation: anyone may open an empty
        // account, but creation is per-IP rate-limited against abuse.
        let private_account_rate_limiter =
            Arc::new(RateLimiter::new(5, std::time::Duration::from_secs(600)));
        let private_create_account_route = warp::post()
            .and(warp::path("private"))
            .and(warp::path("account"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(warp::any().map({
                let limiter = private_account_rate_limiter.clone();
                move || limiter.clone()
            }))
            .and_then(create_private_account_handler);

        let private_register_node_route = warp::post()
            .and(warp::path("private"))
            .and(warp::path("nodes"))
            .and(warp::path::end())
            .and(warp::header::optional::<String>("authorization"))
            .and(warp::body::json::<NodeRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and_then(register_private_node_handler);

        let private_list_nodes_route = warp::get()
            .and(warp::path("private"))
            .and(warp::path("nodes"))
            .and(warp::path::end())
            .and(warp::query::<PrivateNodesQuery>())
            .and(with_sync(self.sync.clone()))
            .and_then(list_private_nodes_handler);

        // Live gauges of the account's own nodes (CPU/RAM/disk/network plus
        // inbound totals) — the cabinet's "server" card.
        let private_node_stats_route = warp::get()
            .and(warp::path("private"))
            .and(warp::path("nodes"))
            .and(warp::path("stats"))
            .and(warp::path::end())
            .and(warp::query::<PrivateNodesQuery>())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and_then(private_node_stats_handler);

        // Downsampled 24h/7d history for one own node (per-server chart).
        let private_node_series_route = warp::get()
            .and(warp::path("private"))
            .and(warp::path("nodes"))
            .and(warp::path::param::<Uuid>())
            .and(warp::path("series"))
            .and(warp::path::end())
            .and(warp::query::<PrivateNodeSeriesQuery>())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and_then(private_node_series_handler);

        // Cabinet counters: used/limit/remaining for servers and users.
        let private_state_route = warp::get()
            .and(warp::path("private"))
            .and(warp::path("state"))
            .and(warp::path::end())
            .and(warp::query::<PrivateNodesQuery>())
            .and(with_sync(self.sync.clone()))
            .and_then(private_state_handler);

        let private_feed_route = warp::post()
            .and(warp::path("private"))
            .and(warp::path("nodes"))
            .and(warp::path::param::<Uuid>())
            .and(warp::path("feed"))
            .and(warp::path::end())
            .and(warp::body::json::<PrivateFeedRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(set_private_feed_handler);

        let private_delete_node_route = warp::delete()
            .and(warp::path("private"))
            .and(warp::path("nodes"))
            .and(warp::path::param::<Uuid>())
            .and(warp::path::end())
            .and(warp::query::<PrivateNodesQuery>())
            .and(with_sync(self.sync.clone()))
            .and_then(delete_private_node_handler);

        // Selfhosted "users": child subscriptions carrying the account's
        // nodes (pinned subset or all of them).
        let private_create_user_route = warp::post()
            .and(warp::path("private"))
            .and(warp::path("users"))
            .and(warp::path::end())
            .and(warp::body::json::<PrivateUserCreateRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and_then(create_private_user_handler);

        let private_list_users_route = warp::get()
            .and(warp::path("private"))
            .and(warp::path("users"))
            .and(warp::path::end())
            .and(warp::query::<PrivateNodesQuery>())
            .and(with_sync(self.sync.clone()))
            .and_then(list_private_users_handler);

        let private_delete_user_route = warp::delete()
            .and(warp::path("private"))
            .and(warp::path("users"))
            .and(warp::path::param::<Uuid>())
            .and(warp::path::end())
            .and(warp::query::<PrivateNodesQuery>())
            .and(with_sync(self.sync.clone()))
            .and_then(delete_private_user_handler);

        let private_routes = private_mint_install_token_route
            .or(private_create_account_route)
            .or(private_register_node_route)
            .or(private_list_nodes_route)
            .or(private_node_stats_route)
            .or(private_node_series_route)
            .or(private_state_route)
            .or(private_feed_route)
            .or(private_delete_node_route)
            .or(private_create_user_route)
            .or(private_list_users_route)
            .or(private_delete_user_route);

        let get_clusters_route = warp::get()
            .and(warp::path("clusters"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and_then(get_clusters_handler);

        let get_cluster_nodes_route = warp::get()
            .and(warp::path("cluster"))
            .and(warp::path::param::<String>())
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and_then(get_cluster_nodes_handler);

        let get_subscription_route = warp::get()
            .and(warp::path("sub"))
            .and(warp::path::end())
            .and(warp::query::<SubscriptionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and(with_param_string(params.subscription_title.clone()))
            .and(with_param_string(params.base_url.clone()))
            .and(with_param_string(params.support_contact.clone()))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and(warp::any().map({
                let metered = params.metered_conns.clone();
                move || metered.clone()
            }))
            .and_then(subscription_link_handler);

        let get_catalog_version_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("catalog"))
            .and(warp::path("version"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and_then(get_catalog_version_handler);

        let get_subscription_info_route = warp::get()
            .and(warp::path!("subscription" / Uuid))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and(with_param_string(api_base.clone()))
            .and_then(get_subscription_info_json);

        let get_subscription_by_ref_code_route = warp::get()
            .and(warp::path("subscription"))
            .and(warp::path("by_ref_code"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::query::<crate::http::request::RefCodeQuery>())
            .and(with_sync(self.sync.clone()))
            .and_then(get_subscription_by_ref_code_handler);

        let get_subscription_traffic_route = warp::get()
            .and(warp::path!("subscription" / Uuid / "traffic"))
            .and(warp::path::end())
            .and(warp::query::<
                crate::http::handlers::subscription::TrafficHistoryQuery,
            >())
            .and(with_sync(self.sync.clone()))
            .and_then(get_subscription_traffic_history);

        let post_subscription_traffic_route = warp::post()
            .and(warp::path!("subscription" / Uuid / "traffic"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and_then(post_subscription_traffic_handler);

        // Admin routes
        let admin_enabled = self.settings.service.admin_enabled;
        let admin_token = self
            .settings
            .service
            .admin_token
            .clone()
            .unwrap_or_default();

        let admin_page_route = warp::get()
            .and(warp::path("admin"))
            .and(warp::path::end())
            .and(warp::query::<crate::http::handlers::admin::AdminPageQuery>())
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and_then(admin_page_handler);

        let admin_premium_inventory_route = warp::get()
            .and(warp::path!("admin" / "api" / "premium" / "inventory"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_premium_inventory_handler);

        let admin_api_state_route = warp::get()
            .and(warp::path!("admin" / "api" / "state"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_state_handler);

        let admin_api_nodes_route = warp::get()
            .and(warp::path!("admin" / "api" / "nodes"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and_then(admin_api_nodes_handler);

        let admin_api_subscriptions_route = warp::get()
            .and(warp::path!("admin" / "api" / "subscriptions"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and_then(admin_api_subscriptions_handler);

        let admin_api_subscriptions_count_route = warp::get()
            .and(warp::path!("admin" / "api" / "subscriptions" / "count"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_subscriptions_count_handler);

        let admin_api_subscription_connections_route = warp::get()
            .and(warp::path!(
                "admin" / "api" / "subscriptions" / Uuid / "connections"
            ))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and_then(admin_api_subscription_connections_handler);

        let admin_api_connections_route = warp::get()
            .and(warp::path!("admin" / "api" / "connections"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and_then(admin_api_connections_handler);

        let admin_api_node_metrics_route = warp::get()
            .and(warp::path!("admin" / "api" / "nodes" / Uuid / "metrics"))
            .and(warp::path::end())
            .and(warp::query::<
                crate::http::handlers::admin::AdminNodeMetricsQuery,
            >())
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and_then(admin_api_node_metrics_handler);

        let admin_api_assign_premium_route = warp::post()
            .and(warp::path!(
                "admin" / "api" / "subscriptions" / Uuid / "premium"
            ))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_assign_premium_handler);

        let admin_api_create_subscription_route = warp::post()
            .and(warp::path!("admin" / "api" / "subscriptions"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_create_subscription_handler);

        let admin_api_extend_subscription_route = warp::post()
            .and(warp::path!(
                "admin" / "api" / "subscriptions" / Uuid / "extend"
            ))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_extend_subscription_handler);

        // Shards: declare one, or change its plan/title/kind/caps. The plan
        // cannot be derived from an opaque id, so this is the only place it is
        // set — no hand-written SQL during provisioning.
        let admin_api_shards_get = warp::get()
            .and(warp::path!("admin" / "api" / "shards"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_shards_handler);

        let admin_api_shards_put = warp::put()
            .and(warp::path!("admin" / "api" / "shards" / String))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_put_shard_handler);

        let admin_api_shards_delete = warp::delete()
            .and(warp::path!("admin" / "api" / "shards" / String))
            .and(warp::path::end())
            .and(warp::query::<std::collections::HashMap<String, String>>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |env: String,
                 q: std::collections::HashMap<String, String>,
                 sync: MemSync<N, C, S>,
                 enabled: bool,
                 token: String,
                 auth: Option<String>| async move {
                    let force = q
                        .get("force")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);
                    admin_api_delete_shard_handler(env, force, sync, enabled, token, auth).await
                },
            );

        // Selfhosted caps of one account (servers / users).
        let admin_api_selfhosted_limits_route = warp::put()
            .and(warp::path!(
                "admin" / "api" / "subscriptions" / Uuid / "selfhosted-limits"
            ))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_set_selfhosted_limits_handler);

        // Владелец selfhosted-нод привязывает свой Telegram: алерты по его
        // серверам уходят ему, а не в наш общий чат.
        let admin_api_alert_chat_put = warp::put()
            .and(warp::path!(
                "admin" / "api" / "subscriptions" / Uuid / "alerts" / "telegram"
            ))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_set_alert_chat_handler);

        let admin_api_alert_chat_get = warp::get()
            .and(warp::path!(
                "admin" / "api" / "subscriptions" / Uuid / "alerts" / "telegram"
            ))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_get_alert_chat_handler);

        let admin_api_alert_chat_delete = warp::delete()
            .and(warp::path!(
                "admin" / "api" / "subscriptions" / Uuid / "alerts" / "telegram"
            ))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_delete_alert_chat_handler);

        let admin_api_delete_subscription_route = warp::delete()
            .and(warp::path!("admin" / "api" / "subscriptions" / Uuid))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_delete_subscription_handler);

        let admin_api_delete_connection_route = warp::delete()
            .and(warp::path!("admin" / "api" / "connections" / Uuid))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_delete_connection_handler);

        let score_cfg = self.settings.score.clone();
        let admin_score_route = warp::get()
            .and(warp::path!("v1" / "admin" / "nodes" / "score"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and(warp::any().map(move || score_cfg.clone()))
            .and_then(score_list_handler);

        let admin_score_history_route = warp::get()
            .and(warp::path!("v1" / "admin" / "nodes" / "score" / "history"))
            .and(warp::path::end())
            .and(warp::query::<crate::http::handlers::score::ScoreHistoryQuery>())
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and_then(score_history_handler);

        let admin_routes = admin_page_route
            .or(admin_score_history_route)
            .or(admin_score_route)
            .or(admin_api_state_route)
            .or(admin_premium_inventory_route)
            .or(admin_api_nodes_route)
            .or(admin_api_node_metrics_route)
            .or(admin_api_subscriptions_route)
            .or(admin_api_subscriptions_count_route)
            .or(admin_api_subscription_connections_route)
            .or(admin_api_connections_route)
            .or(admin_api_assign_premium_route)
            .or(admin_api_create_subscription_route)
            .or(admin_api_extend_subscription_route)
            .or(admin_api_shards_get)
            .or(admin_api_shards_put)
            .or(admin_api_shards_delete)
            .or(admin_api_selfhosted_limits_route)
            .or(admin_api_alert_chat_put)
            .or(admin_api_alert_chat_get)
            .or(admin_api_alert_chat_delete)
            .or(admin_api_delete_subscription_route)
            .or(admin_api_delete_connection_route);

        // Premium routes
        let premium_state_route = warp::get()
            .and(warp::path("premium"))
            .and(warp::path("state"))
            .and(warp::path::end())
            .and(with_premium_auth(self.sync.clone()))
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and_then(premium_state_handler);

        let premium_child_list_route = warp::get()
            .and(warp::path("premium"))
            .and(warp::path("child"))
            .and(warp::path::end())
            .and(with_premium_auth(self.sync.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(premium_child_list_handler);

        let premium_create_child_route = warp::post()
            .and(warp::path("premium"))
            .and(warp::path("child"))
            .and(warp::path::end())
            .and(with_premium_auth(self.sync.clone()))
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and_then(premium_create_child_handler);

        let premium_update_child_route = warp::put()
            .and(warp::path("premium"))
            .and(warp::path("child"))
            .and(with_premium_auth(self.sync.clone()))
            .and(warp::path::param::<Uuid>())
            .and(warp::path::end())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and_then(premium_update_child_handler);

        let premium_child_connections_route = warp::get()
            .and(warp::path("premium"))
            .and(warp::path("child"))
            .and(with_premium_auth(self.sync.clone()))
            .and(warp::path::param::<Uuid>())
            .and(warp::path("connections"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and_then(premium_child_connections_handler);

        let premium_create_connection_route = warp::post()
            .and(warp::path("premium"))
            .and(warp::path("child"))
            .and(with_premium_auth(self.sync.clone()))
            .and(warp::path::param::<Uuid>())
            .and(warp::path("connections"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and_then(premium_create_connection_handler);

        let premium_delete_connection_route = warp::delete()
            .and(warp::path("premium"))
            .and(warp::path("connections"))
            .and(with_premium_auth(self.sync.clone()))
            .and(warp::path::param::<Uuid>())
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and_then(premium_delete_connection_handler);

        let premium_child_traffic_route = warp::get()
            .and(warp::path("premium"))
            .and(warp::path("child"))
            .and(with_premium_auth(self.sync.clone()))
            .and(warp::path::param::<Uuid>())
            .and(warp::path("traffic"))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and_then(premium_child_traffic_handler);

        let premium_routes = premium_state_route
            .or(premium_child_list_route)
            .or(premium_create_child_route)
            .or(premium_update_child_route)
            .or(premium_child_connections_route)
            .or(premium_create_connection_route)
            .or(premium_delete_connection_route)
            .or(premium_child_traffic_route);

        let post_subscription_route = warp::post()
            .and(warp::path("subscription"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and_then(post_subscription_handler);

        let put_conn_resolver = params.conn_resolver();

        let put_subscription_route = warp::put()
            .and(warp::path("subscription"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::query::<SubIdQueryParam>())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and(warp::any().map(move || put_conn_resolver.clone()))
            .and_then(put_subscription_handler);

        // Connections Routes
        let get_a_connection_route = warp::path!("connection")
            .and(warp::get())
            .and(mgmt_auth.clone())
            .and(warp::query::<ConnQueryParam>())
            .and(with_sync(self.sync.clone()))
            .and_then(get_connection_handler);

        let get_wg_connections_info_route = warp::path!("info" / "connections" / "wireguard")
            .and(warp::get())
            .and(warp::query::<ConnectionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and_then(wireguard_connections_handler);

        let get_awg_connections_info_route = warp::path!("info" / "connections" / "amneziawg")
            .and(warp::get())
            .and(warp::query::<ConnectionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and_then(amnezia_wireguard_connections_handler);

        let get_mtproto_connections_info_route = warp::path!("info" / "connections" / "mtproto")
            .and(warp::get())
            .and(warp::query::<ConnectionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and_then(mtproto_connections_handler);

        // События watchcat: переключение выхода, «живых нет», сломанный перехват.
        // Авторизация как у node-сигналов: service token (или admin, или
        // node_-токен self-hosted ноды). Уведомления отправляет api, а не демон:
        // токен Telegram на нодах не нужен.
        let alerts_cfg = self.settings.alerts.clone();
        let watchcat_throttle = crate::http::handlers::watchcat::NotifyThrottle::new();
        // POST /v1/node/snapshot/drift — часовая сверка снапшота ноды с api.
        let snapshot_drift_throttle = crate::http::handlers::watchcat::NotifyThrottle::new();
        let post_snapshot_drift_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("node"))
            .and(warp::path!("snapshot" / "drift"))
            .and(warp::path::end())
            .and(node_sync_auth.clone())
            .and(warp::body::content_length_limit(4 * 1024))
            .and(warp::body::json::<
                crate::http::handlers::watchcat::SnapshotDriftReport,
            >())
            .and(with_sync(self.sync.clone()))
            .and(warp::any().map({
                let cfg = alerts_cfg.clone();
                move || cfg.clone()
            }))
            .and(warp::any().map(move || snapshot_drift_throttle.clone()))
            .and_then(crate::http::handlers::watchcat::snapshot_drift_handler);

        let post_watchcat_event_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("node"))
            .and(warp::path!("watchcat" / "event"))
            .and(warp::path::end())
            .and(node_sync_auth.clone())
            .and(warp::body::content_length_limit(16 * 1024))
            .and(warp::body::json::<
                crate::http::handlers::watchcat::WatchcatReport,
            >())
            .and(with_sync(self.sync.clone()))
            .and(warp::any().map({
                let cfg = alerts_cfg.clone();
                move || cfg.clone()
            }))
            .and(with_param_u64(alerts_cfg.watchcat_switch_throttle_sec))
            .and(with_param_u64(alerts_cfg.watchcat_intercept_throttle_sec))
            .and(with_param_u64(alerts_cfg.watchcat_no_healthy_throttle_sec))
            .and(warp::any().map(move || watchcat_throttle.clone()))
            .and_then(crate::http::handlers::watchcat::watchcat_event_handler);

        let post_connections_sync_route = warp::path("connections")
            .and(warp::path("sync"))
            .and(warp::post())
            .and(connections_sync_auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and_then(get_connections_handler);

        // POST /connections/count — how many connections of a tag a node would
        // receive. fnode asks at startup to decide whether its snapshot is
        // still valid; the answer must come from the same predicate as /sync.
        let connections_count_limiter =
            Arc::new(crate::http::handlers::report::SubRateLimiter::new(
                1,
                std::time::Duration::from_secs(10),
            ));
        let with_connections_count_limiter = {
            let limiter = connections_count_limiter.clone();
            warp::any().map(move || limiter.clone())
        };
        let post_connections_count_route = warp::path("connections")
            .and(warp::path("count"))
            .and(warp::post())
            .and(warp::path::end())
            .and(connections_sync_auth.clone())
            .and(warp::body::json::<
                crate::http::handlers::connection::ConnCountRequest,
            >())
            .and(with_sync(self.sync.clone()))
            .and(with_connections_count_limiter)
            .and_then(crate::http::handlers::connection::connections_count_handler);

        let post_connection_route = warp::post()
            .and(warp::path("connection"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and(warp::any().map({
                let metered = params.metered_conns.clone();
                move || metered.clone()
            }))
            .and_then(create_connection_handler);

        let post_connection_feed_route = warp::post()
            .and(warp::path("connection"))
            .and(warp::path::param::<Uuid>())
            .and(warp::path("feed"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json::<ConnectionFeedRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(set_connection_feed_handler);

        let delete_connection_route = warp::delete()
            .and(warp::path("connection"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::query::<ConnQueryParam>())
            .and(with_sync(self.sync.clone()))
            .and_then(delete_connection_handler);

        // Share token management (service token auth — the site's mrkting
        // proxy calls these; the app uses the AGW /v1/share* routes).
        let post_share_mgmt_route = warp::post()
            .and(warp::path("share"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json::<MgmtShareMintRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_string(api_base.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and_then(mgmt_share_mint_handler);

        // Shared subscription (env+protocol, `frkn://sub/shared/<uuid>`):
        // same service-token auth, minted from the cabinet's region picker.
        let post_share_sub_mgmt_route = warp::post()
            .and(warp::path("share"))
            .and(warp::path("sub"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json::<MgmtSharedSubRequest>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_string(api_base.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and_then(mgmt_share_sub_mint_handler);

        let post_share_revoke_mgmt_route = warp::post()
            .and(warp::path("share"))
            .and(warp::path("revoke"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json::<MgmtShareRevokeRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(mgmt_share_revoke_handler);

        // Keys Routes
        let get_key_validation_route = warp::get()
            .and(warp::path("key"))
            .and(warp::path("validate"))
            .and(warp::path::end())
            .and(warp::query::<KeyQueryParams>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_vec(params.key_sign_token.clone()))
            .and_then(get_key_validate_handler);

        let post_key_route = warp::post()
            .and(warp::path("key"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_vec(params.key_sign_token.clone()))
            .and_then(post_key_handler);

        // Public geo lists for the OpenWrt router image (`/router/geo/*`):
        // plain text, no AGW envelope, no auth — the same shape as
        // `/sub/<token>`. The daily `frkn-geo-update` timer (deploy/geo/)
        // replaces the file atomically; `If-None-Match` keeps router polling
        // at a 304 most of the time.
        let geo_routes = geo_handlers::routes(params.geo_dir.clone());

        let activate_conn_resolver = params.conn_resolver();
        let mrkting_config = params.mrkting.clone();

        let post_activate_key_route = warp::post()
            .and(warp::path("key"))
            .and(warp::path("activate"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and(warp::any().map(move || activate_conn_resolver.clone()))
            .and(warp::any().map(move || mrkting_config.clone()))
            .and(warp::any().map(|| crate::http::handlers::key::ActivationFlow::Legacy))
            .and_then(post_activate_key_handler);

        // New flow: same body as /v1/key/activate, but access is granted from
        // shards and grants. The site points here when testing is over; there
        // is no fallback — a payer without a live shard is quarantined.
        let post_activate_peers_route = warp::post()
            .and(warp::path!("v1" / "activate"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and(warp::any().map({
                let resolver = params.conn_resolver();
                move || resolver.clone()
            }))
            .and(warp::any().map({
                let mrk = params.mrkting.clone();
                move || mrk.clone()
            }))
            .and(warp::any().map(|| crate::http::handlers::key::ActivationFlow::Peers))
            .and_then(post_activate_key_handler);

        // Amnezia gateway routes
        let gateway_labels = GatewayLabels {
            price: params
                .gateway_price_label
                .clone()
                .unwrap_or_else(|| "500".to_string()),
            speed: params
                .gateway_speed_label
                .clone()
                .unwrap_or_else(|| "1000".to_string()),
        };
        let with_labels = warp::any().map(move || gateway_labels.clone());

        let post_amnezia_services_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("services"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewayServicesRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(with_labels.clone())
            .and(with_param_bool(params.traffic_mode_enabled))
            .and(warp::any().map({
                let metered = params.metered_conns.clone();
                move || metered.clone()
            }))
            .and_then(
                |req: GatewayServicesRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 labels: GatewayLabels,
                 traffic_mode_enabled: bool,
                 metered_conns: Vec<String>| async move {
                    let response = gateway_services_handler(
                        req,
                        sync,
                        labels,
                        traffic_mode_enabled,
                        metered_conns,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_amnezia_account_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("account_info"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewayAccountInfoRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and_then(
                |req: GatewayAccountInfoRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 traffic_mode_enabled: bool| async move {
                    let response =
                        gateway_account_info_handler(req, sync, traffic_mode_enabled).await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        // Share tokens (/v1/share*, share branch of /v1/config): per-IP
        // rate limit against token enumeration, same params as mrkting.
        let share_rate_limiter = Arc::new(RateLimiter::new(10, std::time::Duration::from_secs(60)));
        let with_share_limiter = {
            let limiter = share_rate_limiter.clone();
            warp::any().map(move || limiter.clone())
        };

        // Block telemetry: 10/мин на IP (отчёты о блокировке адреса).
        let report_rate_limiter =
            Arc::new(RateLimiter::new(10, std::time::Duration::from_secs(60)));
        let with_report_limiter = {
            let limiter = report_rate_limiter.clone();
            warp::any().map(move || limiter.clone())
        };
        // 30 отчётов в час на подписку.
        let report_sub_limiter = Arc::new(crate::http::handlers::report::SubRateLimiter::new(
            30,
            std::time::Duration::from_secs(3600),
        ));
        let with_report_sub_limiter = {
            let limiter = report_sub_limiter.clone();
            warp::any().map(move || limiter.clone())
        };

        // POST /v1/report/block и /v1/report/scan — шифрованный конверт gateway,
        // как у /v1/services: клиент не умеет читать plaintext-ответ.
        let post_block_report_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("report"))
            .and(warp::path("block"))
            .and(warp::path::end())
            .and(warp::body::content_length_limit(
                crate::http::handlers::report::MAX_REPORT_BODY_BYTES,
            ))
            // Тело разбираем как Value: /v1/report/block принимает и старый
            // формат (report_id + report), и новый (mode + targets/attempts),
            // а различает их сам хендлер.
            .and(crypto::with_agw_decryption::<serde_json::Value>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_report_limiter.clone())
            .and(with_report_sub_limiter.clone())
            .and_then(
                |req: serde_json::Value,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 remote: Option<std::net::SocketAddr>,
                 x_forwarded_for: Option<String>,
                 rate_limiter: Arc<RateLimiter>,
                 sub_limiter: Arc<crate::http::handlers::report::SubRateLimiter>| async move {
                    let response = crate::http::handlers::report::gateway_report_handler(
                        req,
                        sync,
                        remote,
                        x_forwarded_for,
                        rate_limiter,
                        sub_limiter,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_scan_report_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("report"))
            .and(warp::path("scan"))
            .and(warp::path::end())
            .and(warp::body::content_length_limit(
                crate::http::handlers::report::MAX_SCAN_BODY_BYTES,
            ))
            // Как и /report/block: тело — Value, потому что новый формат
            // приходит и сюда (report_id в обоих режимах), а различает его
            // диспетчер по вложенному `scan`.
            .and(crypto::with_agw_decryption::<serde_json::Value>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_report_limiter.clone())
            .and(with_report_sub_limiter.clone())
            .and_then(
                |req: serde_json::Value,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 remote: Option<std::net::SocketAddr>,
                 x_forwarded_for: Option<String>,
                 rate_limiter: Arc<RateLimiter>,
                 sub_limiter: Arc<crate::http::handlers::report::SubRateLimiter>| async move {
                    let response = crate::http::handlers::report::gateway_report_handler(
                        req,
                        sync,
                        remote,
                        x_forwarded_for,
                        rate_limiter,
                        sub_limiter,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        // POST /v1/nodes/load — текущая загрузка нод (доля занятости 0..1),
        // шифрованный конверт как у /v1/services. Не чаще раза в 15 с на подписку:
        // клиенту хватает кэша 30 с, а частые опросы бьют по api на каждом подключении.
        let nodes_load_limiter = Arc::new(crate::http::handlers::report::SubRateLimiter::new(
            1,
            std::time::Duration::from_secs(15),
        ));
        let with_nodes_load_limiter = {
            let limiter = nodes_load_limiter.clone();
            warp::any().map(move || limiter.clone())
        };
        let node_load_conn_capacity = params.node_load_conn_capacity;
        let node_load_threshold = params.node_load_threshold;

        let post_nodes_load_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("nodes"))
            .and(warp::path("load"))
            .and(warp::path::end())
            .and(warp::body::content_length_limit(8 * 1024))
            .and(crypto::with_agw_decryption::<
                crate::http::handlers::amnezia::GatewayNodesLoadRequest,
            >(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and(with_nodes_load_limiter)
            .and_then(
                move |req: crate::http::handlers::amnezia::GatewayNodesLoadRequest,
                      ctx: Option<AesContext>,
                      sync: MemSync<N, C, S>,
                      metrics: Arc<MetricStorage>,
                      limiter: Arc<crate::http::handlers::report::SubRateLimiter>| async move {
                    let response = crate::http::handlers::amnezia::gateway_nodes_load_handler(
                        req,
                        sync,
                        metrics,
                        limiter,
                        node_load_conn_capacity,
                        node_load_threshold,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        // Админ-ручки оповещений: история и тест канала.
        let get_admin_alerts_route = warp::get()
            .and(warp::path!("v1" / "admin" / "alerts"))
            .and(warp::path::end())
            .and(warp::query::<std::collections::HashMap<String, String>>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |q: std::collections::HashMap<String, String>,
                 sync: MemSync<N, C, S>,
                 admin_enabled: bool,
                 admin_token: String,
                 auth_header: Option<String>| async move {
                    let limit = q.get("limit").and_then(|v| v.parse::<i64>().ok());
                    crate::http::handlers::alerts::admin_alerts_handler(
                        limit,
                        sync,
                        admin_enabled,
                        admin_token,
                        auth_header,
                    )
                    .await
                },
            );

        let alerts_cfg = self.settings.alerts.clone();
        let test_alerts_cfg = alerts_cfg.clone();
        let post_admin_alerts_test_route = warp::post()
            .and(warp::path!("v1" / "admin" / "alerts" / "test"))
            .and(warp::path::end())
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(warp::any().map(move || test_alerts_cfg.clone()))
            .and_then(crate::http::handlers::alerts::admin_alerts_test_handler);

        let send_alerts_cfg = alerts_cfg.clone();
        let post_admin_alerts_send_route = warp::post()
            .and(warp::path!("v1" / "admin" / "alerts" / "send"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::any().map(move || send_alerts_cfg.clone()))
            .and(warp::body::json::<
                crate::http::handlers::alerts::SendAlertRequest,
            >())
            .and_then(crate::http::handlers::alerts::admin_alerts_send_handler);

        // Digest data for mrkting's daily/weekly/monthly email: the api owns
        // the data, mrkting owns the schedule, the template and the SMTP.
        let admin_digest_route = warp::get()
            .and(warp::path!("v1" / "admin" / "digest"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::query::<crate::http::handlers::alerts::DigestQuery>())
            .and(with_sync(self.sync.clone()))
            .and_then(crate::http::handlers::alerts::admin_digest_handler);

        // GET /v1/admin/report/reports — отчёты клиента: кто прислал и что не так.
        let get_client_reports_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("admin"))
            .and(warp::path("report"))
            .and(warp::path("reports"))
            .and(warp::path::end())
            .and(warp::query::<
                crate::http::handlers::report::ClientReportsQuery,
            >())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |query: crate::http::handlers::report::ClientReportsQuery,
                 sync: MemSync<N, C, S>,
                 admin_enabled: bool,
                 admin_token: String,
                 auth_header: Option<String>| async move {
                    crate::http::handlers::report::admin_client_reports_handler(
                        query,
                        sync,
                        admin_enabled,
                        admin_token,
                        auth_header,
                    )
                    .await
                },
            );

        // GET /v1/admin/report/connect-stats — события подключения по подписке.
        let get_connect_stats_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("admin"))
            .and(warp::path("report"))
            .and(warp::path("connect-stats"))
            .and(warp::path::end())
            .and(warp::query::<
                crate::http::handlers::report::ConnectStatsQuery,
            >())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |query: crate::http::handlers::report::ConnectStatsQuery,
                 sync: MemSync<N, C, S>,
                 admin_enabled: bool,
                 admin_token: String,
                 auth_header: Option<String>| async move {
                    crate::http::handlers::report::admin_connect_stats_handler(
                        query,
                        sync,
                        admin_enabled,
                        admin_token,
                        auth_header,
                    )
                    .await
                },
            );

        // GET /v1/admin/report/observations — сырые отчёты для поддержки.
        let get_report_observations_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("admin"))
            .and(warp::path("report"))
            .and(warp::path("observations"))
            .and(warp::path::end())
            .and(warp::query::<
                crate::http::handlers::report::ObservationsQuery,
            >())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |query: crate::http::handlers::report::ObservationsQuery,
                 sync: MemSync<N, C, S>,
                 admin_enabled: bool,
                 admin_token: String,
                 auth_header: Option<String>| async move {
                    crate::http::handlers::report::admin_observations_handler(
                        query,
                        sync,
                        admin_enabled,
                        admin_token,
                        auth_header,
                    )
                    .await
                },
            );

        // GET /v1/admin/usage/exit — кто через какой выход ходил.
        let get_exit_usage_admin_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("admin"))
            .and(warp::path("usage"))
            .and(warp::path("exit"))
            .and(warp::path::end())
            .and(warp::query::<crate::http::handlers::usage::UsageQuery>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |query: crate::http::handlers::usage::UsageQuery,
                 sync: MemSync<N, C, S>,
                 admin_enabled: bool,
                 admin_token: String,
                 auth_header: Option<String>| async move {
                    crate::http::handlers::usage::admin_exit_usage_handler(
                        query,
                        sync,
                        admin_enabled,
                        admin_token,
                        auth_header,
                    )
                    .await
                },
            );

        // GET /v1/admin/usage/entry — откуда подключались.
        let get_entry_usage_admin_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("admin"))
            .and(warp::path("usage"))
            .and(warp::path("entry"))
            .and(warp::path::end())
            .and(warp::query::<crate::http::handlers::usage::UsageQuery>())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |query: crate::http::handlers::usage::UsageQuery,
                 sync: MemSync<N, C, S>,
                 admin_enabled: bool,
                 admin_token: String,
                 auth_header: Option<String>| async move {
                    crate::http::handlers::usage::admin_entry_usage_handler(
                        query,
                        sync,
                        admin_enabled,
                        admin_token,
                        auth_header,
                    )
                    .await
                },
            );

        // GET /v1/admin/report/block — агрегат с вердиктами (под admin-токеном).
        let get_block_report_admin_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("admin"))
            .and(warp::path("report"))
            .and(warp::path("block"))
            .and(warp::path::end())
            .and(warp::query::<
                crate::http::handlers::report::BlockReportAggregateQuery,
            >())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(
                |query: crate::http::handlers::report::BlockReportAggregateQuery,
                 sync: MemSync<N, C, S>,
                 admin_enabled: bool,
                 admin_token: String,
                 auth_header: Option<String>| async move {
                    crate::http::handlers::report::admin_block_report_handler(
                        query,
                        sync,
                        admin_enabled,
                        admin_token,
                        auth_header,
                    )
                    .await
                },
            );
        // The public per-share feed (GET /sub/<token>) gets polled by
        // clients — a more generous budget than mint.
        let share_feed_rate_limiter =
            Arc::new(RateLimiter::new(30, std::time::Duration::from_secs(60)));

        let post_amnezia_config_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("config"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewayConfigRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(params.traffic_mode_enabled))
            .and(warp::any().map({
                let metered = params.metered_conns.clone();
                move || metered.clone()
            }))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_share_limiter.clone())
            .and_then(
                |req: GatewayConfigRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 traffic_mode_enabled: bool,
                 metered_conns: Vec<String>,
                 remote: Option<std::net::SocketAddr>,
                 x_forwarded_for: Option<String>,
                 rate_limiter: Arc<RateLimiter>| async move {
                    let response = gateway_config_handler(
                        req,
                        sync,
                        traffic_mode_enabled,
                        metered_conns,
                        remote,
                        x_forwarded_for,
                        rate_limiter,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_share_mint_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("share"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<ShareMintRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_share_limiter.clone())
            .and(with_param_string(api_base.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and_then(
                |req: ShareMintRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 remote: Option<std::net::SocketAddr>,
                 x_forwarded_for: Option<String>,
                 rate_limiter: Arc<RateLimiter>,
                 base_url: String,
                 wg_network: fcore::IpAddrMask,
                 awg_network: fcore::IpAddrMask,
                 awg_mobile_network: Option<fcore::IpAddrMask>| async move {
                    let response = gateway_share_mint_handler(
                        req,
                        sync,
                        remote,
                        x_forwarded_for,
                        rate_limiter,
                        base_url,
                        wg_network,
                        awg_network,
                        awg_mobile_network,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_shares_list_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("shares"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<ShareListRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_share_limiter.clone())
            .and_then(
                |req: ShareListRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 remote: Option<std::net::SocketAddr>,
                 x_forwarded_for: Option<String>,
                 rate_limiter: Arc<RateLimiter>| async move {
                    let response = gateway_shares_list_handler(
                        req,
                        sync,
                        remote,
                        x_forwarded_for,
                        rate_limiter,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_share_revoke_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("share"))
            .and(warp::path("revoke"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<ShareRevokeRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_share_limiter.clone())
            .and_then(
                |req: ShareRevokeRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 remote: Option<std::net::SocketAddr>,
                 x_forwarded_for: Option<String>,
                 rate_limiter: Arc<RateLimiter>| async move {
                    let response = gateway_share_revoke_handler(
                        req,
                        sync,
                        remote,
                        x_forwarded_for,
                        rate_limiter,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        // Public per-share feed for third-party clients (Happ/Streisand/
        // Clash). The token in the path is the whole credential; plain HTTP
        // like the owner's /sub route. The existing query route keeps
        // matching path::end() right after "sub", this one takes the param.
        let get_share_feed_route = warp::get()
            .and(warp::path("sub"))
            .and(warp::path::param::<String>())
            .and(warp::path::end())
            .and(warp::query::<ShareFeedQuery>())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and(with_param_string(params.subscription_title.clone()))
            .and(with_param_string(api_base.clone()))
            .and(with_param_string(params.support_contact.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(warp::any().map(move || share_feed_rate_limiter.clone()))
            .and_then(share_feed_handler);

        // Peers: the client owns the key pair, the api hands out the address.
        // Mounted even when disabled — the handlers answer 404 in that case,
        // so a half-configured deployment is visible instead of silent.
        let peers_cfg = self.settings.service.peers.clone();
        let peers_enabled = peers_cfg.enabled;
        let peers_remember_days = peers_cfg.remember_days as u64;
        let peers_per_device = peers_cfg.peers_per_device as u64;
        let peers_per_subscription = peers_cfg.max_per_subscription as u64;
        let peers_devices_per_subscription = peers_cfg.max_devices_per_subscription as u64;
        let peers_auto_prefix = peers_cfg.auto_subnet_prefix as u64;
        let peers_auto_pool: Option<fcore::IpAddrMask> = match peers_cfg
            .auto_subnet_pool
            .as_deref()
        {
            Some(raw) => match raw.parse::<fcore::IpAddrMask>() {
                Ok(pool) => Some(pool),
                Err(_) => {
                    tracing::error!(
                        "peers: auto_subnet_pool {:?} is not a CIDR, auto allocation stays off",
                        raw
                    );
                    None
                }
            },
            None => None,
        };
        // A non-Copy optional value rides as a filter of its own.
        let with_peers_auto_pool = warp::any().map(move || peers_auto_pool.clone());

        // Кабинет: на какой схеме подписка (legacy/peers/mixed) и что рисовать.
        // Плоский GET, как остальные ручки ЛК — конверта в браузере нет.
        let cabinet_route = warp::get()
            .and(warp::path!("cabinet" / Uuid))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |subscription_id: Uuid, enabled: bool, sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return Ok::<Box<dyn warp::Reply>, warp::Rejection>(
                            peers_handlers::not_enabled(),
                        );
                    }
                    Ok(peers_handlers::get_cabinet_handler(subscription_id, sync).await)
                },
            );

        // Client peer routes speak the AGW envelope only: every one of them is a
        // POST with an encrypted body, so `subscription_id` never lands in a
        // query string, an access log or a crash report. Ids that used to sit
        // in the path ride inside the body for the same reason.
        let post_peers_route = warp::post()
            .and(warp::path!("v1" / "peers"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(crypto::with_agw_decryption::<
                peers_handlers::PeerCreateRequest,
            >(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and(with_param_u64(peers_per_device))
            .and(with_param_u64(peers_per_subscription))
            .and(with_param_u64(peers_devices_per_subscription))
            .and(with_peers_auto_pool)
            .and(with_param_u64(peers_auto_prefix))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::PeerCreateRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 per_device: u64,
                 per_subscription: u64,
                 devices_per_subscription: u64,
                 auto_pool: Option<fcore::IpAddrMask>,
                 auto_prefix: u64| async move {
                    if !enabled {
                        return crypto::encrypt_gateway_reply(
                            peers_handlers::not_enabled().into_response(),
                            ctx,
                        )
                        .await;
                    }
                    let limits = crate::peers::Limits {
                        per_device: per_device as i64,
                        per_subscription: per_subscription as i64,
                        devices_per_subscription: devices_per_subscription as i64,
                    };
                    let reply = peers_handlers::post_peers_handler(
                        req,
                        sync,
                        limits,
                        auto_pool,
                        auto_prefix as u8,
                    )
                    .await;
                    crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
                },
            );

        let post_peers_list_route = warp::post()
            .and(warp::path!("v1" / "peers" / "list"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(crypto::with_agw_decryption::<peers_handlers::PeerListRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(with_param_u64(peers_remember_days))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::PeerListRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 remember_days: u64| async move {
                    if !enabled {
                        return crypto::encrypt_gateway_reply(
                            peers_handlers::not_enabled().into_response(),
                            ctx,
                        )
                        .await;
                    }
                    let reply =
                        peers_handlers::get_peers_handler(req, sync, remember_days as i64).await;
                    crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
                },
            );

        let post_peers_delete_route = warp::post()
            .and(warp::path!("v1" / "peers" / "delete"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(crypto::with_agw_decryption::<
                peers_handlers::PeerDeleteRequest,
            >(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::PeerDeleteRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return crypto::encrypt_gateway_reply(
                            peers_handlers::not_enabled().into_response(),
                            ctx,
                        )
                        .await;
                    }
                    let reply = peers_handlers::delete_peer_handler(req, sync).await;
                    crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
                },
            );

        let post_devices_list_route = warp::post()
            .and(warp::path!("v1" / "devices" / "list"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(crypto::with_agw_decryption::<
                peers_handlers::DeviceListRequest,
            >(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::DeviceListRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return crypto::encrypt_gateway_reply(
                            peers_handlers::not_enabled().into_response(),
                            ctx,
                        )
                        .await;
                    }
                    let reply = peers_handlers::get_devices_handler(req, sync).await;
                    crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
                },
            );

        let post_devices_update_route = warp::post()
            .and(warp::path!("v1" / "devices" / "update"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(crypto::with_agw_decryption::<
                peers_handlers::DeviceUpdateRequest,
            >(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::DeviceUpdateRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return crypto::encrypt_gateway_reply(
                            peers_handlers::not_enabled().into_response(),
                            ctx,
                        )
                        .await;
                    }
                    let reply = peers_handlers::patch_device_handler(req, sync).await;
                    crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
                },
            );

        let post_devices_delete_route = warp::post()
            .and(warp::path!("v1" / "devices" / "delete"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(crypto::with_agw_decryption::<
                peers_handlers::DeviceDeleteRequest,
            >(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::DeviceDeleteRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return crypto::encrypt_gateway_reply(
                            peers_handlers::not_enabled().into_response(),
                            ctx,
                        )
                        .await;
                    }
                    let reply = peers_handlers::delete_device_handler(req, sync).await;
                    crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
                },
            );

        let post_locations_route = warp::post()
            .and(warp::path!("v1" / "locations"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(crypto::with_agw_decryption::<peers_handlers::LocationsRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::LocationsRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return crypto::encrypt_gateway_reply(
                            peers_handlers::not_enabled().into_response(),
                            ctx,
                        )
                        .await;
                    }
                    let reply = peers_handlers::get_locations_handler(req, sync).await;
                    crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
                },
            );

        let get_access_route = warp::get()
            .and(warp::path!("v1" / "access"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(warp::query::<std::collections::HashMap<String, String>>())
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 q: std::collections::HashMap<String, String>,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return Ok::<Box<dyn warp::Reply>, warp::Rejection>(
                            peers_handlers::not_enabled(),
                        );
                    }
                    let Some(subscription_id) = q
                        .get("subscription_id")
                        .and_then(|v| uuid::Uuid::from_str(v).ok())
                    else {
                        return Ok(peers_handlers::not_enabled());
                    };
                    Ok(peers_handlers::get_access_handler(subscription_id, sync).await)
                },
            );

        let post_access_route = warp::post()
            .and(warp::path!("v1" / "access"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(mgmt_auth.clone())
            .and(warp::body::json::<peers_handlers::AccessUpsertRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::AccessUpsertRequest,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return Ok::<Box<dyn warp::Reply>, warp::Rejection>(
                            peers_handlers::not_enabled(),
                        );
                    }
                    Ok(peers_handlers::post_access_handler(req, sync).await)
                },
            );

        let get_capacity_route = warp::get()
            .and(warp::path("v1"))
            .and(warp::path("access"))
            .and(warp::path("capacity"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(mgmt_auth.clone())
            .and(warp::query::<std::collections::HashMap<String, String>>())
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 q: std::collections::HashMap<String, String>,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return Ok::<Box<dyn warp::Reply>, warp::Rejection>(
                            peers_handlers::not_enabled(),
                        );
                    }
                    let Some(plan) = q.get("plan").cloned() else {
                        return Ok(peers_handlers::not_enabled());
                    };
                    Ok(peers_handlers::get_capacity_handler(plan, sync).await)
                },
            );

        let delete_access_route = warp::delete()
            .and(warp::path!("v1" / "access"))
            .and(warp::path::end())
            .and(with_param_bool(peers_enabled))
            .and(mgmt_auth.clone())
            .and(warp::body::json::<peers_handlers::AccessRevokeRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(
                |enabled: bool,
                 req: peers_handlers::AccessRevokeRequest,
                 sync: MemSync<N, C, S>| async move {
                    if !enabled {
                        return Ok::<Box<dyn warp::Reply>, warp::Rejection>(
                            peers_handlers::not_enabled(),
                        );
                    }
                    Ok(peers_handlers::delete_access_handler(req, sync).await)
                },
            );

        let get_support_route =
            warp::get()
                .and(warp::path("v1"))
                .and(warp::path("admin"))
                .and(warp::path("support"))
                .and(warp::path::param::<String>())
                .and(warp::path::end())
                .and(with_param_bool(peers_enabled))
                .and(mgmt_auth.clone())
                .and(with_sync(self.sync.clone()))
                .and(with_param_u64(peers_remember_days))
                .and_then(
                    |support_tag: String,
                     enabled: bool,
                     sync: MemSync<N, C, S>,
                     remember_days: u64| async move {
                        if !enabled {
                            return Ok::<Box<dyn warp::Reply>, warp::Rejection>(
                                peers_handlers::not_enabled(),
                            );
                        }
                        Ok(peers_handlers::get_support_handler(
                            support_tag,
                            sync,
                            remember_days as i64,
                        )
                        .await)
                    },
                );

        let peers_routes = post_peers_route
            .or(post_peers_list_route)
            .or(post_peers_delete_route)
            .or(post_devices_list_route)
            .or(post_devices_update_route)
            .or(post_devices_delete_route)
            .or(post_locations_route)
            .or(get_access_route)
            .or(get_capacity_route)
            .or(cabinet_route)
            .or(post_access_route)
            .or(delete_access_route)
            .or(get_support_route);

        // App Store IAP: the client is optional — without [service.apple] the
        // route stays mounted and answers 503.
        let apple_iap = params
            .apple
            .as_ref()
            .and_then(|cfg| match AppleIapClient::new(cfg) {
                Ok(client) => {
                    tracing::info!(
                        "Apple IAP client initialized ({} environment)",
                        cfg.environment
                    );
                    Some(Arc::new(client))
                }
                Err(err) => {
                    tracing::error!(
                        "Apple IAP client init failed: {err}. /v1/subscriptions will answer 503"
                    );
                    None
                }
            });

        let iap_wg_network = params.wireguard_network.clone();
        let iap_awg_network = params.amnezia_wireguard_network.clone();
        let iap_awg_mobile_network = params.amnezia_wireguard_mobile_network.clone();
        let iap_enabled_conns = params.enabled_conns.clone();
        let iap_mrkting = params.mrkting.clone();

        let post_amnezia_subscriptions_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("subscriptions"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewaySubscriptionsRequest>(
                agw_key.clone(),
            ))
            .and(with_sync(self.sync.clone()))
            .and(warp::any().map(move || apple_iap.clone()))
            .and(warp::any().map(move || iap_wg_network.clone()))
            .and(warp::any().map(move || iap_awg_network.clone()))
            .and(warp::any().map(move || iap_awg_mobile_network.clone()))
            .and(warp::any().map(move || iap_enabled_conns.clone()))
            .and(warp::any().map(move || iap_mrkting.clone()))
            .and_then(
                |req: GatewaySubscriptionsRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 iap: Option<Arc<AppleIapClient>>,
                 wg_network: fcore::IpAddrMask,
                 awg_network: fcore::IpAddrMask,
                 awg_mobile_network: Option<fcore::IpAddrMask>,
                 enabled_conns: Option<std::collections::HashMap<fcore::Env, Vec<fcore::Tag>>>,
                 mrkting: Option<crate::config::MrktingConfig>| async move {
                    let response = gateway_subscriptions_handler(
                        req,
                        sync,
                        iap,
                        wg_network,
                        awg_network,
                        awg_mobile_network,
                        enabled_conns,
                        mrkting,
                    )
                    .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        //Metrics

        let ws_route = warp::path("ws")
            .and(warp::path("metrics"))
            .and(warp::ws())
            .and(warp::query::<WsMetricQuery>())
            .and(with_metrics(self.metrics.clone()))
            .map(|ws: warp::ws::Ws, query: WsMetricQuery, storage| {
                ws.on_upgrade(move |socket| async move {
                    metrics_ws_handler(socket, query, storage).await;
                })
            });

        let routes = get_healthcheck_route
            // Subscription
            .or(get_subscription_route)
            .or(get_catalog_version_route)
            .or(get_share_feed_route)
            .or(get_subscription_info_route)
            .or(get_subscription_by_ref_code_route)
            .or(get_subscription_traffic_route)
            .or(post_subscription_route)
            .or(put_subscription_route)
            .or(post_subscription_traffic_route)
            // Node
            .or(get_nodes_route)
            .or(get_status_route)
            .or(get_node_route)
            .or(delete_node_route)
            .or(post_node_register_route)
            .or(private_routes)
            // Cluster
            .or(get_clusters_route)
            .or(get_cluster_nodes_route)
            // Connection
            .or(post_connection_route)
            .or(post_connection_feed_route)
            .or(post_connections_sync_route)
            .or(post_connections_count_route)
            .or(post_snapshot_drift_route)
            .or(post_watchcat_event_route)
            .or(delete_connection_route)
            .or(get_mtproto_connections_info_route)
            .or(get_wg_connections_info_route)
            .or(get_awg_connections_info_route)
            .or(get_a_connection_route)
            // Share (mgmt)
            .or(post_share_sub_mgmt_route)
            .or(post_share_mgmt_route)
            .or(post_share_revoke_mgmt_route)
            // Key
            .or(get_key_validation_route)
            .or(post_key_route)
            .or(post_activate_key_route)
            .or(post_activate_peers_route)
            // Public geo lists (OpenWrt /router/geo/*)
            .or(geo_routes)
            // Amnezia
            .or(post_amnezia_services_route)
            .or(post_amnezia_account_route)
            .or(post_amnezia_config_route)
            .or(post_amnezia_subscriptions_route)
            // Share tokens
            .or(post_share_mint_route)
            .or(post_shares_list_route)
            .or(post_share_revoke_route)
            // Block telemetry
            .or(post_block_report_route)
            .or(post_scan_report_route)
            .or(post_nodes_load_route)
            .or(get_block_report_admin_route)
            .or(get_client_reports_route)
            .or(get_report_observations_route)
            .or(get_connect_stats_route)
            .or(get_exit_usage_admin_route)
            .or(get_entry_usage_admin_route)
            .or(get_admin_alerts_route)
            .or(post_admin_alerts_test_route)
            .or(post_admin_alerts_send_route)
            .or(admin_digest_route)
            // Peers
            .or(peers_routes)
            // Admin
            .or(admin_routes)
            // Premium
            .or(premium_routes)
            // Metrics
            .or(ws_route)
            .recover(rejection)
            .with(cors);

        warp::serve(routes)
            .run((self.settings.service.listen, self.settings.service.port))
            .await;

        Ok(())
    }
}
