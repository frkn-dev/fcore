use serde::Deserialize;

#[derive(Deserialize)]
pub struct Auth {
    pub addr: String,
    pub auth: uuid::Uuid,
    pub tx: u64,
}
