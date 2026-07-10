use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;
use warp::Filter;

use fcore::{
    http::filters::{with_i64, with_param_bool, with_param_string},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations, Result,
    Subscription, SubscriptionOperations,
};

use super::{
    crypto::{self, AesContext},
    super::{service::Service, sync::MemSync},
    filters::*,
    handlers::{
        admin::*, amnezia::*, cluster::*, connection::*, healthcheck_handler, key::*, metrics::*,
        node::*, premium::*, subscription::*, trial::*,
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
            .and_then(get_subscription_info_json);

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

        let admin_routes = admin_page_route
            .or(admin_api_state_route)
            .or(admin_api_nodes_route)
            .or(admin_api_node_metrics_route)
            .or(admin_api_subscriptions_route)
            .or(admin_api_subscription_connections_route)
            .or(admin_api_connections_route)
            .or(admin_api_assign_premium_route);

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
            .and(with_param_vec_string(params.system_refer_codes.clone()))
            .and_then(post_subscription_handler);

        let put_subscription_route = warp::put()
            .and(warp::path("subscription"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::query::<SubIdQueryParam>())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
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
            .and_then(create_connection_handler);

        let delete_connection_route = warp::delete()
            .and(warp::path("connection"))
            .and(warp::path::end())
            .and(mgmt_auth.clone())
            .and(warp::query::<ConnQueryParam>())
            .and(with_sync(self.sync.clone()))
            .and_then(delete_connection_handler);

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

        let post_activate_key_route = warp::post()
            .and(warp::path("key"))
            .and(warp::path("activate"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and_then(post_activate_key_handler);

        //Trial
        let post_trial_route = warp::post()
            .and(warp::path("trial"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(warp::header::optional::<String>("x-trace-id"))
            .and(with_sync(self.sync.clone()))
            .and(with_email_store(self.email_store.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and(with_param_ipaddrmask(
                params.amnezia_wireguard_network.clone(),
            ))
            .and(with_param_vec_string(params.system_refer_codes.clone()))
            .and(with_param_envs(params.enabled_envs.clone()))
            .and(with_param_tags(params.enabled_tags.clone()))
            .and(with_i64(params.trial_limit_days))
            .and(with_i64(params.trial_limit_bytes))
            .and_then(post_trial_handler);

        // Amnezia gateway routes
        let post_amnezia_services_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("services"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewayServicesRequest>(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |req: GatewayServicesRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>|
                 async move {
                    let response = gateway_services_handler(req, sync).await?;
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

        let post_amnezia_config_route = warp::post()
            .and(warp::path("v1"))
            .and(warp::path("config"))
            .and(warp::path::end())
            .and(crypto::with_agw_decryption::<GatewayConfigRequest>(agw_key.clone()))
            .and(with_sync(self.sync.clone()))
            .and_then(
                |req: GatewayConfigRequest,
                 ctx: Option<AesContext>,
                 sync: MemSync<N, C, S>|
                 async move {
                    let response = gateway_config_handler(req, sync).await?;
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
            .or(get_subscription_info_route)
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
            // Key
            .or(get_key_validation_route)
            .or(post_key_route)
            .or(post_activate_key_route)
            //Trial
            .or(post_trial_route)
            // Amnezia
            .or(post_amnezia_services_route)
            .or(post_amnezia_account_route)
            .or(post_amnezia_config_route)
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
