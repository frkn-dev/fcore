use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AccountRequest {
    pub user: Option<String>,
    pub email: Option<String>,
    pub referred_by: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub trial: bool,
    pub days: Option<i64>,
    pub limit_bytes: Option<i64>,
    pub subscription_id: Option<uuid::Uuid>,
}

impl AccountRequest {
    pub fn email(&self) -> Option<&str> {
        self.email
            .as_deref()
            .or(self.user.as_deref())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    }
}

#[derive(Debug, Deserialize)]
pub struct RefCodeQuery {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct TrialsQuery {
    pub period: i64,
    #[serde(default = "default_trials_granularity")]
    pub granularity: String,
}

fn default_trials_granularity() -> String {
    "daily".to_string()
}
