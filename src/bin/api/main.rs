#![recursion_limit = "256"]

use std::collections::HashMap;

use fcore::{Connection, Env, Node, Result, Settings, Subscription, BANNER, VERSION};

use crate::{
    config::ServiceSettings,
    service::{Cache, Service},
};

mod bootstrap;
mod config;
mod http;
mod iap;
mod metrics;
mod postgres;
mod runtime;
mod service;
mod subscription_audit;
mod sync;
mod tasks;
mod traffic;

pub type ApiService = Service<HashMap<Env, Vec<Node>>, Connection, Subscription>;

#[tokio::main]
async fn main() -> Result<()> {
    println!(">>> API Service {}", VERSION);
    println!("{}", BANNER);

    #[cfg(feature = "debug")]
    console_subscriber::init();

    let config_path = &std::env::args()
        .nth(1)
        .expect("required config path as an argument");
    println!("Config file {:?}", config_path);

    let settings = ServiceSettings::from_file(config_path);

    settings.validate().expect("Wrong settings file");
    println!(">>> Settings: {:?}", settings.clone());

    bootstrap::init_tracing(settings.clone());

    let api_service = ApiService::bootstrap(settings).await?;
    api_service.run().await
}
