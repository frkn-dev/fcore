use serde::{Deserialize, Serialize};

use fcore::Code;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubIdQueryParam {
    pub id: uuid::Uuid,
    pub env: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NodesQueryParams {
    pub env: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnQueryParam {
    pub id: uuid::Uuid,
}

#[derive(Serialize, Deserialize)]
pub struct KeyQueryParams {
    pub key: Code,
}
