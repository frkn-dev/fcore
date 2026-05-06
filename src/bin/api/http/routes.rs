use async_trait::async_trait;
use std::sync::Arc;
use warp::Filter;

use fcore::{
    http::filters::{auth, with_i64},
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations, Result,
    Subscription, SubscriptionOperations,
};

use super::{
    super::{config::ServiceConfig, service::Service},
    filters::*,
    handlers::{
        connection::*, healthcheck_handler, key::*, metrics::*, node::*, subscription::*, trial::*,
    },
    param::*,
    rejection,
    request::*,
};

#[async_trait]
pub trait Http {
    async fn run(&self, params: ServiceConfig) -> Result<()>;
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
        + std::fmt::Debug
        + serde::Serialize
        + PartialEq,
    N: NodeStorageOperations + Send + Sync + Clone,
    Connection: From<C>,

    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    async fn run(&self, params: ServiceConfig) -> Result<()> {
        let auth = auth(Arc::new(self.settings.service.token.clone()));

        let cors = warp::cors()
            .allow_origin(params.base_url.as_str())
            .allow_methods(vec!["GET", "POST", "DELETE", "OPTIONS"])
            .allow_headers(vec!["Content-Type", "Authorization"])
            .max_age(86400)
            .build();

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
            .and(with_metrics(self.metrics.clone()))
            .and_then(get_nodes_handler);

        let post_node_register_route = warp::post()
            .and(warp::path("node"))
            .and(warp::path::end())
            .and(auth.clone())
            .and(warp::body::json::<NodeRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(post_node_handler);

        let get_node_route = warp::get()
            .and(warp::path("node"))
            .and(warp::path::end())
            .and(auth.clone())
            .and(warp::query::<NodeIdParam>())
            .and(with_sync(self.sync.clone()))
            .and_then(get_node_handler);

        let get_subscription_route = warp::get()
            .and(warp::path("sub"))
            .and(warp::path::end())
            .and(warp::query::<SubscriptionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(subscription_link_handler);

        let get_subscription_info_route = warp::get()
            .and(warp::path!("subscription" / Uuid))
            .and(warp::path::end())
            .and(with_sync(self.sync.clone()))
            .and(with_metrics(self.metrics.clone()))
            .and_then(get_subscription_info_json);

        let post_subscription_route = warp::post()
            .and(warp::path("subscription"))
            .and(warp::path::end())
            .and(auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_i64(params.bonus_days))
            .and(with_param_vec_string(params.system_refer_codes.clone()))
            .and_then(post_subscription_handler);

        let put_subscription_route = warp::put()
            .and(warp::path("subscription"))
            .and(warp::path::end())
            .and(auth.clone())
            .and(warp::query::<SubIdQueryParam>())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and_then(put_subscription_handler);

        // Connections Routes
        let get_a_connection_route = warp::path!("connection")
            .and(warp::get())
            .and(auth.clone())
            .and(warp::query::<ConnQueryParam>())
            .and(with_sync(self.sync.clone()))
            .and_then(get_connection_handler);

        let get_wg_connections_info_route = warp::path!("info" / "connections" / "wireguard")
            .and(warp::get())
            .and(warp::query::<ConnectionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(wireguard_connections_handler);

        let get_mtproto_connections_info_route = warp::path!("info" / "connections" / "mtproto")
            .and(warp::get())
            .and(warp::query::<ConnectionInfoRequest>())
            .and(with_sync(self.sync.clone()))
            .and_then(mtproto_connections_handler);

        let post_connections_sync_route = warp::path("connections")
            .and(warp::path("sync"))
            .and(warp::post())
            .and(auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and_then(get_connections_handler);

        let post_connection_route = warp::post()
            .and(warp::path("connection"))
            .and(warp::path::end())
            .and(auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network.clone()))
            .and_then(create_connection_handler);

        let delete_connection_route = warp::delete()
            .and(warp::path("connection"))
            .and(warp::path::end())
            .and(auth.clone())
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
            .and(auth.clone())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_param_vec(params.key_sign_token))
            .and_then(post_key_handler);

        let post_activate_key_route = warp::post()
            .and(warp::path("key"))
            .and(warp::path("activate"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and_then(post_activate_key_handler);

        //Trial
        let post_trial_route = warp::post()
            .and(warp::path("trial"))
            .and(warp::path::end())
            .and(warp::body::json())
            .and(with_sync(self.sync.clone()))
            .and(with_email_store(self.email_store.clone()))
            .and(with_param_ipaddrmask(params.wireguard_network))
            .and(with_param_vec_string(params.system_refer_codes))
            .and(with_param_envs(params.enabled_envs))
            .and(with_param_tags(params.enabled_tags))
            .and(with_i64(params.trial_days))
            .and(with_i64(params.bonus_days))
            .and_then(post_trial_handler);

        use uuid::Uuid;
        let ws_all_metrics_route = warp::path!("metrics" / "all" / Uuid / u64 / "ws")
            .and(warp::ws())
            .and(with_metrics(self.metrics.clone()))
            .map(
                |node_id: Uuid, series_hash: u64, ws: warp::ws::Ws, storage| {
                    ws.on_upgrade(move |socket| {
                        handle_ws_client(socket, node_id, series_hash, storage)
                    })
                },
            );

        let ws_aggregate_route =
            warp::path!("metrics" / "aggregate" / String / String / String / "ws")
                .and(warp::ws())
                .and(with_metrics(self.metrics.clone()))
                .map(
                    |tag_key: String,
                     tag_value: String,
                     metric_name: String,
                     ws: warp::ws::Ws,
                     storage| {
                        ws.on_upgrade(move |socket| {
                            handle_aggregated_ws(socket, tag_key, tag_value, metric_name, storage)
                        })
                    },
                );

        let routes = get_healthcheck_route
            // Subscription
            .or(get_subscription_route)
            .or(get_subscription_info_route)
            .or(post_subscription_route)
            .or(put_subscription_route)
            // Node
            .or(get_nodes_route)
            .or(get_node_route)
            .or(post_node_register_route)
            // Connection
            .or(post_connection_route)
            .or(post_connections_sync_route)
            .or(delete_connection_route)
            .or(get_mtproto_connections_info_route)
            .or(get_wg_connections_info_route)
            .or(get_a_connection_route)
            // Key
            .or(get_key_validation_route)
            .or(post_key_route)
            .or(post_activate_key_route)
            //Trial
            .or(post_trial_route)
            // Metrics
            .or(ws_all_metrics_route)
            .or(ws_aggregate_route)
            .recover(rejection)
            .with(cors);

        warp::serve(routes)
            .run((self.settings.service.listen, self.settings.service.port))
            .await;

        Ok(())
    }
}
