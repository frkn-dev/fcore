use super::super::storage::Status as OperationStatus;
use super::super::subscription::Operations as SubscriptionOp;
use super::super::subscription::Subscriptions;

pub trait Operations<S>
where
    S: Send + Sync + Clone + 'static + PartialEq,
{
    fn find_by_id(&self, id: &uuid::Uuid) -> Option<&S>;
    fn find_by_id_mut(&mut self, id: &uuid::Uuid) -> Option<&mut S>;
    fn find_by_refer_code(&self, code: &str) -> Option<&S>;
    fn find_by_premium_token(&self, token: &str) -> Option<&S>;
    fn find_by_parent_id(&self, parent_id: &uuid::Uuid) -> Vec<S>;
    fn all(&self) -> Vec<S>;
    fn add(&mut self, new_subscription: S) -> OperationStatus;
    fn delete(&mut self, id: &uuid::Uuid);
    fn update(&mut self, subscription: S);
    fn count_invited_by(&self, refer_code: &str) -> usize;
    fn exist_refer_code(&self, code: &str) -> bool;
}

impl<S> Operations<S> for Subscriptions<S>
where
    S: SubscriptionOp + Send + Sync + Clone + 'static + PartialEq + SubscriptionOp,
{
    fn count_invited_by(&self, refer_code: &str) -> usize {
        self.values()
            .filter(|s| s.referred_by() == Some(refer_code))
            .count()
    }

    fn find_by_id(&self, id: &uuid::Uuid) -> Option<&S> {
        self.get(id)
    }
    fn find_by_id_mut(&mut self, id: &uuid::Uuid) -> Option<&mut S> {
        self.get_mut(id)
    }

    fn find_by_refer_code(&self, code: &str) -> Option<&S> {
        self.values().find(|s| s.refer_code() == code)
    }

    fn find_by_premium_token(&self, token: &str) -> Option<&S> {
        self.values()
            .find(|s| s.premium_token().map(|t| t == token).unwrap_or(false))
    }

    fn find_by_parent_id(&self, parent_id: &uuid::Uuid) -> Vec<S> {
        self.values()
            .filter(|s| s.parent_id() == Some(*parent_id))
            .cloned()
            .collect()
    }

    fn exist_refer_code(&self, code: &str) -> bool {
        self.values().any(|s| s.refer_code() == code)
    }

    fn all(&self) -> Vec<S> {
        self.values().cloned().collect()
    }

    fn add(&mut self, new_subscription: S) -> OperationStatus {
        let id = new_subscription.id();

        match self.get_mut(&id) {
            Some(existing) => {
                if existing == &new_subscription {
                    OperationStatus::AlreadyExist(id)
                } else {
                    *existing = new_subscription;
                    OperationStatus::Updated(id)
                }
            }
            None => {
                self.insert(id, new_subscription);
                OperationStatus::Ok(id)
            }
        }
    }

    fn delete(&mut self, id: &uuid::Uuid) {
        self.remove(id);
    }

    fn update(&mut self, subscription: S) {
        self.insert(subscription.id(), subscription);
    }
}
