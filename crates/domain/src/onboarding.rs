//! Joining through Discord: the roles someone may give themselves by
//! signing the welcome agreement. Crew and above stay a steward's decision
//! (`discord/server.toml` `[members]`).

use serde::{Deserialize, Serialize};

use crate::ValidationError;

string_enum!(
    /// Named exactly as the Discord role it grants.
    OnboardingRole, "role" {
    Community => "Community",
    Volunteer => "Volunteer",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_match_the_migration() {
        let sql = include_str!("../../../migrations/0050_onboarding.sql");
        crate::tests::assert_check_list(sql, "role", OnboardingRole::ALL, OnboardingRole::as_str);
        let joins = include_str!("../../../migrations/0051_joins.sql");
        crate::tests::assert_check_list(joins, "role", OnboardingRole::ALL, OnboardingRole::as_str);
    }
}
