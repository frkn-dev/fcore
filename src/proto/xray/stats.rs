use async_trait::async_trait;
use tonic::Status;

use super::api::app::stats::command::GetStatsResponse;
use crate::memory::connection::stat::Stat as ConnectionStat;
use crate::memory::node::InboundStat;
use crate::memory::stat::StatType;
use crate::memory::tag::ProtoTag as Tag;

#[derive(Debug, Clone, Copy)]
pub enum Prefix {
    ConnPrefix(uuid::Uuid),
    InboundPrefix(Tag),
}

impl Prefix {
    pub fn as_tag(&self) -> Option<&Tag> {
        if let Prefix::InboundPrefix(tag) = self {
            Some(tag)
        } else {
            None
        }
    }
}

#[async_trait]
pub trait StatsOp {
    async fn stat(
        &self,
        prefix: Prefix,
        stat_kind: StatType,
        reset: bool,
    ) -> Result<GetStatsResponse, Status>;
    async fn conn(&self, conn_id: Prefix) -> Result<ConnectionStat, Status>;
    async fn inbound(&self, inbound: Prefix) -> Result<InboundStat, Status>;
    async fn conn_count(&self, inbound: Tag) -> Result<Option<i64>, Status>;
    async fn reset(&self, conn_id: &uuid::Uuid) -> Result<(), Status>;
}
