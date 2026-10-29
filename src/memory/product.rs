use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::Error;
use crate::memory::env::Env;

/// Tariff product: what a customer bought. Stored next to the key and the
/// subscription, never inside the activation code.
///
/// `Product` is a separate axis from `PlanKind`: every product here is an
/// ordinary time-based `PlanKind::Standard` subscription. The product decides
/// which envs the subscription gets:
///
/// * [`Product::Standard`] — the classic set (`enabled_conns`, `wl`
///   included). An absent value (old rows, old requests) means exactly this,
///   so existing subscriptions keep the access they always had.
/// * [`Product::Gaming`] / [`Product::BsPrem`] — that single extra env only,
///   without the classic envs.
/// * [`Product::StandardGaming`] / [`Product::StandardBsPrem`] — the classic
///   set plus that one extra env.
/// * [`Product::Full`] — the classic set plus both extra envs (`wl`
///   included, because `wl` is part of the classic set).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Product {
    #[default]
    #[serde(rename = "standard")]
    Standard,
    #[serde(rename = "gaming")]
    Gaming,
    #[serde(rename = "bs-prem")]
    BsPrem,
    #[serde(rename = "standard-gaming")]
    StandardGaming,
    #[serde(rename = "standard-bs-prem")]
    StandardBsPrem,
    #[serde(rename = "full")]
    Full,
}

impl Product {
    /// The sellable products. `standard` is also the value of every row and
    /// request created before products existed.
    pub const ALL: [Product; 6] = [
        Product::Standard,
        Product::Gaming,
        Product::BsPrem,
        Product::StandardGaming,
        Product::StandardBsPrem,
        Product::Full,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Product::Standard => "standard",
            Product::Gaming => "gaming",
            Product::BsPrem => "bs-prem",
            Product::StandardGaming => "standard-gaming",
            Product::StandardBsPrem => "standard-bs-prem",
            Product::Full => "full",
        }
    }

    /// The classic product: the historical `enabled_conns` access and the
    /// default of every old row/request.
    pub fn is_standard(&self) -> bool {
        matches!(self, Product::Standard)
    }

    /// Whether the product includes the classic (`enabled_conns`) env set.
    /// False only for the single-env products.
    pub fn includes_standard(&self) -> bool {
        matches!(
            self,
            Product::Standard | Product::StandardGaming | Product::StandardBsPrem | Product::Full
        )
    }

    /// Extra envs of the product, on top of the classic set (see
    /// [`Product::includes_standard`]). The protocols for these envs come
    /// from `env_proto_templates` in the api config.
    pub fn extra_envs(&self) -> &'static [Env] {
        match self {
            Product::Standard => &[],
            Product::Gaming => &[Env::Gaming],
            Product::BsPrem => &[Env::BsPrem],
            Product::StandardGaming => &[Env::Gaming],
            Product::StandardBsPrem => &[Env::BsPrem],
            Product::Full => &[Env::Gaming, Env::BsPrem],
        }
    }
}

impl fmt::Display for Product {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lowercased, separators (`-`, `_`, spaces) stripped: `BS-Prem` and
/// `bsprem` become `bsprem`.
fn normalize(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect()
}

impl FromStr for Product {
    type Err = Error;

    /// Strict parse of the canonical kebab-case names (the compact spellings
    /// without separators are accepted too, case-insensitive). Old names are
    /// deliberately rejected here: the api must not silently accept a product
    /// that is no longer sold. Use [`Product::from_stored`] for stored values.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match normalize(s).as_str() {
            "standard" => Ok(Product::Standard),
            "gaming" => Ok(Product::Gaming),
            "bsprem" => Ok(Product::BsPrem),
            "standardgaming" => Ok(Product::StandardGaming),
            "standardbsprem" => Ok(Product::StandardBsPrem),
            "full" => Ok(Product::Full),
            _ => Err(Error::Custom(format!("Unknown product: {}", s))),
        }
    }
}

impl Product {
    /// Lenient parse for values coming out of the database: the pre-rename
    /// names mean the same product (`legacy`/`classic`/`dev` → `standard`,
    /// `dev-gaming` → `standard-gaming`, `dev-bs-prem` → `standard-bs-prem`).
    /// `None` for anything unknown — row readers fall back to the default.
    pub fn from_stored(s: &str) -> Option<Product> {
        match normalize(s).as_str() {
            "legacy" | "classic" | "dev" => Some(Product::Standard),
            "devgaming" => Some(Product::StandardGaming),
            "devbsprem" => Some(Product::StandardBsPrem),
            _ => Product::from_str(s).ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_standard() {
        assert_eq!(Product::default(), Product::Standard);
        assert!(Product::Standard.is_standard());
        assert!(!Product::Full.is_standard());
    }

    #[test]
    fn names_round_trip() {
        for product in Product::ALL {
            let s = product.to_string();
            assert_eq!(Product::from_str(&s).unwrap(), product, "name {}", s);
        }
    }

    #[test]
    fn accepts_compact_and_case() {
        assert_eq!(Product::from_str("BS-PREM").unwrap(), Product::BsPrem);
        assert_eq!(Product::from_str("bsprem").unwrap(), Product::BsPrem);
        assert_eq!(
            Product::from_str("Standard-Gaming").unwrap(),
            Product::StandardGaming
        );
        assert_eq!(
            Product::from_str("standardgaming").unwrap(),
            Product::StandardGaming
        );
        assert_eq!(
            Product::from_str("standardbsprem").unwrap(),
            Product::StandardBsPrem
        );
        assert_eq!(Product::from_str(" Standard ").unwrap(), Product::Standard);
    }

    #[test]
    fn rejects_unknown_and_old_names() {
        // Old/removed names must not be accepted as new sales.
        assert!(Product::from_str("legacy").is_err());
        assert!(Product::from_str("classic").is_err());
        assert!(Product::from_str("dev").is_err());
        assert!(Product::from_str("dev-gaming").is_err());
        assert!(Product::from_str("dev-bs-prem").is_err());
        // Freely combined envs are not products.
        assert!(Product::from_str("gaming-bs-prem").is_err());
        assert!(Product::from_str("").is_err());
        assert!(Product::from_str("premium").is_err());
    }

    #[test]
    fn stored_aliases_map_to_the_renamed_products() {
        assert_eq!(Product::from_stored("legacy"), Some(Product::Standard));
        assert_eq!(Product::from_stored("classic"), Some(Product::Standard));
        assert_eq!(Product::from_stored("dev"), Some(Product::Standard));
        assert_eq!(
            Product::from_stored("dev-gaming"),
            Some(Product::StandardGaming)
        );
        assert_eq!(
            Product::from_stored("dev-bs-prem"),
            Some(Product::StandardBsPrem)
        );
        assert_eq!(Product::from_stored("gaming"), Some(Product::Gaming));
        assert_eq!(Product::from_stored("nonsense"), None);
    }

    /// The product → env contract, in one place: which products include the
    /// classic set and which extra envs they add.
    #[test]
    fn product_to_env_mapping() {
        let expected: [(Product, bool, &[Env]); 6] = [
            (Product::Standard, true, &[]),
            (Product::Gaming, false, &[Env::Gaming]),
            (Product::BsPrem, false, &[Env::BsPrem]),
            (Product::StandardGaming, true, &[Env::Gaming]),
            (Product::StandardBsPrem, true, &[Env::BsPrem]),
            (Product::Full, true, &[Env::Gaming, Env::BsPrem]),
        ];

        for (product, includes_standard, extra_envs) in expected {
            assert_eq!(
                product.includes_standard(),
                includes_standard,
                "includes_standard of {}",
                product
            );
            assert_eq!(
                product.extra_envs(),
                extra_envs,
                "extra_envs of {}",
                product
            );
        }
    }

    #[test]
    fn serde_uses_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Product::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(
            serde_json::to_string(&Product::BsPrem).unwrap(),
            "\"bs-prem\""
        );
        assert_eq!(
            serde_json::to_string(&Product::StandardGaming).unwrap(),
            "\"standard-gaming\""
        );
        let parsed: Product = serde_json::from_str("\"standard-bs-prem\"").unwrap();
        assert_eq!(parsed, Product::StandardBsPrem);

        // An absent product (old payloads) deserializes to the default.
        #[derive(serde::Deserialize)]
        struct Holder {
            #[serde(default)]
            product: Product,
        }
        let holder: Holder = serde_json::from_str("{}").unwrap();
        assert_eq!(holder.product, Product::Standard);
    }
}
