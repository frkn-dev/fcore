use super::super::super::env::Env;
use super::super::conn::Conn;
use super::super::proto::Proto;
use crate::zmq::message::Action;
use crate::zmq::message::Message;

pub trait Operations {
    fn get_env(&self) -> Env;
    fn set_env(&mut self, env: &Env);

    fn as_update_message(&self, conn_id: &uuid::Uuid) -> Message;
    fn as_create_message(&self, conn_id: &uuid::Uuid) -> Message;
    fn as_delete_message(&self, conn_id: &uuid::Uuid) -> Message;
}

impl Operations for Conn {
    fn get_env(&self) -> Env {
        self.env.clone()
    }

    fn set_env(&mut self, env: &Env) {
        self.env = env.clone();
    }

    fn as_create_message(&self, conn_id: &uuid::Uuid) -> Message {
        let password = match &self.proto {
            Proto::Shadowsocks { password } => Some(password.clone()),
            _ => None,
        };

        let tag = self.proto.proto();
        let expires_at = self.expires_at;

        let token = match &self.proto {
            Proto::Hysteria2 { token } => Some(*token),
            _ => None,
        };

        let wg = match &self.proto {
            Proto::Wireguard { param, .. } => Some(param.clone()),
            Proto::AmneziaWg { param, .. } => Some(param.clone()),
            Proto::AmneziaWgMobile { param, .. } => Some(param.clone()),
            _ => None,
        };

        let sub_id = self.subscription_id;

        Message {
            conn_id: (*conn_id),
            subscription_id: sub_id,
            action: Action::Create,
            password,
            token,
            tag,
            wg,
            expires_at: expires_at.map(Into::into),
        }
    }

    fn as_update_message(&self, conn_id: &uuid::Uuid) -> Message {
        let password = match &self.proto {
            Proto::Shadowsocks { password } => Some(password.clone()),
            _ => None,
        };

        let tag = self.proto.proto();
        let expires_at = self.expires_at;

        let wg = match &self.proto {
            Proto::Wireguard { param, .. } => Some(param.clone()),
            _ => None,
        };

        let token = match &self.proto {
            Proto::Hysteria2 { token } => Some(*token),
            _ => None,
        };

        let sub_id = self.subscription_id;

        Message {
            conn_id: (*conn_id),
            subscription_id: sub_id,
            action: Action::Update,
            password,
            token,
            tag,
            wg,
            expires_at: expires_at.map(Into::into),
        }
    }

    fn as_delete_message(&self, conn_id: &uuid::Uuid) -> Message {
        let password = match &self.proto {
            Proto::Shadowsocks { password } => Some(password.clone()),
            _ => None,
        };

        let tag = self.proto.proto();
        let expires_at = self.expires_at;

        let wg = match &self.proto {
            Proto::Wireguard { param, .. } => Some(param.clone()),
            _ => None,
        };

        let token = match &self.proto {
            Proto::Hysteria2 { token } => Some(*token),
            _ => None,
        };

        let sub_id = self.subscription_id;

        Message {
            conn_id: (*conn_id),
            subscription_id: sub_id,
            action: Action::Delete,
            password,
            token,
            tag,
            wg,
            expires_at: expires_at.map(Into::into),
        }
    }
}
