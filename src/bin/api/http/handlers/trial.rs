use chrono::{DateTime, Utc};
use std::net::{IpAddr, Ipv4Addr};

use fcore::{
    http::helpers as http, http::response::Instance, utils, utils::get_uuid_last_octet_simple,
    Connection, ConnectionApiOperations, ConnectionBaseOperations, ConnectionStorageApiOperations,
    Env, IpAddrMask, NodeStorageOperations, Proto, Status, Subscription, SubscriptionOperations,
    SubscriptionStorageOperations, Tag, Topic, WgKeys, WgParam,
};

use super::super::super::email::EmailStore;
use super::super::super::sync::{tasks::SyncOp, MemSync};
use super::super::request;

pub async fn post_trial_handler<N, C, S>(
    req: request::Trial,
    memory: MemSync<N, C, S>,
    store: EmailStore,
    wireguard_network: IpAddrMask,
    system_refer_codes: Vec<String>,
    envs: Vec<Env>,
    protos: Vec<Tag>,
    trial_days: i64,
    bonus: i64,
) -> Result<impl warp::Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    req.validate()?;

    if let Some(ref user) = req.user {
        if store.check_email_hmac(user).await {
            return Ok(http::bad_request("Trial already requested"));
        }
    }

    if let Some(ref email) = req.email {
        if store.check_email_hmac(email).await {
            return Ok(http::bad_request("Trial already requested"));
        }
    }

    let mut bonus_days = 0;
    let ref_by = req.referred_by.clone().unwrap_or_else(|| "WEB".to_string());
    let sub_id = uuid::Uuid::new_v4();

    let sub_id_to_update = if let Some(ref_by_code) = req.referred_by.clone() {
        let mem = memory.memory.read().await;

        let is_system_code = system_refer_codes.iter().any(|c| c == &ref_by_code);
        let is_user_referral = !is_system_code;

        if let Some(sub) = mem.subscriptions.find_by_refer_code(&ref_by_code) {
            if is_user_referral {
                bonus_days = bonus;
            }
            Some(sub.id())
        } else {
            return Ok(http::bad_request("Refer code not found"));
        }
    } else {
        None
    };

    if let Some(id) = sub_id_to_update {
        if let Err(e) = SyncOp::add_days(&memory, &id, bonus_days).await {
            return Ok(http::internal_error(&format!(
                "Couldn't add bonus days: {}",
                e
            )));
        }
    }

    let now = Utc::now();

    let expires_at: Option<DateTime<Utc>> = Some(now + chrono::Duration::days(trial_days.into()));

    let ref_code = get_uuid_last_octet_simple(&sub_id);
    let sub = Subscription::new(sub_id, req.referred_by, ref_code, expires_at);

    let new_sub_id = match SyncOp::add_sub(&memory, sub.clone()).await {
        Ok(Status::Ok(id)) => id,
        _ => return Ok(http::internal_error("Failed to add sub")),
    };

    for env in envs {
        for p in &protos {
            let proto = match p {
                Tag::Wireguard => {
                    let mem = memory.memory.read().await;

                    let last_ip: Option<Ipv4Addr> = mem
                        .connections
                        .get_last_wg_addr()
                        .and_then(|mask| mask.as_ipv4());

                    let next = match last_ip {
                        Some(ip) => IpAddrMask::increment_ipv4(ip),
                        None => wireguard_network.first_peer_ip(),
                    };

                    let next = match next {
                        Some(ip) => ip,
                        None => return Ok(http::internal_error("Failed to allocate IP")),
                    };

                    if !wireguard_network.contains_ipv4(next) {
                        return Ok(http::internal_error("IP out of range"));
                    }

                    Proto::Wireguard {
                        param: WgParam {
                            keys: WgKeys::default(),
                            address: IpAddrMask {
                                address: IpAddr::V4(next),
                                cidr: 32,
                            },
                        },
                    }
                }
                Tag::Shadowsocks => {
                    let password = utils::generate_random_password(15);
                    Proto::Shadowsocks { password }
                }
                Tag::VlessTcpReality
                | Tag::VlessGrpcReality
                | Tag::VlessXhttpReality
                | Tag::Vmess => Proto::Xray(p.clone()),
                Tag::Hysteria2 => {
                    let token = uuid::Uuid::new_v4();
                    Proto::Hysteria2 { token }
                }
                Tag::Mtproto => {
                    let secret = utils::generate_random_password(15);
                    Proto::Mtproto { secret }
                }
            };

            let conn: Connection = Connection::new(&env, Some(new_sub_id), proto, None);
            let conn_id = uuid::Uuid::new_v4();
            let msg = conn.as_create_message(&conn_id);
            let messages = vec![msg];

            match SyncOp::add_conn(&memory, &conn_id, conn.clone()).await {
                Ok(Status::Ok(_)) => {
                    let bytes = match rkyv::to_bytes::<_, 1024>(&messages) {
                        Ok(b) => b,
                        Err(e) => {
                            return Ok(http::internal_error(&format!("Serialization error: {}", e)))
                        }
                    };

                    let topic = if conn.get_token().is_some() {
                        Some(Topic::Auth)
                    } else if conn.get_proto().is_mtproto() {
                        None
                    } else {
                        Some(conn.get_env().into())
                    };

                    if let Some(topic) = topic {
                        let _ = memory.publisher.send_binary(&topic, bytes.as_ref()).await;
                    }
                }
                _ => continue,
            }
        }
    }

    if let Some(email) = req.user {
        let _ = store.save_trial_hmac(&email, &sub.id, &now, &ref_by).await;
    }

    if let Some(email) = req.email {
        let _ = store.save_trial_hmac(&email, &sub.id, &now, &ref_by).await;
        let _ = store.send_email_background(email, sub.id).await;
    }

    Ok(http::success_response(
        "Trial activated. Check email".into(),
        Some(sub.id),
        Instance::None,
    ))
}
