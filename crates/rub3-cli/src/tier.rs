//! Tier bundles and front doors, named the way an operator names them.
//!
//! `architecture.md` -> "Security Tiers" names the five tiers `offline`,
//! `cached`, `verified`, `cooldown` and `hardened`;
//! `crates/rub3-wrapper/Cargo.toml` names the same five `tier-0` .. `tier-4`.
//! Both spellings are accepted here, and the cargo feature is what comes out,
//! because that is what the build has to be given.

use std::fmt;

/// A tier bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Offline,
    Cached,
    Verified,
    Cooldown,
    Hardened,
}

impl Tier {
    /// The cargo feature that selects this bundle.
    pub fn feature(self) -> &'static str {
        match self {
            Tier::Offline => "tier-0",
            Tier::Cached => "tier-1",
            Tier::Verified => "tier-2",
            Tier::Cooldown => "tier-3",
            Tier::Hardened => "tier-4",
        }
    }

    /// The tier's number, for the comparisons that need an order.
    pub fn level(self) -> u8 {
        match self {
            Tier::Offline => 0,
            Tier::Cached => 1,
            Tier::Verified => 2,
            Tier::Cooldown => 3,
            Tier::Hardened => 4,
        }
    }

    /// Accepts the name, the cargo feature, or the bare number.
    pub fn parse(value: &str) -> Result<Tier, String> {
        let tier = match value.trim().to_ascii_lowercase().as_str() {
            "offline" | "tier-0" | "0" => Tier::Offline,
            "cached" | "tier-1" | "1" => Tier::Cached,
            "verified" | "tier-2" | "2" => Tier::Verified,
            "cooldown" | "tier-3" | "3" => Tier::Cooldown,
            "hardened" | "tier-4" | "4" => Tier::Hardened,
            other => {
                return Err(format!(
                    "unknown tier `{other}`. The five are offline (0), cached (1), verified (2), \
                     cooldown (3) and hardened (4); the tier-N spelling works too. \
                     architecture.md -> \"Security Tiers\" says what each one enforces."
                ))
            }
        };
        Ok(tier)
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Tier::Offline => "offline",
            Tier::Cached => "cached",
            Tier::Verified => "verified",
            Tier::Cooldown => "cooldown",
            Tier::Hardened => "hardened",
        };
        write!(f, "{} ({})", name, self.feature())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_tier_reaches_the_same_feature() {
        for spelling in ["cooldown", "COOLDOWN", "tier-3", "3", " tier-3 "] {
            assert_eq!(Tier::parse(spelling).unwrap(), Tier::Cooldown, "{spelling}");
        }
        assert_eq!(Tier::Cooldown.feature(), "tier-3");
    }

    #[test]
    fn the_five_tiers_map_onto_the_five_bundles() {
        let features: Vec<&str> = [
            Tier::Offline,
            Tier::Cached,
            Tier::Verified,
            Tier::Cooldown,
            Tier::Hardened,
        ]
        .into_iter()
        .map(Tier::feature)
        .collect();
        assert_eq!(features, ["tier-0", "tier-1", "tier-2", "tier-3", "tier-4"]);
    }

    #[test]
    fn an_unknown_tier_is_refused_with_the_list() {
        let err = Tier::parse("paranoid").unwrap_err();
        assert!(err.contains("paranoid"), "{err}");
        assert!(err.contains("cooldown"), "{err}");
    }
}
