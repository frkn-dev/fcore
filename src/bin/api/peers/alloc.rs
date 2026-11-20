//! Address allocation for peers.
//!
//! The api decides which address a peer gets; nodes never do. The allocation
//! itself is pure: the handler fetches the blocks of the env and the set of
//! addresses already in use, and this module picks the first free one. The
//! unique index in Postgres is the fuse for two api instances racing — the
//! loser simply asks for the next address.

use std::collections::HashSet;
use std::net::Ipv4Addr;

use fcore::IpAddrMask;

/// First host of a block belongs to the node's own interface (`awg0-peer`
/// holds `.1`), so allocation starts at `.2`.
const FIRST_HOST_OFFSET: u32 = 2;

/// Highest address we hand out inside a block: `broadcast - 1`.
fn last_host(base: u32, cidr: u8) -> Option<u32> {
    if cidr >= 32 {
        return None;
    }
    let size = 1u32 << (32 - cidr);
    base.checked_add(size - 2)
}

fn network_of(block: &IpAddrMask) -> Option<u32> {
    let base = block.as_ipv4()?;
    let mask = if block.cidr == 0 {
        0
    } else {
        u32::MAX << (32 - block.cidr)
    };
    Some(u32::from(base) & mask)
}

/// First address of `blocks` that is neither reserved nor in `used`.
///
/// Blocks are tried in the order they were stored, so a small env keeps using
/// its first block until it is full and only then spills into the next one.
pub fn first_free(blocks: &[IpAddrMask], used: &HashSet<Ipv4Addr>) -> Option<Ipv4Addr> {
    for block in blocks {
        let Some(base) = network_of(block) else {
            continue;
        };
        let Some(last) = last_host(base, block.cidr) else {
            continue;
        };
        let start = base.checked_add(FIRST_HOST_OFFSET)?;
        if start > last {
            continue;
        }
        for raw in start..=last {
            let candidate = Ipv4Addr::from(raw);
            if !used.contains(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Whether two blocks share at least one address.
pub fn overlaps(a: &IpAddrMask, b: &IpAddrMask) -> bool {
    let (Some(a_net), Some(b_net)) = (network_of(a), network_of(b)) else {
        return false;
    };
    let prefix = a.cidr.min(b.cidr);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (a_net & mask) == (b_net & mask)
}

/// First free `prefix`-sized block inside `pool`.
///
/// Used where the number of shards is not known in advance (premium slots, a
/// customer's own machine): nobody wants to keep a live list of those subnets in
/// the api config, so the api carves a block out of a reserved supernet on first
/// use. `taken` is every block the database already holds — the allocator is a
/// pure function; the unique/exclude constraint on `env_networks` is the fuse.
pub fn next_block(pool: &IpAddrMask, taken: &[IpAddrMask], prefix: u8) -> Option<IpAddrMask> {
    let base = network_of(pool)?;
    if prefix < pool.cidr || prefix > 32 {
        return None;
    }
    let step = 1u32 << (32 - prefix);
    let count = 1u32 << (prefix - pool.cidr);

    for index in 0..count {
        let net = base.checked_add(index * step)?;
        let candidate = IpAddrMask::new(
            std::net::IpAddr::V4(Ipv4Addr::from(net)),
            prefix,
        );
        if !taken.iter().any(|used| overlaps(used, &candidate)) {
            return Some(candidate);
        }
    }

    None
}

/// Share of the address budget we refuse to hand out. A revoke frees its
/// address for the next request, but a burst of re-issues (card switch,
/// reinstall) must not race into the last host of the block.
pub const RESERVE_PERCENT: u64 = 10;

/// Hosts of one block that can carry a peer: network and broadcast are not
/// peers, and `.1` is the node's own interface (`awg0-peer`).
pub fn block_hosts(block: &IpAddrMask) -> u64 {
    let Some(base) = network_of(block) else {
        return 0;
    };
    let Some(last) = last_host(base, block.cidr) else {
        return 0;
    };
    let Some(first) = base.checked_add(FIRST_HOST_OFFSET) else {
        return 0;
    };
    if first > last {
        return 0;
    }
    u64::from(last - first) + 1
}

/// Every usable host of every block of an env.
pub fn total_hosts(blocks: &[IpAddrMask]) -> u64 {
    usable_blocks(blocks).iter().map(block_hosts).sum()
}

/// The address budget of an env: all hosts minus the reserve. This is the real
/// capacity of a shard — peers are what runs out, not subscription rows.
pub fn allocatable(blocks: &[IpAddrMask]) -> u64 {
    total_hosts(blocks) * (100 - RESERVE_PERCENT) / 100
}

/// Blocks that a peer of this env may use, given everything already stored.
/// Kept separate so the handler can log a precise reason when nothing is free.
pub fn usable_blocks(blocks: &[IpAddrMask]) -> Vec<IpAddrMask> {
    blocks
        .iter()
        .filter(|b| b.as_ipv4().is_some() && b.cidr < 32)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn block(cidr: &str) -> IpAddrMask {
        IpAddrMask::from_str(cidr).expect("valid block")
    }

    #[test]
    fn auto_blocks_are_carved_out_of_the_pool() {
        let pool = block("10.128.0.0/9");
        // Nothing taken yet: the first /16 of the pool.
        assert_eq!(
            next_block(&pool, &[], 16).map(|b| b.to_string()),
            Some("10.128.0.0/16".to_string())
        );
        // A taken /24 inside the first /16 pushes the next shard to the next /16.
        let taken = vec![block("10.128.7.0/24")];
        assert_eq!(
            next_block(&pool, &taken, 16).map(|b| b.to_string()),
            Some("10.129.0.0/16".to_string())
        );
        // A pool of /16s yields exactly 128 of them, then it is full.
        let mut taken = Vec::new();
        for _ in 0..128 {
            let Some(next) = next_block(&pool, &taken, 16) else {
                panic!("pool exhausted early");
            };
            taken.push(next);
        }
        assert!(next_block(&pool, &taken, 16).is_none());
    }

    #[test]
    fn test_block_budget_and_reserve() {
        let blocks = vec![block("10.88.0.0/24")];
        // 256 addresses - network - broadcast - the node's own .1
        assert_eq!(block_hosts(&blocks[0]), 253);
        assert_eq!(total_hosts(&blocks), 253);
        // 10% stays reserved for re-issues
        assert_eq!(allocatable(&blocks), 227);

        // Two blocks add up, a /32 carries nothing.
        let two = vec![block("10.88.0.0/24"), block("10.89.0.0/24"), block("10.90.0.5/32")];
        assert_eq!(total_hosts(&two), 506);
        assert_eq!(allocatable(&two), 455);
    }

    fn used(ips: &[&str]) -> HashSet<Ipv4Addr> {
        ips.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn starts_at_the_second_host() {
        let blocks = vec![block("10.88.0.0/24")];
        assert_eq!(
            first_free(&blocks, &HashSet::new()),
            Some("10.88.0.2".parse().unwrap())
        );
    }

    #[test]
    fn skips_used_addresses() {
        let blocks = vec![block("10.88.0.0/29")];
        let taken = used(&["10.88.0.2", "10.88.0.3"]);
        // .0 network, .1 interface, .2/.3 taken → .4
        assert_eq!(
            first_free(&blocks, &taken),
            Some("10.88.0.4".parse().unwrap())
        );
    }

    #[test]
    fn never_hands_out_broadcast() {
        let blocks = vec![block("10.88.0.0/29")];
        let taken = used(&["10.88.0.2", "10.88.0.3", "10.88.0.4", "10.88.0.5"]);
        // hosts of /29 are .1..6, .7 is broadcast
        assert_eq!(
            first_free(&blocks, &taken),
            Some("10.88.0.6".parse().unwrap())
        );
        let full = used(&[
            "10.88.0.2",
            "10.88.0.3",
            "10.88.0.4",
            "10.88.0.5",
            "10.88.0.6",
        ]);
        assert_eq!(first_free(&blocks, &full), None);
    }

    #[test]
    fn spills_into_the_next_block() {
        let blocks = vec![block("10.88.0.0/30"), block("10.88.1.0/24")];
        // /30 hosts: .1 (interface), .2 (last usable before broadcast .3)
        let taken = used(&["10.88.0.2"]);
        assert_eq!(
            first_free(&blocks, &taken),
            Some("10.88.1.2".parse().unwrap())
        );
    }

    #[test]
    fn a_large_block_fits_the_whole_pool() {
        let blocks = vec![block("10.88.0.0/20")];
        let first = first_free(&blocks, &HashSet::new()).unwrap();
        assert_eq!(first, "10.88.0.2".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn unusable_blocks_are_ignored() {
        // A /32 has no hosts: skip it instead of looping.
        let blocks = vec![block("10.88.0.5/32"), block("10.88.9.0/24")];
        assert_eq!(
            first_free(&blocks, &HashSet::new()),
            Some("10.88.9.2".parse().unwrap())
        );
        assert_eq!(usable_blocks(&blocks).len(), 1);
    }

    #[test]
    fn network_address_is_masked_from_the_block() {
        // Stored with a host address instead of the network address: the
        // allocator must still start from the network boundary.
        let blocks = vec![block("10.88.0.77/24")];
        assert_eq!(
            first_free(&blocks, &HashSet::new()),
            Some("10.88.0.2".parse().unwrap())
        );
    }
}
