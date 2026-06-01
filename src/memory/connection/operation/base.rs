use chrono::DateTime;
use chrono::Utc;

use super::super::base::Base;
use super::super::conn::Conn;
use super::super::proto::Proto;
use super::super::wireguard::Param as WgParam;
use crate::error::{Error, Result};

pub trait Operations {
    fn get_modified_at(&self) -> DateTime<Utc>;
    fn set_modified_at(&mut self);

    fn get_expires_at(&self) -> Option<DateTime<Utc>>;
    fn set_expires_at(&mut self, dt: Option<DateTime<Utc>>);

    fn set_proto(&mut self, proto: Proto);
    fn get_proto(&self) -> Proto;

    fn set_deleted(&mut self, v: bool);
    fn get_deleted(&self) -> bool;

    fn get_wireguard(&self) -> Option<&WgParam>;
    fn get_password(&self) -> Option<String>;
    fn get_token(&self) -> Option<uuid::Uuid>;
    fn set_password(&mut self, password: Option<String>) -> Result<()>;

    fn get_subscription_id(&self) -> Option<uuid::Uuid>;
    fn set_subscription_id(&mut self, subscription_id: &uuid::Uuid);
}

impl Operations for Base {
    fn get_modified_at(&self) -> DateTime<Utc> {
        self.modified_at
    }
    fn set_modified_at(&mut self) {
        self.modified_at = Utc::now();
    }

    fn get_expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    fn set_expires_at(&mut self, dt: Option<DateTime<Utc>>) {
        self.expires_at = dt;
    }

    fn set_proto(&mut self, proto: Proto) {
        self.proto = proto
    }
    fn get_proto(&self) -> Proto {
        self.proto.clone()
    }

    fn set_deleted(&mut self, v: bool) {
        self.is_deleted = v;
    }
    fn get_deleted(&self) -> bool {
        self.is_deleted
    }

    fn get_password(&self) -> Option<String> {
        match &self.proto {
            Proto::Shadowsocks { password } => Some(password.clone()),
            _ => None,
        }
    }

    fn get_token(&self) -> Option<uuid::Uuid> {
        match &self.proto {
            Proto::Hysteria2 { token } => Some(*token),
            _ => None,
        }
    }

    fn get_wireguard(&self) -> Option<&WgParam> {
        match &self.proto {
            Proto::Wireguard { param, .. } => Some(param),
            _ => None,
        }
    }

    fn set_password(&mut self, password: Option<String>) -> Result<()> {
        match (&mut self.proto, password) {
            (Proto::Shadowsocks { password: p }, Some(new_pw)) => {
                *p = new_pw;
                Ok(())
            }
            _ => Err(Error::Custom(
                "Password update failed: not a Shadowsocks connection".into(),
            )),
        }
    }

    fn get_subscription_id(&self) -> Option<uuid::Uuid> {
        self.subscription_id
    }
    fn set_subscription_id(&mut self, subscription_id: &uuid::Uuid) {
        self.subscription_id = Some(*subscription_id);
    }
}

impl Operations for Conn {
    fn get_modified_at(&self) -> DateTime<Utc> {
        self.modified_at
    }
    fn set_modified_at(&mut self) {
        self.modified_at = Utc::now();
    }

    fn get_expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    fn set_expires_at(&mut self, dt: Option<DateTime<Utc>>) {
        self.expires_at = dt;
    }

    fn set_proto(&mut self, proto: Proto) {
        self.proto = proto
    }
    fn get_proto(&self) -> Proto {
        self.proto.clone()
    }

    fn set_deleted(&mut self, v: bool) {
        self.is_deleted = v;
    }
    fn get_deleted(&self) -> bool {
        self.is_deleted
    }

    fn get_password(&self) -> Option<String> {
        match &self.proto {
            Proto::Shadowsocks { password } => Some(password.clone()),
            _ => None,
        }
    }

    fn get_wireguard(&self) -> Option<&WgParam> {
        match &self.proto {
            Proto::Wireguard { param, .. } => Some(param),
            _ => None,
        }
    }

    fn set_password(&mut self, password: Option<String>) -> Result<()> {
        match (&mut self.proto, password) {
            (Proto::Shadowsocks { password: p }, Some(new_pw)) => {
                *p = new_pw;
                Ok(())
            }
            _ => Err(Error::Custom(
                "Password update failed: not a Shadowsocks connection".into(),
            )),
        }
    }
    fn get_token(&self) -> Option<uuid::Uuid> {
        match &self.proto {
            Proto::Hysteria2 { token } => Some(*token),
            _ => None,
        }
    }

    fn get_subscription_id(&self) -> Option<uuid::Uuid> {
        self.subscription_id
    }
    fn set_subscription_id(&mut self, subscription_id: &uuid::Uuid) {
        self.subscription_id = Some(*subscription_id);
    }
}
