use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Status};

use crate::error::Error;
use crate::memory::tag::ProtoTag as Tag;

use super::api::{
    app::proxyman::command::{
        AddUserOperation, AlterInboundRequest, GetInboundUserRequest, GetInboundUserResponse,
        GetInboundUsersCountResponse, RemoveUserOperation,
    },
    common::protocol::User,
    common::serial::TypedMessage,
};

use super::api::app::{
    proxyman::command::handler_service_client::HandlerServiceClient,
    stats::command::stats_service_client::StatsServiceClient,
};

use tonic::transport::Channel;

use super::shadowsocks::ConnInfo as SsConnInfo;
use super::vless::ConnFlow;
use super::vless::ConnInfo as VlessConnInfo;
use super::vmess::ConnInfo as VmessConnInfo;

pub trait XrayClient {
    type Client;

    fn new(endpoint: &str) -> impl std::future::Future<Output = Result<Self, Error>> + Send
    where
        Self: Sized;
}

#[derive(Clone)]
pub struct HandlerClient {
    pub client: HandlerServiceClient<Channel>,
}

impl XrayClient for HandlerClient {
    type Client = HandlerServiceClient<Channel>;

    async fn new(endpoint: &str) -> Result<Self, Error> {
        let channel = Channel::from_shared(endpoint.to_string())?
            .connect()
            .await?;

        Ok(Self {
            client: HandlerServiceClient::new(channel),
        })
    }
}

#[derive(Clone)]
pub struct StatsClient {
    pub client: StatsServiceClient<Channel>,
}

impl XrayClient for StatsClient {
    type Client = StatsServiceClient<Channel>;
    async fn new(endpoint: &str) -> Result<Self, Error> {
        let channel = Channel::from_shared(endpoint.to_string())?
            .connect()
            .await?;

        Ok(Self {
            client: StatsServiceClient::new(channel),
        })
    }
}

#[async_trait::async_trait]
pub trait HandlerActions {
    async fn create(
        &self,
        conn_id: &uuid::Uuid,
        tag: Tag,
        password: Option<String>,
    ) -> Result<(), Error>;
    async fn remove(
        &self,
        conn_id: &uuid::Uuid,
        tag: Tag,
        password: Option<String>,
    ) -> Result<(), Error>;
}

#[async_trait::async_trait]
impl HandlerActions for Arc<Mutex<HandlerClient>> {
    async fn create(
        &self,
        conn_id: &uuid::Uuid,
        tag: Tag,
        password: Option<String>,
    ) -> Result<(), Error> {
        match tag {
            Tag::VlessTcpReality => {
                let user_info = VlessConnInfo::new(conn_id, ConnFlow::Vision, Tag::VlessTcpReality);
                user_info.create(self.clone()).await?;
                Ok(())
            }
            Tag::VlessGrpcReality => {
                let user_info =
                    VlessConnInfo::new(conn_id, ConnFlow::Direct, Tag::VlessGrpcReality);
                user_info.create(self.clone()).await?;
                Ok(())
            }
            Tag::VlessXhttpReality => {
                let user_info = VlessConnInfo::new(conn_id, ConnFlow::None, Tag::VlessXhttpReality);
                user_info.create(self.clone()).await?;
                Ok(())
            }
            Tag::VlessXhttpCdn => {
                let user_info = VlessConnInfo::new(conn_id, ConnFlow::None, Tag::VlessXhttpCdn);
                user_info.create(self.clone()).await?;
                Ok(())
            }
            Tag::Vmess => {
                let user_info = VmessConnInfo::new(conn_id);
                user_info.create(self.clone()).await?;
                Ok(())
            }
            Tag::Shadowsocks => {
                if let Some(pass) = password.clone() {
                    let user_info = SsConnInfo::new(conn_id, Some(pass));
                    user_info.create(self.clone()).await?;
                    Ok(())
                } else {
                    Err(Error::Custom(
                        "Create SS user error, password not provided".to_string(),
                    ))
                }
            }
            _ => Err(Error::Custom("Not supported Proto".into())),
        }
    }

    async fn remove(
        &self,
        conn_id: &uuid::Uuid,
        tag: Tag,
        password: Option<String>,
    ) -> Result<(), Error> {
        match tag {
            Tag::VlessTcpReality => {
                let user_info = VlessConnInfo::new(conn_id, ConnFlow::Vision, tag);
                let _ = user_info.remove(self.clone()).await;
                Ok(())
            }
            Tag::VlessGrpcReality => {
                let user_info = VlessConnInfo::new(conn_id, ConnFlow::Direct, tag);
                let _ = user_info.remove(self.clone()).await;
                Ok(())
            }
            Tag::VlessXhttpReality => {
                let user_info = VlessConnInfo::new(conn_id, ConnFlow::None, tag);
                let _ = user_info.remove(self.clone()).await;
                Ok(())
            }
            Tag::VlessXhttpCdn => {
                let user_info = VlessConnInfo::new(conn_id, ConnFlow::None, tag);
                let _ = user_info.remove(self.clone()).await;
                Ok(())
            }
            Tag::Vmess => {
                let user_info = VmessConnInfo::new(conn_id);
                let _ = user_info.remove(self.clone()).await;
                Ok(())
            }
            Tag::Shadowsocks => {
                if let Some(pass) = password.clone() {
                    let user_info = SsConnInfo::new(conn_id, Some(pass));
                    let _ = user_info.remove(self.clone()).await;
                    return Ok(());
                }
                Err(Error::Custom(
                    "Remove SS user error, password not provided".to_string(),
                ))
            }
            _ => Err(Error::Custom("Not supported Proto".into())),
        }
    }
}

#[async_trait::async_trait]
pub trait ProtocolConn: Send + Sync {
    fn tag(&self) -> Tag;
    fn email(&self) -> String;
    fn to_user(&self) -> Result<User, Box<dyn std::error::Error + Send + Sync>>;

    async fn create(&self, client: Arc<Mutex<HandlerClient>>) -> Result<(), Status> {
        let request = AlterInboundRequest {
            tag: self.tag().to_string(),
            operation: Some(TypedMessage {
                r#type: "xray.app.proxyman.command.AddUserOperation".to_string(),
                value: prost::Message::encode_to_vec(&AddUserOperation {
                    user: Some(self.to_user().expect("REASON")),
                }),
            }),
        };

        let mut locked = client.lock().await;
        locked
            .client
            .alter_inbound(Request::new(request))
            .await
            .map(|_| ())
    }

    async fn remove(&self, client: Arc<Mutex<HandlerClient>>) -> Result<(), Status> {
        let operation = RemoveUserOperation {
            email: self.email(),
        };

        let operation_message = TypedMessage {
            r#type: "xray.app.proxyman.command.RemoveUserOperation".to_string(),
            value: prost::Message::encode_to_vec(&operation),
        };

        let request = AlterInboundRequest {
            tag: self.tag().to_string(),
            operation: Some(operation_message),
        };

        let mut handler_client = client.lock().await;
        handler_client
            .client
            .alter_inbound(Request::new(request))
            .await
            .map(|_| ())
    }
}

#[async_trait::async_trait]
pub trait ConnOp {
    async fn conn_op(
        &mut self,
        tag: Tag,
        user_id: String,
    ) -> Result<GetInboundUserResponse, Status>;
    async fn conn_count_op(&mut self, tag: Tag) -> Result<i64, Status>;
}

#[async_trait::async_trait]
impl ConnOp for HandlerClient {
    /// Not used
    async fn conn_op(
        &mut self,
        tag: Tag,
        conn_id: String,
    ) -> Result<GetInboundUserResponse, Status> {
        let request = GetInboundUserRequest {
            tag: tag.to_string(),
            email: format!("{}@pony", conn_id).to_string(),
        };

        self.client
            .get_inbound_users(Request::new(request))
            .await
            .map(|res| res.into_inner())
    }

    async fn conn_count_op(&mut self, tag: Tag) -> Result<i64, Status> {
        let request = GetInboundUserRequest {
            tag: tag.to_string(),
            email: "".to_string(),
        };

        match self
            .client
            .get_inbound_users_count(Request::new(request))
            .await
        {
            Ok(res) => {
                let res: GetInboundUsersCountResponse = res.into_inner();
                Ok(res.count)
            }
            Err(e) => {
                let error = format!("Failed to fetch conn count for tag {}: {}", tag, e);
                Err(Status::internal(error))
            }
        }
    }
}
