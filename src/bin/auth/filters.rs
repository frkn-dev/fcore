use fcore::{ApiAccessConfig, Env, Tag};
use warp::Filter;

pub fn with_api_settings(
    api: ApiAccessConfig,
) -> impl Filter<Extract = (ApiAccessConfig,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || api.clone())
}

pub fn with_envs(
    envs: Vec<Env>,
) -> impl Filter<Extract = (Vec<Env>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || envs.clone())
}

pub fn with_protos(
    protos: Vec<Tag>,
) -> impl Filter<Extract = (Vec<Tag>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || protos.clone())
}
