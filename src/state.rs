//! What has been shown, kept across reboots so photos do not repeat until all have been seen.

use anyhow::{Context, Result};
use rand::seq::IndexedRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    /// Cache key of the photo on the panel now (empty while the welcome screen is up).
    pub current: Option<String>,
    /// Keys shown since the pool was last exhausted.
    pub played: Vec<String>,
    /// Seconds since the epoch of the last panel refresh.
    pub last_refresh: Option<i64>,
    /// The panel shows the welcome screen.
    pub welcome: bool,
}

impl State {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("state.json")
    }

    /// A missing or corrupt file means a fresh start, never an error.
    pub fn load(dir: &Path) -> State {
        std::fs::read_to_string(Self::path(dir)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = Self::path(dir);
        crate::fsutil::write_atomic(&path, &serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Choose the next photo: random among those not yet shown; when all have been, start a new round (never
/// repeating the photo on the panel when there is another).
pub fn pick(ready: &[String], played: &mut Vec<String>, current: Option<&str>, rng: &mut impl Rng) -> Option<String> {
    if ready.is_empty() {
        return None;
    }
    played.retain(|k| ready.contains(k));
    let mut fresh: Vec<&String> = ready.iter().filter(|k| !played.contains(k)).collect();
    if fresh.is_empty() {
        played.clear();
        fresh = ready.iter().filter(|k| ready.len() == 1 || Some(k.as_str()) != current).collect();
    }
    let choice = (*fresh.choose(rng)?).clone();
    played.push(choice.clone());
    Some(choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn keys(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("k{i}")).collect()
    }

    #[test]
    fn every_photo_is_shown_once_per_round_and_the_round_never_repeats_the_last() {
        let ready = keys(5);
        let mut played = Vec::new();
        let mut rng = StdRng::seed_from_u64(1);
        let mut cur: Option<String> = None;
        for round in 0..3 {
            let mut seen = Vec::new();
            for _ in 0..5 {
                let k = pick(&ready, &mut played, cur.as_deref(), &mut rng).unwrap();
                assert!(!seen.contains(&k), "round {round} repeated {k}");
                if seen.is_empty() && round > 0 {
                    assert_ne!(Some(&k), cur.as_ref(), "new round started with the photo just shown");
                }
                cur = Some(k.clone());
                seen.push(k);
            }
            assert_eq!(seen.len(), 5);
        }
    }

    #[test]
    fn single_photo_repeats_and_empty_gives_none() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut played = Vec::new();
        assert_eq!(pick(&[], &mut played, None, &mut rng), None);
        let one = keys(1);
        assert_eq!(pick(&one, &mut played, None, &mut rng).as_deref(), Some("k0"));
        assert_eq!(pick(&one, &mut played, Some("k0"), &mut rng).as_deref(), Some("k0"));
    }

    #[test]
    fn deleted_photos_leave_the_history() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut played = vec!["gone".to_string(), "k0".to_string()];
        pick(&keys(3), &mut played, None, &mut rng).unwrap();
        assert!(!played.contains(&"gone".to_string()));
    }

    #[test]
    fn state_round_trip_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()), State::default());
        let s = State { current: Some("a".into()), played: vec!["a".into()], last_refresh: Some(5), welcome: false };
        s.save(dir.path()).unwrap();
        assert_eq!(State::load(dir.path()), s);
        std::fs::write(State::path(dir.path()), "{ not json").unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }
}
