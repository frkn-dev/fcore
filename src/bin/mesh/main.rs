use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use warp::Filter;
use tokio_postgres::NoTls;

mod http;
mod store;
mod subs;

use http::{name, poll, pull, register, send, RateLimiter};
use store::{Store, POLL_TIMEOUT};
use subs::PgSubscriptions;

#[derive(Clone, Debug, Deserialize)]
struct Settings {
    service: ServiceConfig,
    pg: PgConfig,
}

#[derive(Clone, Debug, Deserialize)]
struct ServiceConfig {
    listen: SocketAddr,
    #[serde(default = "default_db_path")]
    db_path: String,
}

#[derive(Clone, Debug, Deserialize)]
struct PgConfig {
    host: String,
    port: u16,
    db: String,
    username: String,
    password: String,
}

fn default_db_path() -> String {
    "/var/lib/fcore/mesh/mesh.db".to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config_path = std::env::args().nth(1).expect("config path required");
    let raw = std::fs::read_to_string(&config_path)?;
    let settings: Settings = toml::from_str(&raw)?;

    if let Some(parent) = std::path::Path::new(&settings.service.db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pg = &settings.pg;
    let conninfo = format!(
        "host={} user={} dbname={} password={} port={} connect_timeout=5",
        pg.host, pg.username, pg.db, pg.password, pg.port
    );
    let (client, connection) = tokio_postgres::connect(&conninfo, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!(error = %e, "mesh: postgres connection dropped");
        }
    });

    let store = Arc::new(
        Store::open(
            &settings.service.db_path,
            Arc::new(PgSubscriptions::new(client)),
            POLL_TIMEOUT,
        )
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{e:?}")))?,
    );
    let limiter = Arc::new(RateLimiter::new(60, Duration::from_secs(60)));
    let gc = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            gc.gc();
        }
    });

    let routes = {
        let reg = warp::post()
            .and(warp::path!("v1" / "mesh" / "register"))
            .and(warp::path::end())
            .and(http::body_limit())
            .and(warp::body::json())
            .and(with_store(store.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_limiter(limiter.clone()))
            .and_then(register);

        let look = warp::get()
            .and(warp::path!("v1" / "mesh" / "lookup" / String))
            .and(warp::path::end())
            .and(with_store(store.clone()))
            .and_then(lookup_route);

        let send_route = warp::post()
            .and(warp::path!("v1" / "mesh" / "send"))
            .and(warp::path::end())
            .and(http::body_limit())
            .and(warp::body::json())
            .and(with_store(store.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_limiter(limiter.clone()))
            .and_then(send);

        let pull_route = warp::post()
            .and(warp::path!("v1" / "mesh" / "pull"))
            .and(warp::path::end())
            .and(http::body_limit())
            .and(warp::body::json())
            .and(with_store(store.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_limiter(limiter.clone()))
            .and_then(pull);

        // long-poll holds a connection instead of re-requesting — the per-minute
        // IP limiter would 429 an active chat; abusive parallel polls are bounded
        // by the 30s server-side hold, not by request rate
        let poll_route = warp::post()
            .and(warp::path!("v1" / "mesh" / "poll"))
            .and(warp::path::end())
            .and(http::body_limit())
            .and(warp::body::json())
            .and(with_store(store.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and_then(poll);

        let name_route = warp::post()
            .and(warp::path!("v1" / "mesh" / "name"))
            .and(warp::path::end())
            .and(http::body_limit())
            .and(warp::body::json())
            .and(with_store(store.clone()))
            .and(warp::addr::remote())
            .and(warp::header::optional::<String>("x-forwarded-for"))
            .and(with_limiter(limiter.clone()))
            .and_then(name);

        reg.or(look)
            .or(send_route)
            .or(pull_route)
            .or(poll_route)
            .or(name_route)
    };

    tracing::info!(listen = %settings.service.listen, db = %settings.service.db_path, "meshd");
    warp::serve(routes).run(settings.service.listen).await;
    Ok(())
}

async fn lookup_route(
    uin: String,
    store: Arc<Store>,
) -> Result<impl warp::Reply, warp::Rejection> {
    http::lookup(uin, store).await
}

fn with_store(
    store: Arc<Store>,
) -> impl Filter<Extract = (Arc<Store>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || store.clone())
}

fn with_limiter(
    limiter: Arc<RateLimiter>,
) -> impl Filter<Extract = (Arc<RateLimiter>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || limiter.clone())
}
