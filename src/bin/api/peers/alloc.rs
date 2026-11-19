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
