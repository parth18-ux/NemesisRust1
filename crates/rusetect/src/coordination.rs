//! Flagged sender network rule (port of service/coordination.py): a payee that keeps receiving money from accounts
//! RuseTect itself flagged recently is collecting from many victims or laundering hops. The LOCAL count is off by
//! default (RUSETECT_COORDINATION=1 turns it on); cross-bank consortium flags stay on.
use std::collections::{HashMap, HashSet, VecDeque};

/// least noisy setting in the fusion test (7 days, senders held for an analyst, 3 of them); results/coordination_eval.json
pub const WINDOW_MIN: i64 = 7 * 24 * 60;
pub const FLAG_RANK: usize = 2;
pub const K: usize = 3;

pub fn enabled_from_env() -> bool {
    std::env::var("RUSETECT_COORDINATION").map(|v| v == "1").unwrap_or(false)
}

pub struct CoordinationTracker {
    pub window: i64,
    pub flag_rank: usize,
    pub k: usize,
    pub enabled: bool,
    flagged_at: HashMap<String, i64>,
    incoming: HashMap<String, VecDeque<(i64, String)>>,
}

impl Default for CoordinationTracker {
    fn default() -> Self {
        Self::new(WINDOW_MIN, FLAG_RANK, K, enabled_from_env())
    }
}

impl CoordinationTracker {
    pub fn new(window_min: i64, flag_rank: usize, k: usize, enabled: bool) -> Self {
        CoordinationTracker { window: window_min, flag_rank, k, enabled, flagged_at: HashMap::new(), incoming: HashMap::new() }
    }

    pub fn reset(&mut self) {
        self.flagged_at.clear();
        self.incoming.clear();
    }

    /// Distinct senders that paid this payee in the window and were flagged at or before their payment.
    pub fn flagged_payers(&mut self, payee: &str, t: i64) -> usize {
        let Some(q) = self.incoming.get_mut(payee) else { return 0 };
        if q.is_empty() {
            return 0;
        }
        while q.front().map(|e| e.0 <= t - self.window).unwrap_or(false) {
            q.pop_front();
        }
        let lo = t - self.window;
        q.iter().filter(|(_, s)| self.flagged_at.get(s).copied().unwrap_or(-1_000_000_000_000) > lo).map(|(_, s)| s.as_str()).collect::<HashSet<_>>().len()
    }

    pub fn coordinated(&mut self, payee: &str, t: i64) -> bool {
        self.enabled && self.flagged_payers(payee, t) >= self.k
    }

    /// After scoring: remember the payment and whether its sender was flagged.
    pub fn record(&mut self, sender: &str, payee: &str, t: i64, final_rank: usize) {
        if final_rank >= self.flag_rank {
            self.flagged_at.insert(sender.to_string(), t);
        }
        self.incoming.entry(payee.to_string()).or_default().push_back((t, sender.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn off_by_default_but_counts() {
        let mut t = CoordinationTracker::new(1000, 1, 2, false);
        t.record("s1", "P", 0, 2);
        t.record("s2", "P", 5, 2);
        assert_eq!(t.flagged_payers("P", 10), 2);
        assert!(!t.coordinated("P", 10));
        t.enabled = true;
        assert!(t.coordinated("P", 10));
    }
}
