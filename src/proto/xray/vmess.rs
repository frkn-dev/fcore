use super::api::proxy::vmess;
use super::api::{common::protocol::User, common::serial::TypedMessage};
use crate::memory::tag::ProtoTag as Tag;

use super::client::ProtocolConn;

#[derive(Clone, Debug)]
pub struct ConnInfo {
    pub in_tag: Tag,
    pub level: u32,
    pub email: String,
    pub uuid: uuid::Uuid,
}

impl ConnInfo {
    pub fn new(uuid: &uuid::Uuid) -> Self {
        Self {
            in_tag: Tag::Vmess,
            level: 0,
            email: format!("{}@{}", uuid, "pony"),
            uuid: match std::env::var("FCORE_VMESS_NODE_IDENTITY") {
                Ok(identity) if !identity.is_empty() => crate::utils::vmess_node_uuid(uuid, &identity),
                _ => *uuid,
            },
        }
    }
}

#[async_trait::async_trait]
impl ProtocolConn for ConnInfo {
    fn tag(&self) -> Tag {
        self.in_tag
    }
    fn email(&self) -> String {
        self.email.clone()
    }
    fn to_user(&self) -> Result<User, Box<dyn std::error::Error + Send + Sync>> {
        let account = vmess::Account {
            id: self.uuid.to_string(),
            ..Default::default()
        };

        Ok(User {
            level: self.level,
            email: self.email.clone(),
            account: Some(TypedMessage {
                r#type: "xray.proxy.vmess.Account".to_string(),
                value: prost::Message::encode_to_vec(&account),
            }),
        })
    }
}
