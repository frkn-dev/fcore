//! Peer identity on the node: `(public_key, address)`.
//!
//! The api hands out one tunnel address per pool, and the same device key may
//! own an address in several pools of one env. Keying the node's peer mirror
//! by the public key alone collapsed them: the last message won the map, the
//! interface kept the allowed-ip of whichever pool was applied first, and the
//! client that received the other address came up with no traffic at all
//! ("connected, no internet").
//!
//! Everything here is pure: the mirror bookkeeping and the decision of what the
//! interface needs for one key. The netlink/defguard calls live in `tasks.rs`,
//! so the behaviour can be tested without a kernel.

// Without a peer-plane interface (neither `amnezia-wg` nor `wireguard`
// compiled in) only the mirror bookkeeping is live; the rest is covered by the
// tests in this module.
#![cfg_attr(
    not(any(feature = "amnezia-wg", feature = "wireguard")),
    allow(dead_code)
)]

use std::collections::{BTreeMap, BTreeSet};

use fcore::{Env, PeerIdentity, PeerMirror, SnapshotPeer, Tag};

/// Bus topic of this node's live peer stream.
///
/// One expression for both the subscription and the filter: a node with a pool
/// listens on `peers-update-<env>-<pool>`, a node without one on the env-wide
/// `peers-update-<env>`. The string is compared whole — see the parser note in
/// `zmq::topic` for why the halves cannot be split reliably.
pub(crate) fn peers_topic(env: Env, pool: Option<&str>) -> String {
    fcore::Topic::PeersUpdates(env, pool.map(str::to_string)).to_string()
}

/// Address without the prefix: the form the api sends and the mirror stores.
pub(crate) fn host(address: &str) -> &str {
    address.split('/').next().unwrap_or(address).trim()
}

/// Every address the interface must allow for one key of one tag.
///
/// The tag matters: a key may hold an AmneziaWG peer in one pool and a
/// WireGuard peer in another, and those live on different interfaces.
pub(crate) fn desired(mirror: &PeerMirror, tag: Tag, public_key: &str) -> BTreeSet<String> {
    collect(mirror, tag, public_key, None)
}

/// [`desired`] without the entries of one peer row.
///
/// A Create/Update re-applies a single row: when the api moved it to another
/// address, the old entry must not survive, and the other pools of the same
/// key must not be touched.
pub(crate) fn desired_except(
    mirror: &PeerMirror,
    tag: Tag,
    public_key: &str,
    peer_id: uuid::Uuid,
) -> BTreeSet<String> {
    collect(mirror, tag, public_key, Some(peer_id))
}

fn collect(
    mirror: &PeerMirror,
    tag: Tag,
    public_key: &str,
    skip_peer_id: Option<uuid::Uuid>,
) -> BTreeSet<String> {
    mirror
        .values()
        .filter(|peer| peer.tag == tag && peer.public_key == public_key)
        .filter(|peer| skip_peer_id != Some(peer.peer_id))
        .map(|peer| host(&peer.address).to_string())
        .collect()
}

/// The mirror as `public key -> allowed addresses`, for one interface.
pub(crate) fn desired_by_key(mirror: &PeerMirror, tag: Tag) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for peer in mirror.values().filter(|peer| peer.tag == tag) {
        out.entry(peer.public_key.clone())
            .or_default()
            .insert(host(&peer.address).to_string());
    }
    out
}

/// What an interface is missing, holds wrong, or should not hold.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PeerDiff {
    /// Keys the mirror wants and the interface does not have.
    pub add: Vec<String>,
    /// Keys on both sides with a different set of addresses.
    pub update: Vec<String>,
    /// Keys on the interface that the mirror does not hold.
    pub remove: Vec<String>,
}

impl PeerDiff {
    pub(crate) fn is_noop(&self) -> bool {
        self.add.is_empty() && self.update.is_empty() && self.remove.is_empty()
    }
}

/// Compare a whole interface with the authoritative set.
///
/// Used at startup and after an authoritative `peers-init`: applying live
/// messages one by one cannot heal a message the node never heard.
pub(crate) fn diff(
    current: &BTreeMap<String, BTreeSet<String>>,
    desired: &BTreeMap<String, BTreeSet<String>>,
) -> PeerDiff {
    let mut out = PeerDiff::default();
    for (key, wanted) in desired {
        match current.get(key) {
            None => out.add.push(key.clone()),
            Some(held) if held != wanted => out.update.push(key.clone()),
            Some(_) => {}
        }
    }
    for key in current.keys() {
        if !desired.contains_key(key) {
            out.remove.push(key.clone());
        }
    }
    out
}

/// Write one peer into the mirror.
///
/// The identity is `(public_key, address)`, so a key keeps one entry per pool.
/// An entry of the same peer row under another address is dropped first: the
/// row is the same tunnel, it only moved.
pub(crate) fn insert(mirror: &mut PeerMirror, peer: SnapshotPeer) {
    let identity = peer.identity();
    mirror.retain(|existing_id, existing| {
        existing.peer_id != peer.peer_id || *existing_id == identity
    });
    mirror.insert(identity, peer);
}

/// Drop exactly one `(public_key, address)` pair.
pub(crate) fn remove(mirror: &mut PeerMirror, identity: &PeerIdentity) -> Option<SnapshotPeer> {
    mirror.remove(identity)
}

/// What the interface needs for one key, given what it already allows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AllowedIpsChange {
    /// Already exact: leave the peer alone (a re-apply must not reset the
    /// handshake or the counters).
    Keep,
    /// Addresses to append; the peer keeps its session.
    Add(Vec<String>),
    /// Exact set to install in place of the current one.
    Set(Vec<String>),
    /// No address left for this key.
    Remove,
}

/// Compare the interface against the mirror.
pub(crate) fn plan(current: &BTreeSet<String>, desired: &BTreeSet<String>) -> AllowedIpsChange {
    if current == desired {
        return AllowedIpsChange::Keep;
    }
    if desired.is_empty() {
        return AllowedIpsChange::Remove;
    }
    if current.is_subset(desired) {
        // Only additions: appending keeps the peer and its session.
        return AllowedIpsChange::Add(desired.difference(current).cloned().collect());
    }
    AllowedIpsChange::Set(desired.iter().cloned().collect())
}

#[cfg(feature = "amnezia-wg")]
pub(crate) fn awg_peer_key(
    peer: &netlink_packet_amnezia_wireguard::AmneziaWireguardPeer,
) -> Option<[u8; 32]> {
    peer.0.iter().find_map(|attr| match attr {
        netlink_packet_amnezia_wireguard::AmneziaWireguardPeerAttribute::PublicKey(key) => {
            Some(*key)
        }
        _ => None,
    })
}

/// Allowed addresses of a kernel peer, as bare hosts.
///
/// The node only ever installs `/32` hosts, so the mask is not part of the
/// comparison; anything else on the interface is treated as an address of the
/// same key and would be replaced, which is what the api's full list means.
#[cfg(feature = "amnezia-wg")]
pub(crate) fn awg_allowed_hosts(
    peer: &netlink_packet_amnezia_wireguard::AmneziaWireguardPeer,
) -> BTreeSet<String> {
    use netlink_packet_amnezia_wireguard::AmneziaWireguardPeerAttribute;

    let mut out = BTreeSet::new();
    for attr in &peer.0 {
        if let AmneziaWireguardPeerAttribute::AllowedIps(ips) = attr {
            for ip in ips.iter() {
                let address = ip.0.iter().find_map(|attr| match attr {
                    netlink_packet_amnezia_wireguard::AmneziaWireguardAllowedIpAttr::IpAddr(
                        address,
                    ) => Some(*address),
                    _ => None,
                });
                if let Some(address) = address {
                    out.insert(address.to_string());
                }
            }
        }
    }
    out
}

/// Build the netlink attribute set for one peer.
///
/// `replace` makes the kernel swap the allowed-ips list instead of appending to
/// it; that is how one address of a key is removed while the key itself stays.
#[cfg(feature = "amnezia-wg")]
pub(crate) fn awg_peer_with_ips(
    key: [u8; 32],
    addresses: &[String],
    replace: bool,
) -> Result<netlink_packet_amnezia_wireguard::AmneziaWireguardPeer, String> {
    use netlink_packet_amnezia_wireguard::constants::WGPEER_F_REPLACE_ALLOWEDIPS;
    use netlink_packet_amnezia_wireguard::{AmneziaWireguardPeer, AmneziaWireguardPeerAttribute};

    let ips = addresses
        .iter()
        .map(|address| {
            address
                .parse::<std::net::IpAddr>()
                .map(fcore::IpAddrMask::host)
                .map_err(|_| format!("invalid peer address '{address}'"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut attrs = vec![
        AmneziaWireguardPeerAttribute::PublicKey(key),
        AmneziaWireguardPeerAttribute::AllowedIps(ips.into_iter().map(Into::into).collect()),
    ];
    if replace {
        attrs.push(AmneziaWireguardPeerAttribute::Flags(
            WGPEER_F_REPLACE_ALLOWEDIPS,
        ));
    }
    Ok(AmneziaWireguardPeer(attrs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// The key of the live prod incident: one device, an address in the EU pool
    /// and another in the US pool of the same env.
    const KEY: &str = "P3Sks4Q0dYy8h1jY0m9W0n0o0p0q0r0s0t0u0v0w0x0=";
    const EU: &str = "10.88.0.2";
    const US: &str = "10.88.0.12";

    fn peer(peer_id: u128, public_key: &str, address: &str, tag: Tag) -> SnapshotPeer {
        SnapshotPeer {
            peer_id: uuid::Uuid::from_u128(peer_id),
            subscription_id: Some(uuid::Uuid::from_u128(99)),
            tag,
            public_key: public_key.to_string(),
            address: address.to_string(),
        }
    }

    fn set(addresses: &[&str]) -> BTreeSet<String> {
        addresses.iter().map(|a| a.to_string()).collect()
    }

    fn addresses(mirror: &PeerMirror, tag: Tag) -> Vec<String> {
        mirror
            .values()
            .filter(|p| p.tag == tag)
            .map(|p| p.address.clone())
            .collect()
    }

    #[test]
    fn one_key_holds_an_address_per_pool() {
        let mut mirror = PeerMirror::new();

        // EU pool first: the interface gets that one address.
        mirror.insert(
            PeerIdentity::new(KEY, EU),
            peer(1, KEY, EU, Tag::AmneziaPeer),
        );
        let first = desired(&mirror, Tag::AmneziaPeer, KEY);
        assert_eq!(first, set(&[EU]));
        assert_eq!(
            plan(&set(&[]), &first),
            AllowedIpsChange::Add(vec![EU.to_string()])
        );

        // US pool of the same key arrives: the mirror keeps both and the
        // interface only needs the new address appended.
        insert(&mut mirror, peer(2, KEY, US, Tag::AmneziaPeer));

        assert_eq!(mirror.len(), 2);
        assert_eq!(
            addresses(&mirror, Tag::AmneziaPeer)
                .into_iter()
                .collect::<BTreeSet<_>>(),
            set(&[EU, US])
        );

        let wanted = desired(&mirror, Tag::AmneziaPeer, KEY);
        assert_eq!(wanted, set(&[EU, US]));
        assert_eq!(
            plan(&set(&[EU]), &wanted),
            AllowedIpsChange::Add(vec![US.to_string()]),
            "the second pool must add its own allowed-ip, not replace the first"
        );
        // And once both are on the interface a re-apply changes nothing.
        assert_eq!(plan(&set(&[EU, US]), &wanted), AllowedIpsChange::Keep);
    }

    #[test]
    fn deleting_one_pool_keeps_the_other_address() {
        let mut mirror = PeerMirror::new();
        insert(&mut mirror, peer(1, KEY, EU, Tag::AmneziaPeer));
        insert(&mut mirror, peer(2, KEY, US, Tag::AmneziaPeer));

        // The EU tunnel is revoked: drop exactly (KEY, EU).
        let removed = remove(&mut mirror, &PeerIdentity::new(KEY, EU));
        assert!(removed.is_some());
        assert_eq!(addresses(&mirror, Tag::AmneziaPeer), vec![US.to_string()]);

        // The interface still has both, so it must end up with US only - and
        // the key itself must stay (US is still a live tunnel).
        let wanted = desired(&mirror, Tag::AmneziaPeer, KEY);
        assert_eq!(
            plan(&set(&[EU, US]), &wanted),
            AllowedIpsChange::Set(vec![US.to_string()])
        );
        assert_ne!(
            plan(&set(&[EU, US]), &wanted),
            AllowedIpsChange::Remove,
            "a different pool's address must not be dropped with the revoked one"
        );

        // Dropping the last address of the key is what removes the peer.
        remove(&mut mirror, &PeerIdentity::new(KEY, US));
        assert_eq!(
            plan(&set(&[US]), &desired(&mirror, Tag::AmneziaPeer, KEY)),
            AllowedIpsChange::Remove
        );
    }

    #[test]
    fn same_key_reapplied_with_a_new_address_updates_that_entry_only() {
        let mut mirror = PeerMirror::new();
        insert(&mut mirror, peer(1, KEY, EU, Tag::AmneziaPeer));
        insert(&mut mirror, peer(2, KEY, US, Tag::AmneziaPeer));

        // The api moves peer row 1 (the EU one) to another address.
        let moved = peer(1, KEY, "10.88.0.30", Tag::AmneziaPeer);
        insert(&mut mirror, moved.clone());

        assert_eq!(mirror.len(), 2, "one address per pool, not a third entry");
        assert!(!mirror.contains_key(&PeerIdentity::new(KEY, EU)));
        assert_eq!(
            mirror
                .get(&PeerIdentity::new(KEY, "10.88.0.30"))
                .map(|p| p.peer_id),
            Some(uuid::Uuid::from_u128(1))
        );
        assert_eq!(
            mirror.get(&PeerIdentity::new(KEY, US)).map(|p| p.peer_id),
            Some(uuid::Uuid::from_u128(2)),
            "the other pool's peer must be untouched"
        );

        // The update path sees the wanted set without the old address of that
        // row but with the other pool's address.
        let wanted = {
            let mut wanted =
                desired_except(&mirror, Tag::AmneziaPeer, KEY, uuid::Uuid::from_u128(1));
            wanted.insert(host(&moved.address).to_string());
            wanted
        };
        assert_eq!(wanted, set(&["10.88.0.30", US]));
        assert_eq!(
            plan(&set(&[EU, US]), &wanted),
            AllowedIpsChange::Set(vec![US.to_string(), "10.88.0.30".to_string()])
        );
    }

    #[test]
    fn tags_are_kept_apart() {
        // The same key and address on two interfaces must not merge: one is the
        // AmneziaWG peer plane, the other WireGuard's.
        let mut mirror = PeerMirror::new();
        insert(&mut mirror, peer(1, KEY, EU, Tag::AmneziaPeer));
        insert(&mut mirror, peer(2, KEY, US, Tag::WireguardPeer));

        assert_eq!(desired(&mirror, Tag::AmneziaPeer, KEY), set(&[EU]));
        assert_eq!(desired(&mirror, Tag::WireguardPeer, KEY), set(&[US]));
    }

    #[test]
    fn reconcile_diff_names_added_changed_and_removed_keys() {
        let current: BTreeMap<String, BTreeSet<String>> = [
            // The live incident: the key is on the interface with the EU
            // address while the mirror (and the client) has the US one.
            ("P3Sks".to_string(), set(&[EU])),
            ("GONE".to_string(), set(&["10.88.0.7"])),
        ]
        .into_iter()
        .collect();
        let desired: BTreeMap<String, BTreeSet<String>> = [
            ("P3Sks".to_string(), set(&[US])),
            ("NEW".to_string(), set(&["10.88.0.50"])),
        ]
        .into_iter()
        .collect();

        let change = diff(&current, &desired);
        assert_eq!(change.add, vec!["NEW".to_string()]);
        assert_eq!(change.update, vec!["P3Sks".to_string()]);
        assert_eq!(change.remove, vec!["GONE".to_string()]);
        assert!(!change.is_noop());

        // A second pass over the reconciled interface is a no-op: a startup or
        // `peers-init` reconcile must not rewrite live peers every time.
        let idempotent = diff(&desired, &desired);
        assert!(idempotent.is_noop());
    }

    #[test]
    fn reconcile_diff_on_an_empty_interface_adds_every_key() {
        // The owner's repro: snapshot deleted, interface empty, api has the
        // set. Everything is an add, nothing is a change or a removal.
        let desired: BTreeMap<String, BTreeSet<String>> = [
            ("K1".to_string(), set(&[EU])),
            ("K2".to_string(), set(&[US, "10.88.0.13"])),
        ]
        .into_iter()
        .collect();

        let change = diff(&BTreeMap::new(), &desired);
        assert_eq!(change.add, vec!["K1".to_string(), "K2".to_string()]);
        assert!(change.update.is_empty());
        assert!(change.remove.is_empty());
    }

    #[test]
    fn desired_by_key_groups_addresses_per_tag() {
        let mut mirror = PeerMirror::new();
        insert(&mut mirror, peer(1, KEY, EU, Tag::AmneziaPeer));
        insert(&mut mirror, peer(2, KEY, US, Tag::AmneziaPeer));
        insert(&mut mirror, peer(3, KEY, "10.88.0.40", Tag::WireguardPeer));

        let awg = desired_by_key(&mirror, Tag::AmneziaPeer);
        assert_eq!(awg.get(KEY), Some(&set(&[EU, US])));
        assert_eq!(desired_by_key(&mirror, Tag::WireguardPeer).len(), 1);
    }

    #[test]
    fn a_node_listens_on_its_own_pool_topic_only() {
        let env = Env::from_str("0001").expect("valid shard id");
        let us = peers_topic(env.clone(), Some("us"));
        let eu = peers_topic(env.clone(), Some("eu"));
        let env_wide = peers_topic(env, None);

        assert_eq!(us, "peers-update-0001-us");
        assert_eq!(eu, "peers-update-0001-eu");
        assert_eq!(env_wide, "peers-update-0001");
        assert_ne!(us, eu, "a pool node must not hear another pool's changes");
        assert_ne!(us, env_wide);
    }

    #[test]
    fn host_strips_the_prefix() {
        assert_eq!(host("10.88.0.12/32"), "10.88.0.12");
        assert_eq!(host("10.88.0.12"), "10.88.0.12");
        assert_eq!(host(" 10.88.0.12 "), "10.88.0.12");
    }
}
