use std::collections::HashMap;

use fcore::{Env, PlanKind, Product, Tag};

/// Why the entitlements of a product could not be resolved. This is a
/// **configuration** error: an env added by a product has no protocol
/// template. It is never a fallback — silently dropping the env would hand
/// the customer a subscription without the access they paid for, and
/// silently adding `enabled_conns` is not what a single-env product sells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingTemplate {
    pub product: Product,
    pub env: Env,
}

impl std::fmt::Display for MissingTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "product '{}' adds env '{}', but env_proto_templates has no (non-empty) template \
             for it",
            self.product, self.env
        )
    }
}

impl std::error::Error for MissingTemplate {}

/// Decides which shared `(env, proto)` connections a subscription gets.
///
/// The product contract (see [`Product`]) is additive for the classic set:
///
/// * `legacy`-era behaviour is [`Product::Standard`] → `enabled_conns`
///   (`wl` included), exactly what old subscriptions had;
/// * a single-env product (`gaming`, `bs-prem`) → that env only;
/// * a standard+extra product → `enabled_conns` plus the extra env(s);
/// * `lite` (traffic-only) → `lite_enabled_conns`, falling back to
///   `enabled_conns` as before;
/// * `selfhosted` (and its child users) → nothing: such an account only
///   holds the nodes it registered itself.
///
/// Extra envs are served by `env_proto_templates`; a missing or empty
/// template is an error, not a fallback.
#[derive(Debug, Clone, Default)]
pub struct ConnResolver {
    pub enabled_conns: Option<HashMap<Env, Vec<Tag>>>,
    pub lite_enabled_conns: Option<HashMap<Env, Vec<Tag>>>,
    /// env → protocols for the envs added by the products.
    pub env_proto_templates: HashMap<Env, Vec<Tag>>,
}

impl ConnResolver {
    /// The map of `(env → tags)` this plan/product pair is entitled to.
    ///
    /// `Ok(None)` means "no shared connections" (selfhosted accounts, or a
    /// configuration that resolves to nothing), not an error.
    pub fn resolve(
        &self,
        plan_kind: PlanKind,
        product: Product,
    ) -> Result<Option<HashMap<Env, Vec<Tag>>>, MissingTemplate> {
        match plan_kind {
            PlanKind::Selfhosted => Ok(None),
            PlanKind::Lite => Ok(self
                .lite_enabled_conns
                .clone()
                .or_else(|| self.enabled_conns.clone())),
            PlanKind::Premium | PlanKind::Standard => {
                let mut resolved: HashMap<Env, Vec<Tag>> = if product.includes_standard() {
                    self.enabled_conns.clone().unwrap_or_default()
                } else {
                    HashMap::new()
                };

                for env in product.extra_envs() {
                    let mut tags = self
                        .env_proto_templates
                        .get(env)
                        .filter(|tags| !tags.is_empty())
                        .cloned()
                        .ok_or(MissingTemplate {
                            product,
                            env: env.clone(),
                        })?;
                    // A template listing the same proto twice must not make
                    // the ensure step try to create the pair twice.
                    tags.dedup();
                    resolved.insert(env.clone(), tags);
                }

                if resolved.is_empty() {
                    return Ok(None);
                }

                Ok(Some(resolved))
            }
        }
    }

    /// Same as [`ConnResolver::resolve`], for a subscription-like value.
    pub fn resolve_for<S: fcore::SubscriptionOperations>(
        &self,
        sub: &S,
    ) -> Result<Option<HashMap<Env, Vec<Tag>>>, MissingTemplate> {
        self.resolve(sub.plan_kind(), sub.product())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcore::{Product, Subscription, SubscriptionOperations};

    fn classic() -> HashMap<Env, Vec<Tag>> {
        let mut map = HashMap::new();
        map.insert(Env::Dev, vec![Tag::Hysteria2]);
        map.insert(Env::Ru, vec![Tag::Wireguard, Tag::Hysteria2]);
        map.insert(Env::Wl, vec![Tag::VlessXhttpCdn]);
        map
    }

    fn resolver() -> ConnResolver {
        let mut lite = HashMap::new();
        lite.insert(Env::Ru, vec![Tag::VlessTcpReality]);

        let mut templates = HashMap::new();
        templates.insert(Env::Gaming, vec![Tag::AmneziaWg, Tag::AmneziaWgMobile]);
        templates.insert(Env::BsPrem, vec![Tag::AmneziaWg]);

        ConnResolver {
            enabled_conns: Some(classic()),
            lite_enabled_conns: Some(lite),
            env_proto_templates: templates,
        }
    }

    fn envs(resolved: &HashMap<Env, Vec<Tag>>) -> Vec<String> {
        let mut list: Vec<String> = resolved.keys().map(|e| e.to_string()).collect();
        list.sort();
        list
    }

    /// Gate: the classic set is byte-for-byte what `enabled_conns` says —
    /// old subscriptions must not change.
    #[test]
    fn standard_equals_enabled_conns() {
        let resolver = resolver();
        let resolved = resolver
            .resolve(PlanKind::Standard, Product::Standard)
            .unwrap()
            .unwrap();

        assert_eq!(resolved, classic());
        assert_eq!(resolved[&Env::Wl], vec![Tag::VlessXhttpCdn]);
    }

    #[test]
    fn gaming_is_gaming_only() {
        let resolver = resolver();
        let resolved = resolver
            .resolve(PlanKind::Standard, Product::Gaming)
            .unwrap()
            .unwrap();

        assert_eq!(envs(&resolved), vec!["gaming".to_string()]);
        assert_eq!(
            resolved[&Env::Gaming],
            vec![Tag::AmneziaWg, Tag::AmneziaWgMobile]
        );
        // No classic envs, so no wl, dev or ru.
        assert!(!resolved.contains_key(&Env::Wl));
        assert!(!resolved.contains_key(&Env::Dev));
        assert!(!resolved.contains_key(&Env::Ru));
    }

    #[test]
    fn bs_prem_is_bs_prem_only() {
        let resolver = resolver();
        let resolved = resolver
            .resolve(PlanKind::Standard, Product::BsPrem)
            .unwrap()
            .unwrap();

        assert_eq!(envs(&resolved), vec!["bs-prem".to_string()]);
        assert!(!resolved.contains_key(&Env::Wl));
    }

    #[test]
    fn standard_gaming_is_classic_plus_gaming() {
        let resolver = resolver();
        let resolved = resolver
            .resolve(PlanKind::Standard, Product::StandardGaming)
            .unwrap()
            .unwrap();

        assert_eq!(
            envs(&resolved),
            vec![
                "dev".to_string(),
                "gaming".to_string(),
                "ru".to_string(),
                "wl".to_string()
            ]
        );
        assert_eq!(resolved[&Env::Dev], classic()[&Env::Dev]);
        assert!(resolved.contains_key(&Env::Wl));
    }

    #[test]
    fn standard_bs_prem_is_classic_plus_bs_prem() {
        let resolver = resolver();
        let resolved = resolver
            .resolve(PlanKind::Standard, Product::StandardBsPrem)
            .unwrap()
            .unwrap();

        assert_eq!(
            envs(&resolved),
            vec![
                "bs-prem".to_string(),
                "dev".to_string(),
                "ru".to_string(),
                "wl".to_string()
            ]
        );
        assert!(resolved.contains_key(&Env::Wl));
        assert!(!resolved.contains_key(&Env::Gaming));
    }

    #[test]
    fn full_is_classic_plus_both_including_wl() {
        let resolver = resolver();
        let resolved = resolver
            .resolve(PlanKind::Standard, Product::Full)
            .unwrap()
            .unwrap();

        assert_eq!(
            envs(&resolved),
            vec![
                "bs-prem".to_string(),
                "dev".to_string(),
                "gaming".to_string(),
                "ru".to_string(),
                "wl".to_string()
            ]
        );
        assert!(resolved.contains_key(&Env::Wl));
    }

    #[test]
    fn lite_uses_lite_map_and_falls_back() {
        let mut resolver = resolver();
        let resolved = resolver
            .resolve(PlanKind::Lite, Product::Standard)
            .unwrap()
            .unwrap();
        assert_eq!(envs(&resolved), vec!["ru".to_string()]);

        // No dedicated lite map: the classic set is used, as before.
        resolver.lite_enabled_conns = None;
        let resolved = resolver
            .resolve(PlanKind::Lite, Product::Standard)
            .unwrap()
            .unwrap();
        assert_eq!(resolved, classic());
        assert!(resolved.contains_key(&Env::Wl));
    }

    #[test]
    fn selfhosted_gets_no_shared_connections() {
        let resolver = resolver();
        assert!(resolver
            .resolve(PlanKind::Selfhosted, Product::Standard)
            .unwrap()
            .is_none());
        assert!(resolver
            .resolve(PlanKind::Selfhosted, Product::Full)
            .unwrap()
            .is_none());
    }

    #[test]
    fn missing_or_empty_template_is_an_error_not_a_fallback() {
        let mut resolver = resolver();
        resolver.env_proto_templates.remove(&Env::Gaming);

        let err = resolver
            .resolve(PlanKind::Standard, Product::Gaming)
            .unwrap_err();
        assert_eq!(
            err,
            MissingTemplate {
                product: Product::Gaming,
                env: Env::Gaming
            }
        );

        // It fails for the classic-plus-extra products too: the partial set
        // must never be handed out silently.
        let err = resolver
            .resolve(PlanKind::Standard, Product::Full)
            .unwrap_err();
        assert_eq!(err.env, Env::Gaming);

        resolver.env_proto_templates.insert(Env::Gaming, vec![]);
        assert!(resolver
            .resolve(PlanKind::Standard, Product::StandardGaming)
            .is_err());
    }

    #[test]
    fn classic_only_config_serves_all_products() {
        // Gaming/bs-prem templates missing must not break the classic
        // product: deployments without the extra envs still work.
        let resolver = ConnResolver {
            enabled_conns: Some(classic()),
            lite_enabled_conns: None,
            env_proto_templates: HashMap::new(),
        };

        assert_eq!(
            resolver
                .resolve(PlanKind::Standard, Product::Standard)
                .unwrap()
                .unwrap(),
            classic()
        );
        assert!(resolver
            .resolve(PlanKind::Standard, Product::Gaming)
            .is_err());
    }

    #[test]
    fn resolve_for_subscription_uses_its_product() {
        let resolver = resolver();

        let standard = Subscription::default();
        assert_eq!(resolver.resolve_for(&standard).unwrap().unwrap(), classic());

        let mut sub = Subscription::default();
        sub.set_product(Product::Gaming);
        let resolved = resolver.resolve_for(&sub).unwrap().unwrap();
        assert_eq!(envs(&resolved), vec!["gaming".to_string()]);

        // A child user of a selfhosted account gets nothing, whatever the
        // product column says.
        let child = Subscription::new_user_child(uuid::Uuid::new_v4());
        assert!(resolver.resolve_for(&child).unwrap().is_none());
    }

    #[test]
    fn duplicate_template_tags_are_deduped() {
        let mut resolver = resolver();
        resolver
            .env_proto_templates
            .insert(Env::Gaming, vec![Tag::AmneziaWg, Tag::AmneziaWg]);

        let resolved = resolver
            .resolve(PlanKind::Standard, Product::Gaming)
            .unwrap()
            .unwrap();
        assert_eq!(resolved[&Env::Gaming], vec![Tag::AmneziaWg]);
    }
}
