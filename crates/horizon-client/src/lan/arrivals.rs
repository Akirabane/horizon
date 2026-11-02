//! Players that are about to arrive from another server, and the rule for how
//! long a server waits for them.
//!
//! A proxy that moves a player between two servers first tells the target
//! server who is coming (`player-expect <uuid>`); the player's next login is then
//! taken in as a *held* player: connected, receiving the world, but not
//! simulated, shown or announced until the hand-over finishes. Nothing here may
//! grow without bound, because a proxy that dies in the middle of a move leaves
//! its expectations and its held players behind: every entry expires, and the
//! table has a size cap.
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long an expectation lives when the request names no time.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10);
/// The longest an expectation may be asked to live.
pub const MAX_TTL: Duration = Duration::from_secs(60);
/// The most expectations the table keeps at once.
pub const MAX_EXPECTED: usize = 64;
/// Server ticks a held player may wait for its import before the server drops it
/// (one minute at 20 ticks per second).
pub const HELD_MAX_TICKS: u64 = 20 * 60;

/// Why an expectation could not be recorded.
#[derive(Debug, PartialEq, Eq)]
pub enum ExpectError {
    /// The table is full of live expectations.
    Full,
}

/// The players a server was told to expect, by unique id.
#[derive(Debug, Default)]
pub struct Expectations {
    until: HashMap<u128, Instant>,
}

impl Expectations {
    /// Records that `uuid` will log in within `ttl` (cut to [`MAX_TTL`]). A second
    /// request for the same player replaces the first one's deadline. Returns
    /// whether the player was already expected.
    pub fn expect(&mut self, uuid: u128, ttl: Duration, now: Instant) -> Result<bool, ExpectError> {
        self.expire(now);
        if !self.until.contains_key(&uuid) && self.until.len() >= MAX_EXPECTED {
            return Err(ExpectError::Full);
        }
        let deadline = now + ttl.min(MAX_TTL);
        Ok(self.until.insert(uuid, deadline).is_some())
    }

    /// Consumes the expectation of `uuid` if it is still alive: one login takes
    /// it, and no second login of the same player is then held.
    pub fn take(&mut self, uuid: u128, now: Instant) -> bool {
        match self.until.remove(&uuid) {
            Some(deadline) => now < deadline,
            None => false,
        }
    }

    /// Forgets the expectation of `uuid`. Returns whether there was one.
    pub fn forget(&mut self, uuid: u128) -> bool {
        self.until.remove(&uuid).is_some()
    }

    /// Drops the expectations whose time has passed and returns them, so the
    /// caller can log that the player never came.
    pub fn expire(&mut self, now: Instant) -> Vec<u128> {
        let gone: Vec<u128> = self
            .until
            .iter()
            .filter(|(_, deadline)| now >= **deadline)
            .map(|(uuid, _)| *uuid)
            .collect();
        for uuid in &gone {
            self.until.remove(uuid);
        }
        gone
    }

    /// How many expectations are recorded (some may be past their time but not
    /// yet swept).
    pub fn len(&self) -> usize {
        self.until.len()
    }

    pub fn is_empty(&self) -> bool {
        self.until.is_empty()
    }
}

/// Whether a held player that joined at tick `since` has waited too long at tick
/// `now` for its import.
pub fn held_too_long(since: u64, now: u64) -> bool {
    now.saturating_sub(since) >= HELD_MAX_TICKS
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u128 = 0xA;
    const B: u128 = 0xB;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn an_expected_player_is_taken_once_before_the_deadline() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        assert_eq!(e.expect(A, secs(10), t0), Ok(false));
        assert!(e.take(A, t0 + secs(9)));
        // The login consumed it: a second login of the same player is ordinary.
        assert!(!e.take(A, t0 + secs(9)));
        assert!(e.is_empty());
    }

    #[test]
    fn a_login_after_the_deadline_is_not_held() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(10), t0).unwrap();
        // Exactly at the deadline is already too late.
        assert!(!e.take(A, t0 + secs(10)));
        assert!(e.is_empty());
    }

    #[test]
    fn a_player_nobody_expected_is_not_held() {
        let mut e = Expectations::default();
        assert!(!e.take(B, Instant::now()));
    }

    #[test]
    fn asking_again_moves_the_deadline_and_says_so() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(5), t0).unwrap();
        assert_eq!(e.expect(A, secs(30), t0 + secs(4)), Ok(true));
        assert!(e.take(A, t0 + secs(20)));
    }

    #[test]
    fn a_long_wait_is_cut_to_the_maximum() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(3600), t0).unwrap();
        assert!(!e.take(A, t0 + MAX_TTL));
    }

    #[test]
    fn expiry_returns_the_players_that_never_came_and_keeps_the_others() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(5), t0).unwrap();
        e.expect(B, secs(30), t0).unwrap();
        assert_eq!(e.expire(t0 + secs(6)), vec![A]);
        assert_eq!(e.len(), 1);
        assert!(e.take(B, t0 + secs(7)));
    }

    #[test]
    fn the_table_refuses_new_players_when_full_but_expired_ones_make_room() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        for n in 0..MAX_EXPECTED as u128 {
            e.expect(100 + n, secs(10), t0).unwrap();
        }
        assert_eq!(e.expect(A, secs(10), t0), Err(ExpectError::Full));
        // A player already in the table can still be renewed when it is full.
        assert_eq!(e.expect(100, secs(10), t0), Ok(true));
        // Once the old ones are past their time, there is room again.
        assert_eq!(e.expect(A, secs(10), t0 + secs(11)), Ok(false));
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn forgetting_removes_only_that_player() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(10), t0).unwrap();
        e.expect(B, secs(10), t0).unwrap();
        assert!(e.forget(A));
        assert!(!e.forget(A));
        assert!(e.take(B, t0));
    }

    #[test]
    fn a_held_player_waits_one_minute_of_ticks() {
        assert!(!held_too_long(100, 100 + 1199));
        assert!(held_too_long(100, 100 + 1200));
        // A clock that went backwards never drops anyone.
        assert!(!held_too_long(500, 100));
    }
}
