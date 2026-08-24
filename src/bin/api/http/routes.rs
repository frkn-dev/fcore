use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;
use warp::Filter;

use fcore::{
    http::filters::{with_param_bool, with_param_string},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations, Result,
    Subscription, SubscriptionOperations,
};

use super::{
    crypto::{self, AesContext},
    super::{iap::AppleIapClient, service::Service, sync::MemSync},
    filters::*,
    handlers::{
        admin::*, amnezia::*, cluster::*, connection::*, healthcheck_handler, iap::*, key::*,
        metrics::*, node::*, premium::*, share::*, subscription::*,
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
        let admin_token = self.settings.service.admin_token.clone().unwrap_or_default();
        let mgmt_auth = with_service_or_admin_auth(
            Arc::new(self.settings.service.token.clone()),
            admin_token,
        );

        let params = &self.settings.service;
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
            .and(warp::query::<NodesQueryParams>())
            .and(with_sync(self.sync.clone()))
            .and_then(get_nodes_handler);

        let get_node_route = warp::path!("node" / Uuid)
            .and(warp::get())
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
            .and_then(post_node_handler);

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
            .and_then(subscription_link_handler);

        let get_subscription_info_route = warp::get()
            .and(warp::path!("subscription" / Uuid))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and(with_param_string(params.base_url.clone()))
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
            .and(warp::path!("admin" / "api" / "subscriptions" / Uuid / "connections"))
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
            .and(warp::query::<crate::http::handlers::admin::AdminNodeMetricsQuery>())
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and(with_metrics(self.metrics.clone()))
            .and_then(admin_api_node_metrics_handler);

        let admin_api_assign_premium_route = warp::post()
            .and(warp::path!("admin" / "api" / "subscriptions" / Uuid / "premium"))
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
            .and(warp::path!("admin" / "api" / "subscriptions" / Uuid / "extend"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_bool(admin_enabled))
            .and(with_param_string(admin_token.clone()))
            .and(warp::header::optional::<String>("authorization"))
            .and_then(admin_api_extend_subscription_handler);

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

        let admin_routes = admin_page_route
            .or(admin_api_state_route)
            .or(admin_api_nodes_route)
            .or(admin_api_node_metrics_route)
            .or(admin_api_subscriptions_route)
            .or(admin_api_subscriptions_count_route)
            .or(admin_api_subscription_connections_route)
            .or(admin_api_connections_route)
            .or(admin_api_assign_premium_route)
            .or(admin_api_create_subscription_route)
            .or(admin_api_extend_subscription_route)
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
            .and(with_param_ipaddrmask(params.amnezia_wireguard_network.clone()))
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

        let put_enabled_conns = params.enabled_conns.clone();

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
            .and(warp::any().map(move || put_enabled_conns.clone()))
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
            .and_then(wireguard_connections_handler);

        let get_awg_connections_info_route = warp::path!("info" / "connections" / "amneziawg")
            .and(warp::get())
            .and(warp::query::<ConnectionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(amnezia_wireguard_connections_handler);

        let get_mtproto_connections_info_route = warp::path!("info" / "connections" / "mtproto")
            .and(warp::get())
            .and(warp::query::<ConnectionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(mtproto_connections_handler);

        let post_connections_sync_route = warp::path("connections")
            .and(warp::path("sync"))
            .and(warp::post())
            .and(mgmt_auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and_then(get_connections_handler);

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
            .and_then(create_connection_handler);

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
            .and(with_param_string(params.base_url.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and_then(mgmt_share_mint_handler);

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

        let enabled_conns = params.enabled_conns.clone();
        let mrkting_config = params.mrkting.clone();

        let post_activate_key_route = warp::post()
            .and(warp::path("key"))
            .and(warp::path("activate"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(params.amnezia_wireguard_network.clone()))
            .and(warp::any().map({
                let net = params.amnezia_wireguard_mobile_network.clone();
                move || net.clone()
            }))
            .and(warp::any().map(move || enabled_conns.clone()))
            .and(warp::any().map(move || mrkting_config.clone()))
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
            .and(crypto::with_agw_decryption::<GatewayServicesRequest>(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and(with_labels.clone())
            .and_then(
                |req: GatewayServicesRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 labels: GatewayLabels|
                 async move {
                    let response = gateway_services_handler(req, sync, labels).await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_amnezia_account_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("account_info"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewayAccountInfoRequest>(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |req: GatewayAccountInfoRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>|
                 async move {
                    let response = gateway_account_info_handler(req, sync).await?;
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
        // The public per-share feed (GET /sub/<token>) gets polled by
        // clients — a more generous budget than mint.
        let share_feed_rate_limiter =
            Arc::new(RateLimiter::new(30, std::time::Duration::from_secs(60)));

        let post_amnezia_config_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("config"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewayConfigRequest>(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_share_limiter.clone())
            .and_then(
                |req: GatewayConfigRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>,
                 remote: Option<std::net::SocketAddr>,
                 x_forwarded_for: Option<String>,
                 rate_limiter: Arc<RateLimiter>| async move {
                    let response =
                        gateway_config_handler(req, sync, remote, x_forwarded_for, rate_limiter)
                            .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_share_mint_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("share"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<ShareMintRequest>(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_share_limiter.clone())
            .and(with_param_string(params.base_url.clone()))
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
            .and(crypto::with_agw_decryption::<ShareListRequest>(agw_key.clone()))
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
                    let response =
                        gateway_shares_list_handler(req, sync, remote, x_forwarded_for, rate_limiter)
                            .await?;
                    crypto::encrypt_gateway_reply(response, ctx).await
                },
            );

        let post_share_revoke_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("share"))
            .and(warp::path("revoke"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<ShareRevokeRequest>(agw_key.clone()))
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
            .and(with_param_string(params.base_url.clone()))
            .and(with_param_string(params.support_contact.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(warp::any().map(move || share_feed_rate_limiter.clone()))
            .and_then(share_feed_handler);

        // App Store IAP: the client is optional — without [service.apple] the
        // route stays mounted and answers 503.
        let apple_iap = params.apple.as_ref().and_then(|cfg| {
            match AppleIapClient::new(cfg) {
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
            .or(get_share_feed_route)
            .or(get_subscription_info_route)
            .or(get_subscription_by_ref_code_route)
            .or(get_subscription_traffic_route)
            .or(post_subscription_route)
            .or(put_subscription_route)
            // Node
            .or(get_nodes_route)
            .or(get_node_route)
            .or(delete_node_route)
            .or(post_node_register_route)
            // Cluster
            .or(get_clusters_route)
            .or(get_cluster_nodes_route)
            // Connection
            .or(post_connection_route)
            .or(post_connections_sync_route)
            .or(delete_connection_route)
            .or(get_mtproto_connections_info_route)
            .or(get_wg_connections_info_route)
            .or(get_awg_connections_info_route)
            .or(get_a_connection_route)
            // Share (mgmt)
            .or(post_share_mgmt_route)
            .or(post_share_revoke_mgmt_route)
            // Key
            .or(get_key_validation_route)
            .or(post_key_route)
            .or(post_activate_key_route)
            // Amnezia
            .or(post_amnezia_services_route)
            .or(post_amnezia_account_route)
            .or(post_amnezia_config_route)
            .or(post_amnezia_subscriptions_route)
            // Share tokens
            .or(post_share_mint_route)
            .or(post_shares_list_route)
            .or(post_share_revoke_route)
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
