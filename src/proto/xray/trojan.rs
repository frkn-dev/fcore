use super::api::proxy::trojan;
use super::api::{common::protocol::User, common::serial::TypedMessage};
use crate::memory::tag::ProtoTag as Tag;
use super::client::ProtocolConn;

#[derive(Clone, Debug)]
pub struct ConnInfo {
    uuid: uuid::Uuid,
}

impl ConnInfo {
    pub fn new(uuid: &uuid::Uuid) -> Self {
        Self { uuid: *uuid }
    }
}

#[async_trait::async_trait]
impl ProtocolConn for ConnInfo {
    fn tag(&self) -> Tag { Tag::Trojan }
    fn email(&self) -> String { format!("{}@pony", self.uuid) }
    fn to_user(&self) -> Result<User, Box<dyn std::error::Error + Send + Sync>> {
        let account = trojan::Account { password: self.uuid.to_string() };
        Ok(User {
            level: 0,
            email: self.email(),
            account: Some(TypedMessage {
                r#type: "xray.proxy.trojan.Account".to_string(),
                value: prost::Message::encode_to_vec(&account),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn xray_user_uses_connection_id_for_password_and_accounting() {
        let id = uuid::Uuid::from_u128(1);
        let info = ConnInfo::new(&id);
        let user = info.to_user().unwrap();
        assert_eq!(info.tag(), Tag::Trojan);
        assert_eq!(user.email, format!("{id}@pony"));
        let typed = user.account.unwrap();
        assert_eq!(typed.r#type, "xray.proxy.trojan.Account");
        let account = trojan::Account::decode(typed.value.as_slice()).unwrap();
        assert_eq!(account.password, id.to_string());
    }
}
