//! Gifts outside Zeffy, and the written acknowledgment the IRS requires.
//!
//! IRS Publication 1771: a donor needs a contemporaneous written
//! acknowledgment for any single gift of $250 or more, cash or property. It
//! must state the cash amount or *describe* (not value) donated property, and
//! say whether the charity gave goods or services in return, with a
//! good-faith estimate of their value if it did.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use uuid::Uuid;

use crate::{ValidationError, non_blank};

/// The organization as the IRS lists it (EO BMF, checked 2026-09-28).
pub const ORG_LEGAL_NAME: &str = "Sun Brewed Homestead Inc";
pub const ORG_EIN: &str = "92-0510478";
pub const ORG_MAILING_ADDRESS: &str = "11333 Moorpark St PMB 216, Studio City, CA 91602"; // oss-publish:allow: IRS-listed mailbox

/// Gifts at or above this need a written acknowledgment.
pub const ACKNOWLEDGMENT_THRESHOLD: Decimal = Decimal::from_parts(250, 0, 0, false, 0);

string_enum!(GiftKind, "kind" {
    Cash => "cash", InKind => "in_kind", Sponsorship => "sponsorship",
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gift {
    pub id: Uuid,
    pub donor_name: String,
    pub donor_email: String,
    pub kind: GiftKind,
    pub received_on: Date,
    pub amount: Option<Decimal>,
    pub description: String,
    pub estimated_value: Option<Decimal>,
    pub goods_or_services: String,
    pub goods_value: Option<Decimal>,
    pub fund: String,
    pub acknowledged_on: Option<Date>,
    pub notes: String,
}

impl Gift {
    /// Still owes the donor a written acknowledgment. An in-kind gift with
    /// no estimate counts: we can't show it's under the threshold.
    pub fn needs_acknowledgment(&self) -> bool {
        if self.acknowledged_on.is_some() {
            return false;
        }
        match self.kind {
            GiftKind::InKind => self.estimated_value.is_none_or(|v| v >= ACKNOWLEDGMENT_THRESHOLD),
            _ => self.amount.is_some_and(|a| a >= ACKNOWLEDGMENT_THRESHOLD),
        }
    }

    /// The acknowledgment letter, as plain text paragraphs.
    pub fn acknowledgment(&self) -> Vec<String> {
        let what = match (self.kind, self.amount) {
            (GiftKind::InKind, _) => format!("your donation of {}", self.description),
            (_, Some(a)) => {
                let for_what =
                    if self.description.is_empty() { String::new() } else { format!(" ({})", self.description) };
                format!("your contribution of ${}{for_what}", a.round_dp(2))
            }
            (_, None) => "your contribution".to_owned(),
        };
        let fund = if self.fund.is_empty() { String::new() } else { format!(", designated for {}", self.fund) };
        let mut out = vec![
            format!("Dear {},", self.donor_name),
            format!("Thank you for {what}{fund}, received on {}.", self.received_on),
        ];
        out.push(if self.goods_or_services.is_empty() {
            "No goods or services were provided in exchange for this contribution.".to_owned()
        } else {
            let value = self
                .goods_value
                .map(|v| format!(", with a good-faith estimated value of ${}", v.round_dp(2)))
                .unwrap_or_default();
            format!(
                "In exchange for this contribution we provided {}{value}. The amount you may deduct is limited to the \
                 excess of your contribution over that value.",
                self.goods_or_services
            )
        });
        if self.kind == GiftKind::InKind {
            out.push(
                "As required, this letter describes but does not value donated property; its value for tax purposes \
                 is yours to determine."
                    .to_owned(),
            );
        }
        out.push(format!(
            "{ORG_LEGAL_NAME} is a 501(c)(3) public charity, EIN {ORG_EIN}. Please keep this letter for your tax records."
        ));
        out
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewGift {
    pub donor_name: String,
    #[serde(default)]
    pub donor_email: String,
    pub kind: GiftKind,
    pub received_on: Date,
    #[serde(default)]
    pub amount: Option<Decimal>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub estimated_value: Option<Decimal>,
    #[serde(default)]
    pub goods_or_services: String,
    #[serde(default)]
    pub goods_value: Option<Decimal>,
    #[serde(default)]
    pub fund: String,
    #[serde(default)]
    pub notes: String,
}

impl NewGift {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let description = self.description.trim().to_owned();
        let goods_or_services = self.goods_or_services.trim().to_owned();
        match self.kind {
            GiftKind::InKind => {
                if self.amount.is_some() {
                    return Err(ValidationError::Malformed("amount (describe in-kind gifts; don't value them)"));
                }
                if description.is_empty() {
                    return Err(ValidationError::Blank("description"));
                }
            }
            GiftKind::Cash | GiftKind::Sponsorship => {
                if !self.amount.is_some_and(|a| a > Decimal::ZERO) {
                    return Err(ValidationError::NotPositive("amount"));
                }
                if self.estimated_value.is_some() {
                    return Err(ValidationError::Malformed("estimated_value (only for in-kind gifts)"));
                }
            }
        }
        if self.estimated_value.is_some_and(|v| v < Decimal::ZERO)
            || self.goods_value.is_some_and(|v| v < Decimal::ZERO)
        {
            return Err(ValidationError::Negative("value"));
        }
        if self.goods_value.is_some() && goods_or_services.is_empty() {
            return Err(ValidationError::Blank("goods_or_services"));
        }
        let email = self.donor_email.trim().to_lowercase();
        if !email.is_empty() && !(email.contains('@') && !email.contains(char::is_whitespace)) {
            return Err(ValidationError::Malformed("donor_email"));
        }
        Ok(Self {
            donor_name: non_blank("donor_name", &self.donor_name)?,
            donor_email: email,
            description,
            goods_or_services,
            fund: self.fund.trim().to_owned(),
            notes: self.notes.trim().to_owned(),
            ..self
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn gift(kind: GiftKind, amount: Option<i64>, estimated: Option<i64>) -> Gift {
        Gift {
            id: Uuid::nil(),
            donor_name: "Kellogg's".into(),
            donor_email: String::new(),
            kind,
            received_on: date!(2026 - 09 - 18),
            amount: amount.map(Decimal::from),
            description: if kind == GiftKind::InKind { "40 vegetable starts".into() } else { String::new() },
            estimated_value: estimated.map(Decimal::from),
            goods_or_services: String::new(),
            goods_value: None,
            fund: String::new(),
            acknowledged_on: None,
            notes: String::new(),
        }
    }

    #[test]
    fn enum_strings_match_migration() {
        let sql = include_str!("../../../migrations/0007_gifts.sql");
        crate::tests::assert_check_list(sql, "kind", GiftKind::ALL, GiftKind::as_str);
    }

    #[test]
    fn the_250_dollar_line_decides_who_needs_a_letter() {
        assert!(gift(GiftKind::Cash, Some(250), None).needs_acknowledgment(), "exactly $250 needs one");
        assert!(!gift(GiftKind::Cash, Some(249), None).needs_acknowledgment());
        assert!(gift(GiftKind::Sponsorship, Some(1000), None).needs_acknowledgment());
        assert!(gift(GiftKind::InKind, None, None).needs_acknowledgment(), "unknown value: assume it does");
        assert!(!gift(GiftKind::InKind, None, Some(40)).needs_acknowledgment());
        let mut sent = gift(GiftKind::Cash, Some(500), None);
        sent.acknowledged_on = Some(date!(2026 - 09 - 30));
        assert!(!sent.needs_acknowledgment());
    }

    #[test]
    fn in_kind_letters_describe_but_never_value_the_gift() {
        let letter = gift(GiftKind::InKind, None, Some(400)).acknowledgment().join("\n");
        assert!(letter.contains("40 vegetable starts"));
        assert!(!letter.contains("$400") && !letter.contains("400"), "in-kind value leaked: {letter}");
        assert!(letter.contains("No goods or services were provided"));
        assert!(letter.contains(ORG_EIN) && letter.contains(ORG_LEGAL_NAME));
    }

    #[test]
    fn letters_state_cash_and_any_quid_pro_quo() {
        let mut g = gift(GiftKind::Sponsorship, Some(1000), None);
        g.goods_or_services = "logo on the workshop banner".into();
        g.goods_value = Some(Decimal::from(50));
        let letter = g.acknowledgment().join("\n");
        assert!(letter.contains("$1000.00") || letter.contains("$1000"), "{letter}");
        assert!(letter.contains("logo on the workshop banner") && letter.contains("$50"));
        assert!(!letter.contains("No goods or services"));
    }

    #[test]
    fn validation_keeps_amounts_and_descriptions_to_their_kinds() {
        let new = |kind, amount: Option<i64>, description: &str, estimated: Option<i64>| NewGift {
            donor_name: " Neighbour ".into(),
            donor_email: " A@Example.org ".into(),
            kind,
            received_on: date!(2026 - 09 - 18),
            amount: amount.map(Decimal::from),
            description: description.into(),
            estimated_value: estimated.map(Decimal::from),
            goods_or_services: String::new(),
            goods_value: None,
            fund: String::new(),
            notes: String::new(),
        };
        assert!(new(GiftKind::InKind, Some(10), "plants", None).validated().is_err(), "in-kind with an amount");
        assert_eq!(new(GiftKind::InKind, None, " ", None).validated(), Err(ValidationError::Blank("description")));
        assert_eq!(new(GiftKind::Cash, None, "", None).validated(), Err(ValidationError::NotPositive("amount")));
        assert!(new(GiftKind::Cash, Some(20), "", Some(20)).validated().is_err(), "estimate on cash");
        let ok = new(GiftKind::InKind, None, "lumber", Some(80)).validated().expect("ok");
        assert_eq!((ok.donor_name.as_str(), ok.donor_email.as_str()), ("Neighbour", "a@example.org"));
        let mut bad_email = new(GiftKind::Cash, Some(5), "", None);
        bad_email.donor_email = "not an email".into();
        assert!(bad_email.validated().is_err());
    }

    /// The EIN printed on letters is the one the Zeffy campaign states.
    #[test]
    fn ein_matches_the_campaign_spec() {
        let campaign = include_str!("../../../zeffy/campaign.toml");
        assert!(campaign.contains(&format!("EIN {ORG_EIN}")));
    }
}
