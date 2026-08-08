use std::collections::hash_map::Entry;

use super::super::connection::conn::Conn as Connection;
use super::super::connection::conn::ConnPatch as ConnectionPatch;
use super::super::connection::operation::api::Operations as ConnectionApiOp;
use super::super::connection::operation::base::Operations as ConnectionBaseOp;
use super::super::connection::proto::Proto;
use super::super::connection::wireguard::IpAddrMask;
use super::super::connection::Connections;
use super::super::storage::Status as OperationStatus;
use super::super::tag::ProtoTag as Tag;
use crate::error::{Error, Result};

pub trait ApiOp<C>
where
    C: Clone + Send + Sync + 'static,
{
    fn add(&mut self, conn_id: &uuid::Uuid, new_conn: C) -> Result<OperationStatus>;
    fn get_by_subscription_id(&self, subscription_id: &uuid::Uuid) -> Option<Vec<(uuid::Uuid, C)>>;
    fn get_by_proto(&self, proto: Tag) -> Option<Vec<(uuid::Uuid, C)>>;
    /// Max allocated address among connections of the given WG-family tag
    /// (Wireguard / AmneziaWg / AmneziaWgMobile) — each tag has its own pool.
    fn get_last_addr(&self, tag: Tag) -> Option<IpAddrMask>;
    fn apply_update(conn: &mut Connection, patch: ConnectionPatch) -> Option<Connection>;
}

pub trait BaseOp<C>
where
    C: Clone + Send + Sync + 'static,
{
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
    fn add(&mut self, conn_id: &uuid::Uuid, new_conn: C) -> Result<OperationStatus>;
    fn remove(&mut self, conn_id: &uuid::Uuid) -> Result<()>;
    fn get(&self, conn_id: &uuid::Uuid) -> Option<C>;
    fn validate_token(&self, token: &uuid::Uuid) -> Option<uuid::Uuid>;
}

impl<C> BaseOp<C> for Connections<C>
where
    C: ConnectionBaseOp + Clone + Send + Sync + 'static,
{
    fn len(&self) -> usize {
        self.0.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn validate_token(&self, token: &uuid::Uuid) -> Option<uuid::Uuid> {
        self.iter()
            .find(|(_, conn)| {
                matches!(
                    conn.get_proto(),
                    Proto::Hysteria2 { token: t } if t == *token
                )
            })
            .map(|(id, _)| *id)
    }

    fn add(&mut self, conn_id: &uuid::Uuid, new_conn: C) -> Result<OperationStatus> {
        match self.entry(*conn_id) {
            Entry::Occupied(_) => return Ok(OperationStatus::AlreadyExist(*conn_id)),
            Entry::Vacant(entry) => {
                entry.insert(new_conn);
            }
        }
        Ok(OperationStatus::Ok(*conn_id))
    }

    fn remove(&mut self, conn_id: &uuid::Uuid) -> Result<()> {
        self.0
            .remove(conn_id)
            .map(|_| ())
            .ok_or(Error::Custom("Conn not found".into()))
    }

    fn get(&self, conn_id: &uuid::Uuid) -> Option<C> {
        self.0.get(conn_id).cloned()
    }
}

impl<C> ApiOp<C> for Connections<C>
where
    C: ConnectionBaseOp + ConnectionApiOp + Clone + Send + Sync + PartialEq<C> + 'static,
{
    fn add(&mut self, conn_id: &uuid::Uuid, new_conn: C) -> Result<OperationStatus> {
        match self.entry(*conn_id) {
            Entry::Occupied(_entry) => Ok(OperationStatus::AlreadyExist(*conn_id)),
            Entry::Vacant(entry) => {
                entry.insert(new_conn);
                Ok(OperationStatus::Ok(*conn_id))
            }
        }
    }

    fn apply_update(conn: &mut Connection, patch: ConnectionPatch) -> Option<Connection> {
        if patch.is_deleted.is_none() && conn.get_deleted() {
            return None;
        }

        if let Some(proto) = &patch.proto {
            if conn.get_proto() != *proto {
                return None;
            }
        }

        if let Some(_password) = conn.get_password() {
            if conn.get_proto().proto() != Tag::Shadowsocks && patch.proto.is_some() {
                return None;
            }
        }

        let mut changed = false;

        // deleted
        if let Some(deleted) = patch.is_deleted {
            if conn.get_deleted() != deleted {
                conn.set_deleted(deleted);
                changed = true;
            }
        }

        // env
        if let Some(env) = patch.env {
            if conn.get_env() != env {
                conn.set_env(&env);
                changed = true;
            }
        }

        // expires_at
        if let Some(exp) = patch.expires_at {
            if conn.get_expires_at() != Some(exp) {
                conn.set_expires_at(Some(exp));
                changed = true;
            }
        }

        // subscription_id
        if let Some(sub_id) = patch.subscription_id {
            if conn.get_subscription_id() != Some(sub_id) {
                conn.set_subscription_id(&sub_id);
                changed = true;
            }
        }

        if changed {
            conn.set_modified_at();
            Some(conn.clone())
        } else {
            None
        }
    }

    fn get_by_subscription_id(&self, subscription_id: &uuid::Uuid) -> Option<Vec<(uuid::Uuid, C)>> {
        let conns: Vec<(uuid::Uuid, C)> = self
            .iter()
            .filter(|(_, conn)| conn.get_subscription_id() == Some(*subscription_id))
            .map(|(conn_id, conn)| (*conn_id, conn.clone()))
            .collect();

        if conns.is_empty() {
            None
        } else {
            Some(conns)
        }
    }

    fn get_by_proto(&self, proto: Tag) -> Option<Vec<(uuid::Uuid, C)>> {
        let conns: Vec<(uuid::Uuid, C)> = self
            .iter()
            .filter(|(_, conn)| conn.get_proto().proto() == proto)
            .map(|(conn_id, conn)| (*conn_id, conn.clone()))
            .collect();

        if conns.is_empty() {
            None
        } else {
            Some(conns)
        }
    }

    fn get_last_addr(&self, tag: Tag) -> Option<IpAddrMask> {
        self.iter()
            .filter(|(_, conn)| conn.get_proto().proto() == tag)
            .max_by_key(|(_, conn)| {
                let param = conn.get_wireguard().or_else(|| conn.get_amneziawg());
                param
                    .and_then(|wg| match wg.address.address {
                        std::net::IpAddr::V4(ip) => Some(u32::from(ip)),
                        _ => None,
                    })
                    .unwrap_or(0)
            })
            .and_then(|(_, conn)| {
                conn.get_wireguard()
                    .or_else(|| conn.get_amneziawg())
                    .map(|wg| wg.address.clone())
            })
    }
}
