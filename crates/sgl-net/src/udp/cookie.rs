use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};

const COOKIE_DOMAIN: &[u8] = b"udp-connect-cookie/v1";
pub(super) const COOKIE_EPOCH_MS: u64 = 5_000;
pub(super) const SOURCE_PREFIX_BUCKETS: usize = 64;

#[derive(Clone)]
pub(super) struct CookieKey([u8; 32]);

impl CookieKey {
    pub(super) const fn new(key: [u8; 32]) -> Self {
        Self(key)
    }

    pub(super) fn issue(&self, source: SocketAddr, client_nonce: u64, now_ms: u64) -> u64 {
        self.for_epoch(source, client_nonce, epoch(now_ms))
    }

    pub(super) fn validate(
        &self,
        source: SocketAddr,
        client_nonce: u64,
        cookie: u64,
        now_ms: u64,
    ) -> Option<u64> {
        if client_nonce == 0 || cookie == 0 {
            return None;
        }
        let current = epoch(now_ms);
        if self.for_epoch(source, client_nonce, current) == cookie {
            return Some(current);
        }
        let previous = current.checked_sub(1)?;
        (self.for_epoch(source, client_nonce, previous) == cookie).then_some(previous)
    }

    fn for_epoch(&self, source: SocketAddr, client_nonce: u64, epoch: u64) -> u64 {
        let mut hash = blake3::Hasher::new_keyed(&self.0);
        hash.update(COOKIE_DOMAIN);
        hash.update(&epoch.to_le_bytes());
        hash.update(&client_nonce.to_le_bytes());
        match source {
            SocketAddr::V4(source) => {
                hash.update(&[4]);
                hash.update(&source.ip().octets());
                hash.update(&source.port().to_le_bytes());
            }
            SocketAddr::V6(source) => {
                hash.update(&[6]);
                hash.update(&source.ip().octets());
                hash.update(&source.port().to_le_bytes());
                hash.update(&source.flowinfo().to_le_bytes());
                hash.update(&source.scope_id().to_le_bytes());
            }
        }
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&hash.finalize().as_bytes()[..8]);
        u64::from_le_bytes(bytes).max(1)
    }
}

const fn epoch(now_ms: u64) -> u64 {
    now_ms / COOKIE_EPOCH_MS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourcePrefix {
    V4([u8; 3]),
    V6([u8; 8]),
}

impl From<SocketAddr> for SourcePrefix {
    fn from(source: SocketAddr) -> Self {
        match source.ip() {
            IpAddr::V4(ip) => {
                let octets = ip.octets();
                Self::V4([octets[0], octets[1], octets[2]])
            }
            IpAddr::V6(ip) => {
                let octets = ip.octets();
                Self::V6(octets[..8].try_into().expect("IPv6 /64 is eight bytes"))
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PrefixBucket {
    prefix: SourcePrefix,
    tokens: u16,
    last_refill_ms: u64,
    last_seen_ms: u64,
}

pub(super) struct ChallengeLimiter {
    buckets: [Option<PrefixBucket>; SOURCE_PREFIX_BUCKETS],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct VerifiedConfirm {
    source: SocketAddr,
    client_nonce: u64,
    cookie: u64,
}

/// Verified confirms by their cookie's epoch. A cookie validates in its own
/// epoch and the next, so a slot per epoch parity holds every confirm whose
/// cookie can still validate, and a slot is emptied only once its epoch's
/// cookies have expired. A slot holds one entry per verified handshake in its
/// epoch, each having cost a challenge round trip from its address.
#[derive(Default)]
pub(super) struct ConfirmReplayCache {
    slots: [ReplayEpoch; 2],
}

#[derive(Default)]
struct ReplayEpoch {
    epoch: u64,
    confirms: BTreeSet<VerifiedConfirm>,
}

impl ConfirmReplayCache {
    pub(super) fn contains(
        &self,
        source: SocketAddr,
        client_nonce: u64,
        cookie: u64,
        cookie_epoch: u64,
    ) -> bool {
        let slot = &self.slots[slot_index(cookie_epoch)];
        slot.epoch == cookie_epoch
            && slot.confirms.contains(&VerifiedConfirm {
                source,
                client_nonce,
                cookie,
            })
    }

    pub(super) fn remember(
        &mut self,
        source: SocketAddr,
        client_nonce: u64,
        cookie: u64,
        cookie_epoch: u64,
        now_ms: u64,
    ) {
        let current = epoch(now_ms);
        for slot in &mut self.slots {
            if slot.epoch.saturating_add(1) < current {
                *slot = ReplayEpoch::default();
            }
        }
        // A cookie this old no longer validates, so nothing can replay it.
        if cookie_epoch.saturating_add(1) < current {
            return;
        }
        let slot = &mut self.slots[slot_index(cookie_epoch)];
        if slot.epoch != cookie_epoch {
            *slot = ReplayEpoch {
                epoch: cookie_epoch,
                confirms: BTreeSet::new(),
            };
        }
        slot.confirms.insert(VerifiedConfirm {
            source,
            client_nonce,
            cookie,
        });
    }

    #[cfg(test)]
    pub(super) fn occupied(&self) -> usize {
        self.slots.iter().map(|slot| slot.confirms.len()).sum()
    }
}

const fn slot_index(cookie_epoch: u64) -> usize {
    (cookie_epoch % 2) as usize
}

impl Default for ChallengeLimiter {
    fn default() -> Self {
        Self {
            buckets: [None; SOURCE_PREFIX_BUCKETS],
        }
    }
}

impl ChallengeLimiter {
    pub(super) fn allow(
        &mut self,
        source: SocketAddr,
        now_ms: u64,
        burst: u16,
        refill_ms: u64,
    ) -> bool {
        let prefix = SourcePrefix::from(source);
        let index = self
            .buckets
            .iter()
            .position(|bucket| bucket.is_some_and(|bucket| bucket.prefix == prefix))
            .or_else(|| self.buckets.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                self.buckets
                    .iter()
                    .enumerate()
                    .min_by_key(|(index, bucket)| {
                        (
                            bucket
                                .expect("a full limiter has no empty bucket")
                                .last_seen_ms,
                            *index,
                        )
                    })
                    .map_or(0, |(index, _)| index)
            });
        let bucket = self.buckets[index].get_or_insert(PrefixBucket {
            prefix,
            tokens: burst,
            last_refill_ms: now_ms,
            last_seen_ms: now_ms,
        });
        if bucket.prefix != prefix {
            *bucket = PrefixBucket {
                prefix,
                tokens: burst,
                last_refill_ms: now_ms,
                last_seen_ms: now_ms,
            };
        }
        let elapsed = now_ms.saturating_sub(bucket.last_refill_ms);
        let refills = elapsed / refill_ms;
        if refills > 0 {
            let added = u16::try_from(refills.min(u64::from(burst))).unwrap_or(burst);
            bucket.tokens = bucket.tokens.saturating_add(added).min(burst);
            bucket.last_refill_ms = now_ms;
        }
        bucket.last_seen_ms = now_ms;
        if bucket.tokens == 0 {
            return false;
        }
        bucket.tokens -= 1;
        true
    }

    #[cfg(test)]
    pub(super) fn occupied(&self) -> usize {
        self.buckets.iter().flatten().count()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #254: tokens refill by whole intervals: none one millisecond short of
    /// an interval, one at exactly an interval, and never past the burst.
    #[wasm_bindgen_test(unsupported = test)]
    fn challenge_tokens_refill_by_whole_intervals() {
        let source: SocketAddr = "198.51.100.7:4000".parse().unwrap();
        let mut limiter = ChallengeLimiter::default();
        assert!(limiter.allow(source, 0, 2, 1_000));
        assert!(limiter.allow(source, 0, 2, 1_000));
        assert!(!limiter.allow(source, 999, 2, 1_000));
        assert!(
            limiter.allow(source, 1_000, 2, 1_000),
            "one interval refills one token"
        );
        assert!(!limiter.allow(source, 1_000, 2, 1_000));
        assert!(
            limiter.allow(source, 9_000, 2, 1_000),
            "refills cap at the burst"
        );
        assert!(limiter.allow(source, 9_000, 2, 1_000));
        assert!(!limiter.allow(source, 9_000, 2, 1_000));
    }

    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), port).into()
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn cookie_binds_full_address_nonce_and_current_or_previous_epoch() {
        let key = CookieKey::new([7; 32]);
        let source = v4(203, 0, 113, 9, 40_000);
        let cookie = key.issue(source, 11, COOKIE_EPOCH_MS - 1);

        assert_eq!(key.validate(source, 11, cookie, COOKIE_EPOCH_MS), Some(0));
        assert_eq!(key.validate(source, 11, cookie, COOKIE_EPOCH_MS * 2), None);
        assert_eq!(
            key.validate(v4(203, 0, 113, 9, 40_001), 11, cookie, 0),
            None
        );
        assert_eq!(
            key.validate(v4(203, 0, 113, 10, 40_000), 11, cookie, 0),
            None
        );
        assert_eq!(key.validate(source, 12, cookie, 0), None);

        let v6_source: SocketAddr = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 40_000, 7, 3).into();
        let v6_cookie = key.issue(v6_source, 22, 0);
        let different_scope: SocketAddr =
            SocketAddrV6::new(Ipv6Addr::LOCALHOST, 40_000, 7, 4).into();
        let different_flow: SocketAddr =
            SocketAddrV6::new(Ipv6Addr::LOCALHOST, 40_000, 8, 3).into();
        assert_eq!(key.validate(v6_source, 22, v6_cookie, 0), Some(0));
        assert_eq!(key.validate(different_scope, 22, v6_cookie, 0), None);
        assert_eq!(key.validate(different_flow, 22, v6_cookie, 0), None);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn limiter_uses_v4_24_and_v6_64_prefixes_and_never_grows() {
        let mut limiter = ChallengeLimiter::default();
        for host in 0..u16::MAX {
            let third = u8::try_from(host >> 8).unwrap();
            let fourth = u8::try_from(host & 0xff).unwrap();
            let source = v4(10, third, fourth, 1, host);
            let _ = limiter.allow(source, 0, 1, 1_000);
        }
        assert_eq!(limiter.occupied(), SOURCE_PREFIX_BUCKETS);

        let mut v4_prefix = ChallengeLimiter::default();
        assert!(v4_prefix.allow(v4(192, 0, 2, 1, 1), 0, 1, 1_000));
        assert!(!v4_prefix.allow(v4(192, 0, 2, 254, 65_000), 0, 1, 1_000));
        assert!(v4_prefix.allow(v4(192, 0, 3, 1, 1), 0, 1, 1_000));

        let mut v6_prefix = ChallengeLimiter::default();
        let left: SocketAddr =
            SocketAddrV6::new(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 3, 4, 5, 6), 1, 0, 0).into();
        let same: SocketAddr =
            SocketAddrV6::new(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 9, 8, 7, 6), 2, 0, 0).into();
        let other: SocketAddr =
            SocketAddrV6::new(Ipv6Addr::new(0x2001, 0xdb8, 1, 3, 3, 4, 5, 6), 1, 0, 0).into();
        assert!(v6_prefix.allow(left, 0, 1, 1_000));
        assert!(!v6_prefix.allow(same, 0, 1, 1_000));
        assert!(v6_prefix.allow(other, 0, 1, 1_000));
    }

    /// #440: every confirm stays while its cookie can validate (its epoch
    /// and the next), however many arrive, and remembering an expired
    /// cookie's confirm evicts none of them.
    #[wasm_bindgen_test(unsupported = test)]
    fn verified_confirm_replay_cache_keeps_confirms_while_their_cookie_validates() {
        let source = |index: u16| {
            v4(
                203,
                u8::try_from(index >> 8).unwrap(),
                u8::try_from(index & 0xff).unwrap(),
                1,
                40_000,
            )
        };
        let mut cache = ConfirmReplayCache::default();
        for index in 0..1_000_u16 {
            cache.remember(
                source(index),
                1,
                u64::from(index) + 2,
                10,
                COOKIE_EPOCH_MS * 10,
            );
        }
        cache.remember(source(1_000), 1, 9, 11, COOKIE_EPOCH_MS * 11);
        cache.remember(source(1_001), 1, 9, 2, COOKIE_EPOCH_MS * 11);
        assert!((0..1_000_u16).all(|index| cache.contains(
            source(index),
            1,
            u64::from(index) + 2,
            10
        )));
        assert!(cache.contains(source(1_000), 1, 9, 11));
        assert!(!cache.contains(source(1_001), 1, 9, 2));

        cache.remember(source(1_002), 1, 9, 12, COOKIE_EPOCH_MS * 12);
        assert_eq!(cache.occupied(), 2);
        assert!(cache.contains(source(1_000), 1, 9, 11));
    }
}
