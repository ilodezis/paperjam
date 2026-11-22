//! Brakes on guessing passwords and passkeys.
//!
//! The main brake is per `(client address, username)`: a few free failures,
//! then each further one makes the next attempt wait twice as long. Someone
//! failing from another address never locks the owner out. A second brake per
//! address counts failures over every name, so random usernames from one
//! address cannot keep every password check busy. A softer brake per username
//! catches guessing spread over many addresses: past a hundred failures an
//! hour, password attempts on that name are spaced out, while passkeys keep
//! working.
//!
//! An IPv6 address counts by its /64 ([`rate_key`]): one host is usually
//! handed a whole /64, so its addresses are one client. Keys are stored as
//! hashes and old entries are pruned, so memory stays small whatever names
//! and addresses arrive.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::ServiceError;

/// Failures per `(address, name)` before the delay starts.
const FREE_FAILURES: u32 = 5;
/// Password failures per address, over all names, before the delay starts.
const ADDRESS_FREE_FAILURES: u32 = 20;
const MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// A pair is forgotten this long after its last failure.
const PAIR_TTL: Duration = Duration::from_secs(60 * 60);
/// Failures per name within [`NAME_WINDOW`], from any address, before
/// password attempts on it are spaced out.
const NAME_SOFT_LIMIT: u32 = 100;
const NAME_WINDOW: Duration = Duration::from_secs(60 * 60);
/// The spacing between password attempts on a name over its soft limit.
const NAME_GAP: Duration = Duration::from_secs(10);
/// Passkey challenges per address within [`CHALLENGE_WINDOW`].
const MAX_CHALLENGES: u32 = 20;
const CHALLENGE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// Above this many entries a map is pruned; if pruning is not enough, the
/// entries touched longest ago go, live lockouts last.
const MAX_ENTRIES: usize = 50_000;

/// The address a limiter counts: an IPv4 address as it is, an IPv6 address
/// by its /64 (an IPv4-mapped one as the IPv4 address).
pub fn rate_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let mask = u128::MAX << 64;
                IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
            }
        },
    }
}

/// The one shape a username has for counting: whitespace and case do not
/// make a new name.
pub fn normalize_username(name: &str) -> String {
    name.trim().to_lowercase()
}

#[derive(Clone, Copy)]
struct Strikes {
    failures: u32,
    last: Instant,
    wait_until: Option<Instant>,
}

#[derive(Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
    last_attempt: Option<Instant>,
}

#[derive(Default)]
struct Maps {
    pairs: HashMap<u64, Strikes>,
    addresses: HashMap<u64, Strikes>,
    names: HashMap<u64, Window>,
    challenges: HashMap<u64, Window>,
}

pub struct LoginLimiter {
    hasher: RandomState,
    maps: Mutex<Maps>,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self {
            hasher: RandomState::new(),
            maps: Mutex::new(Maps::default()),
        }
    }
}

fn too_many() -> ServiceError {
    ServiceError::rate_limited(
        "too_many_attempts",
        "too many failed sign-in attempts, try again in a few minutes",
    )
}

/// Keeps `map` within [`MAX_ENTRIES`]: expired entries go first; if that is
/// not enough, the ones touched longest ago go down to three quarters of the
/// limit, entries that still lock someone out only after all others.
fn bound<V>(
    map: &mut HashMap<u64, V>,
    alive: impl Fn(&V) -> bool,
    locked: impl Fn(&V) -> bool,
    touched: impl Fn(&V) -> Instant,
) {
    if map.len() <= MAX_ENTRIES {
        return;
    }
    map.retain(|_, v| alive(v));
    if map.len() <= MAX_ENTRIES {
        return;
    }
    let mut order: Vec<(bool, Instant, u64)> = map
        .iter()
        .map(|(k, v)| (locked(v), touched(v), *k))
        .collect();
    order.sort_unstable();
    let excess = map.len() - MAX_ENTRIES * 3 / 4;
    for (_, _, key) in order.into_iter().take(excess) {
        map.remove(&key);
    }
}

fn bound_strikes(map: &mut HashMap<u64, Strikes>, now: Instant) {
    bound(
        map,
        |s| s.last.elapsed() < PAIR_TTL,
        |s| s.wait_until.is_some_and(|t| t > now),
        |s| s.last,
    );
}

fn bound_windows(map: &mut HashMap<u64, Window>, window: Duration, limit: u32) {
    bound(
        map,
        |w| w.start.elapsed() < window,
        |w| w.count >= limit,
        |w| w.last_attempt.unwrap_or(w.start).max(w.start),
    );
}

impl LoginLimiter {
    fn with<T>(&self, f: impl FnOnce(&mut Maps) -> T) -> T {
        let mut guard = self
            .maps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    fn pair_key(&self, ip: IpAddr, scope: &str, name: &str) -> u64 {
        self.hasher
            .hash_one((rate_key(ip), scope, normalize_username(name)))
    }

    fn address_key(&self, ip: IpAddr) -> u64 {
        self.hasher.hash_one(("address", rate_key(ip)))
    }

    fn name_key(&self, name: &str) -> u64 {
        self.hasher.hash_one(("name", normalize_username(name)))
    }

    fn check_pair(maps: &mut Maps, key: u64, now: Instant) -> Result<(), ServiceError> {
        Self::check_strikes(&maps.pairs, key, now)
    }

    fn check_strikes(
        map: &HashMap<u64, Strikes>,
        key: u64,
        now: Instant,
    ) -> Result<(), ServiceError> {
        match map.get(&key) {
            Some(s) if s.wait_until.is_some_and(|t| t > now) => Err(too_many()),
            _ => Ok(()),
        }
    }

    fn fail_pair(maps: &mut Maps, key: u64, now: Instant) {
        Self::strike(&mut maps.pairs, key, now, FREE_FAILURES);
    }

    fn strike(map: &mut HashMap<u64, Strikes>, key: u64, now: Instant, free: u32) {
        bound_strikes(map, now);
        let entry = map.entry(key).or_insert(Strikes {
            failures: 0,
            last: now,
            wait_until: None,
        });
        if now.duration_since(entry.last) >= PAIR_TTL {
            entry.failures = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
        entry.last = now;
        if entry.failures > free {
            let step = (entry.failures - free - 1).min(20);
            let delay = Duration::from_secs(1u64 << step).min(MAX_DELAY);
            entry.wait_until = Some(now + delay);
        }
    }

    /// Before a password is checked: refuses while this address must wait
    /// (for this name, or after too many failures over all names), or while
    /// the name is over its soft limit and was tried moments ago.
    pub fn check_password(&self, ip: IpAddr, name: &str) -> Result<(), ServiceError> {
        let pair = self.pair_key(ip, "password", name);
        let address = self.address_key(ip);
        let name = self.name_key(name);
        let now = Instant::now();
        self.with(|maps| {
            Self::check_strikes(&maps.addresses, address, now)?;
            Self::check_pair(maps, pair, now)?;
            if let Some(w) = maps.names.get_mut(&name)
                && now.duration_since(w.start) < NAME_WINDOW
                && w.count >= NAME_SOFT_LIMIT
            {
                if w.last_attempt
                    .is_some_and(|t| now.duration_since(t) < NAME_GAP)
                {
                    return Err(too_many());
                }
                w.last_attempt = Some(now);
            }
            Ok(())
        })
    }

    pub fn password_failed(&self, ip: IpAddr, name: &str) {
        let pair = self.pair_key(ip, "password", name);
        let address = self.address_key(ip);
        let name = self.name_key(name);
        let now = Instant::now();
        self.with(|maps| {
            Self::fail_pair(maps, pair, now);
            Self::strike(&mut maps.addresses, address, now, ADDRESS_FREE_FAILURES);
            bound_windows(&mut maps.names, NAME_WINDOW, NAME_SOFT_LIMIT);
            let w = maps.names.entry(name).or_insert(Window {
                start: now,
                count: 0,
                last_attempt: None,
            });
            if now.duration_since(w.start) >= NAME_WINDOW {
                *w = Window {
                    start: now,
                    count: 0,
                    last_attempt: None,
                };
            }
            w.count = w.count.saturating_add(1);
        });
    }

    /// A correct password clears this address's strikes for the name. The
    /// per-name count and the address's count over all names stay: one
    /// success (on an account the guesser owns, say) must not reset an attack.
    pub fn password_succeeded(&self, ip: IpAddr, name: &str) {
        let pair = self.pair_key(ip, "password", name);
        self.with(|maps| {
            maps.pairs.remove(&pair);
        });
    }

    pub fn check_passkey(&self, ip: IpAddr) -> Result<(), ServiceError> {
        let pair = self.pair_key(ip, "passkey", "");
        self.with(|maps| Self::check_pair(maps, pair, Instant::now()))
    }

    pub fn passkey_failed(&self, ip: IpAddr) {
        let pair = self.pair_key(ip, "passkey", "");
        self.with(|maps| Self::fail_pair(maps, pair, Instant::now()));
    }

    pub fn passkey_succeeded(&self, ip: IpAddr) {
        let pair = self.pair_key(ip, "passkey", "");
        self.with(|maps| {
            maps.pairs.remove(&pair);
        });
    }

    pub fn check_email_ingest(&self, ip: IpAddr) -> Result<(), ServiceError> {
        let pair = self.pair_key(ip, "email_ingest", "");
        self.with(|maps| Self::check_pair(maps, pair, Instant::now()))
    }

    pub fn email_ingest_failed(&self, ip: IpAddr) {
        let pair = self.pair_key(ip, "email_ingest", "");
        self.with(|maps| Self::fail_pair(maps, pair, Instant::now()));
    }

    pub fn email_ingest_succeeded(&self, ip: IpAddr) {
        let pair = self.pair_key(ip, "email_ingest", "");
        self.with(|maps| {
            maps.pairs.remove(&pair);
        });
    }

    /// Counts a passkey challenge; refuses past [`MAX_CHALLENGES`] per
    /// address, so one client cannot fill the shared ceremony store.
    pub fn allow_challenge(&self, ip: IpAddr) -> Result<(), ServiceError> {
        let key = self.hasher.hash_one(("challenge", rate_key(ip)));
        let now = Instant::now();
        self.with(|maps| {
            bound_windows(&mut maps.challenges, CHALLENGE_WINDOW, MAX_CHALLENGES);
            let w = maps.challenges.entry(key).or_insert(Window {
                start: now,
                count: 0,
                last_attempt: None,
            });
            if now.duration_since(w.start) >= CHALLENGE_WINDOW {
                w.start = now;
                w.count = 0;
            }
            if w.count >= MAX_CHALLENGES {
                return Err(ServiceError::rate_limited(
                    "too_many_attempts",
                    "too many passkey sign-ins started, try again in a few minutes",
                ));
            }
            w.count += 1;
            Ok(())
        })
    }

    #[cfg(test)]
    fn rewind(&self, by: Duration) {
        self.with(|maps| {
            for s in maps.pairs.values_mut().chain(maps.addresses.values_mut()) {
                s.last -= by;
                s.wait_until = s.wait_until.map(|t| t - by);
            }
            for w in maps.names.values_mut().chain(maps.challenges.values_mut()) {
                w.start -= by;
                w.last_attempt = w.last_attempt.map(|t| t - by);
            }
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.with(|maps| {
            maps.pairs.len() + maps.addresses.len() + maps.names.len() + maps.challenges.len()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ipv6_counts_by_its_64() {
        assert_eq!(rate_key(ip("2001:db8:1:2:aaaa::1")), ip("2001:db8:1:2::"));
        assert_eq!(
            rate_key(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")),
            ip("2001:db8:1:2::")
        );
        assert_ne!(rate_key(ip("2001:db8:1:3::1")), ip("2001:db8:1:2::"));
        assert_eq!(rate_key(ip("::ffff:203.0.113.7")), ip("203.0.113.7"));
        assert_eq!(rate_key(ip("203.0.113.7")), ip("203.0.113.7"));

        // Hopping addresses inside one /64 does not reset the brake.
        let l = LoginLimiter::default();
        for i in 0..=FREE_FAILURES {
            l.password_failed(ip(&format!("2001:db8::{:x}", i + 1)), "alice");
        }
        assert!(l.check_password(ip("2001:db8::beef"), "alice").is_err());
        l.check_password(ip("2001:db8:0:1::1"), "alice").unwrap();
        for i in 0..MAX_CHALLENGES {
            l.allow_challenge(ip(&format!("2001:db8:5::{:x}", i + 1)))
                .unwrap();
        }
        assert!(l.allow_challenge(ip("2001:db8:5::ffff")).is_err());
    }

    #[test]
    fn random_names_from_one_address_hit_the_address_brake() {
        let l = LoginLimiter::default();
        let a = ip("203.0.113.7");
        for i in 0..ADDRESS_FREE_FAILURES {
            let name = format!("guess{i}");
            l.check_password(a, &name).unwrap();
            l.password_failed(a, &name);
        }
        l.check_password(a, "fresh-name").unwrap();
        l.password_failed(a, "fresh-name");
        // Every name is refused now, a new one too, until the wait is over.
        assert!(l.check_password(a, "another-name").is_err());
        assert!(l.check_password(a, "alice").is_err());
        // A success does not clear the address's count.
        l.password_succeeded(a, "alice");
        assert!(l.check_password(a, "alice").is_err());
        // Other addresses are not affected.
        l.check_password(ip("198.51.100.1"), "alice").unwrap();
        l.rewind(Duration::from_secs(1));
        l.check_password(a, "alice").unwrap();
    }

    #[test]
    fn a_full_map_keeps_live_lockouts() {
        let l = LoginLimiter::default();
        let locked = ip("198.51.100.66");
        for _ in 0..=FREE_FAILURES {
            l.password_failed(locked, "alice");
        }
        assert!(l.check_password(locked, "alice").is_err());
        // Fill the pair map past its limit with single failures.
        l.with(|maps| {
            let now = Instant::now();
            for i in 0..=MAX_ENTRIES as u64 {
                LoginLimiter::fail_pair(maps, i, now);
            }
        });
        assert!(l.with(|maps| maps.pairs.len()) <= MAX_ENTRIES);
        assert!(
            l.check_password(locked, "alice").is_err(),
            "the lockout survived"
        );
    }

    #[test]
    fn free_failures_then_a_growing_wait() {
        let l = LoginLimiter::default();
        let a = ip("203.0.113.7");
        for _ in 0..FREE_FAILURES {
            l.check_password(a, "alice").unwrap();
            l.password_failed(a, "alice");
        }
        l.check_password(a, "alice").unwrap();
        l.password_failed(a, "alice");
        assert!(l.check_password(a, "alice").is_err());
        // One second later the next attempt is allowed again.
        l.rewind(Duration::from_secs(1));
        l.check_password(a, "alice").unwrap();
        l.password_failed(a, "alice");
        // Now it is two seconds.
        l.rewind(Duration::from_secs(1));
        assert!(l.check_password(a, "alice").is_err());
        l.rewind(Duration::from_secs(1));
        l.check_password(a, "alice").unwrap();
    }

    #[test]
    fn the_wait_is_capped() {
        let l = LoginLimiter::default();
        let a = ip("203.0.113.7");
        for _ in 0..200 {
            l.password_failed(a, "alice");
        }
        assert!(l.check_password(a, "alice").is_err());
        l.rewind(MAX_DELAY);
        l.check_password(a, "alice").unwrap();
    }

    #[test]
    fn failures_elsewhere_do_not_lock_the_owner_out() {
        let l = LoginLimiter::default();
        for _ in 0..50 {
            l.password_failed(ip("198.51.100.66"), "alice");
        }
        assert!(l.check_password(ip("198.51.100.66"), "alice").is_err());
        l.check_password(ip("203.0.113.7"), "alice").unwrap();
    }

    #[test]
    fn whitespace_and_case_do_not_make_a_new_name() {
        let l = LoginLimiter::default();
        let a = ip("203.0.113.7");
        for n in ["alice", "alice ", " Alice", "ALICE\t", "  alice  ", "Alice"] {
            l.password_failed(a, n);
        }
        assert!(l.check_password(a, "alice").is_err());
        assert!(l.check_password(a, "  aLiCe ").is_err());
    }

    #[test]
    fn a_spread_out_attack_spaces_password_attempts_on_the_name() {
        let l = LoginLimiter::default();
        for i in 0..NAME_SOFT_LIMIT {
            let a = IpAddr::from([10, 0, (i / 256) as u8, (i % 256) as u8]);
            l.password_failed(a, "alice");
        }
        let owner = ip("203.0.113.7");
        // The first attempt passes, the next one right after waits.
        l.check_password(owner, "alice").unwrap();
        assert!(l.check_password(owner, "alice").is_err());
        l.rewind(NAME_GAP);
        l.check_password(owner, "alice").unwrap();
        // Other names and passkeys are untouched.
        l.check_password(ip("198.51.100.1"), "bob").unwrap();
        l.check_passkey(owner).unwrap();
        // After the window the name is free again.
        l.rewind(NAME_WINDOW);
        l.check_password(owner, "alice").unwrap();
        l.check_password(owner, "alice").unwrap();
    }

    #[test]
    fn success_clears_the_pair() {
        let l = LoginLimiter::default();
        let a = ip("203.0.113.7");
        for _ in 0..10 {
            l.password_failed(a, "alice");
        }
        l.password_succeeded(a, "alice");
        l.check_password(a, "alice").unwrap();
    }

    #[test]
    fn passkeys_are_counted_per_address() {
        let l = LoginLimiter::default();
        for _ in 0..50 {
            l.passkey_failed(ip("198.51.100.66"));
        }
        assert!(l.check_passkey(ip("198.51.100.66")).is_err());
        l.check_passkey(ip("203.0.113.7")).unwrap();
        l.passkey_succeeded(ip("198.51.100.66"));
        l.check_passkey(ip("198.51.100.66")).unwrap();
    }

    #[test]
    fn challenges_are_limited_per_address() {
        let l = LoginLimiter::default();
        let a = ip("198.51.100.66");
        for _ in 0..MAX_CHALLENGES {
            l.allow_challenge(a).unwrap();
        }
        assert!(l.allow_challenge(a).is_err());
        l.allow_challenge(ip("203.0.113.7")).unwrap();
        l.rewind(CHALLENGE_WINDOW);
        l.allow_challenge(a).unwrap();
    }

    #[test]
    fn memory_stays_bounded() {
        let l = LoginLimiter::default();
        for i in 0..(MAX_ENTRIES as u32 + 10) {
            l.password_failed(IpAddr::from(i.to_be_bytes()), &format!("user{i}"));
        }
        // Pairs, addresses and names: each map stays within its limit.
        assert!(l.len() <= 3 * MAX_ENTRIES + 3);
    }

    #[test]
    fn email_ingest_is_limited_per_address() {
        let l = LoginLimiter::default();
        let a = ip("198.51.100.66");
        for _ in 0..FREE_FAILURES {
            l.check_email_ingest(a).unwrap();
            l.email_ingest_failed(a);
        }
        l.check_email_ingest(a).unwrap();
        l.email_ingest_failed(a);
        assert!(l.check_email_ingest(a).is_err());
        l.rewind(Duration::from_secs(1));
        l.check_email_ingest(a).unwrap();
        l.email_ingest_failed(a);
        l.rewind(Duration::from_secs(1));
        assert!(l.check_email_ingest(a).is_err());
        l.check_email_ingest(ip("203.0.113.7")).unwrap();
        l.email_ingest_succeeded(a);
        l.check_email_ingest(a).unwrap();
    }
}
