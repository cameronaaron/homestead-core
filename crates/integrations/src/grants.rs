//! Personalized grant-letter rendering and Resend delivery.
//!
//! Rendering is deliberately deterministic: a campaign stores the rendered
//! result, so approval always applies to the exact copy that gets sent.

use std::collections::BTreeMap;

use reqwest::StatusCode;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient<'a> {
    pub organization: &'a str,
    pub contact_name: &'a str,
    pub email: &'a str,
    pub program_name: &'a str,
    pub custom_fields: &'a BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub subject: String,
    pub text: String,
    pub html: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LetterTemplate {
    pub key: &'static str,
    pub title: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub subject: &'static str,
    pub body: &'static str,
    pub design: &'static str,
}

/// Copy-ready starting points. They state only facts with a source: the IRS
/// master file (501(c)(3), EIN), sunbrewedhomestead.org (Kiki's Cat Club
/// numbers since August 2024, the compost system, the farm stand) and the
/// team's own records. No acreage, budgets or amounts are invented; add those
/// per funder, as `{{variables}}` from the CSV.
pub const LETTER_LIBRARY: &[LetterTemplate] = &[
    LetterTemplate {
        key: "animal-welfare-cat-club",
        title: "Animal welfare: Kiki's Cat Club",
        category: "Letter of inquiry",
        description: "For animal-welfare funders: TNR, fostering and adoption, with the club's real numbers.",
        subject: "Kiki's Cat Club and {{organization}}: community cats in Los Angeles",
        body: "Dear {{contact_name}},\n\nI'm writing from Sun Brewed Homestead, a 501(c)(3) community homestead in Van Nuys (EIN 92-0510478), about our rescue program, Kiki's Cat Club, and {{organization}}'s {{program_name}} work.\n\nKiki's Cat Club looks after free-roaming cats in Los Angeles. We sterilize community cats through the county's citywide TNR program, and cats who need homes come into our foster program, where they are spayed or neutered, socialized, dewormed and treated for fleas before adoption. Since August 2024 we have found forever homes for 29 cats, trapped, neutered and returned 21 more, hosted 7 adoption events, and we continue to care for more than 20 community cats. We are a PetSmart Charities adoption partner.\n\nSupport from {{organization}} would go directly to vaccines, spay/neuter surgery and adoption-event supplies. Could we share a short proposal, or is there an application we should complete?\n\nWith gratitude,\nSun Brewed Homestead\nsunbrewedhomestead.org",
        design: "orchard",
    },
    LetterTemplate {
        key: "urban-agriculture-food-access",
        title: "Urban agriculture and food access",
        category: "Letter of inquiry",
        description: "For food-security and urban-farming funders: the working homestead, farm stand and hens.",
        subject: "Growing food in the heart of the Valley: Sun Brewed Homestead and {{organization}}",
        body: "Dear {{contact_name}},\n\nSun Brewed Homestead is a 501(c)(3) community homestead in Van Nuys (EIN 92-0510478). We believe everyone deserves the chance to grow their own food, and we are building a working model of how small-scale, regenerative agriculture can strengthen a neighborhood's food system.\n\nToday that means garden beds in permaculture rotation, a flock of laying hens, a farm stand selling eggs, greens and herbs at neighborhood prices, and volunteers of every age learning by doing. We noticed that {{organization}}'s {{program_name}} program supports exactly this kind of local, practical work.\n\nWe would welcome the chance to learn what a strong application looks like for you, and to share a focused proposal. Would a brief conversation be possible?\n\nWarmly,\nSun Brewed Homestead\nsunbrewedhomestead.org",
        design: "meadow",
    },
    LetterTemplate {
        key: "composting-environmental-education",
        title: "Composting and environmental education",
        category: "Letter of inquiry",
        description: "For environmental and education funders: free hands-on workshops and community compost.",
        subject: "Composting 101 for the Valley: a proposal for {{organization}}",
        body: "Dear {{contact_name}},\n\nSun Brewed Homestead, a 501(c)(3) in Van Nuys (EIN 92-0510478), teaches neighbors to turn waste into soil. Our worm farm and three-bin compost system, built from pallets and enriched with manure from our hens, is both a working part of the farm and an outdoor classroom. Neighbors drop off their scraps with us instead of sending them to landfill.\n\nWe run free, hands-on workshops such as Composting 101 and would like to offer them more often and to more people. {{organization}}'s {{program_name}} seems like a natural fit: support would cover workshop materials, tools and seeds, so every session stays free.\n\nCould we send a short proposal, or is there a current cycle we should apply to?\n\nWith thanks,\nSun Brewed Homestead\nsunbrewedhomestead.org",
        design: "sunrise",
    },
    LetterTemplate {
        key: "sponsor-thank-you-update",
        title: "Sponsor thank-you and update",
        category: "Stewardship",
        description: "Thank a sponsor and show what their support made possible. Add an {{update}} column to the CSV.",
        subject: "Thank you, {{organization}}: what your support is growing",
        body: "Dear {{contact_name}},\n\nThank you. {{organization}}'s support has already made a difference at Sun Brewed Homestead, and we wanted you to see it.\n\n{{update}}\n\nEverything we do, from free workshops to Kiki's Cat Club adoptions to the farm stand, depends on partners like you. We would be glad to host your team at the homestead or share photos for your own channels.\n\nWith sunshine and gratitude,\nSun Brewed Homestead\nA 501(c)(3) public charity, EIN 92-0510478",
        design: "sunrise",
    },
    LetterTemplate {
        key: "foundation-introduction",
        title: "Foundation introduction",
        category: "Letter of inquiry",
        description: "A warm first approach connecting the funder's program to the homestead's community mission.",
        subject: "A community-growing partnership with Sun Brewed Homestead",
        body: "Dear {{contact_name}},\n\nI’m writing from Sun Brewed Homestead because {{organization}}’s {{program_name}} program appears closely aligned with our work: building a more resilient local food system while creating practical ways for neighbors to learn, contribute, and grow together.\n\nWe would value the opportunity to learn what makes a strong fit for your current funding priorities and to share a focused proposal grounded in the outcomes your team cares about.\n\nWould you be open to a short conversation, or could you point us toward the best next step?\n\nWith gratitude,\nSun Brewed Homestead",
        design: "sunrise",
    },
    LetterTemplate {
        key: "community-partnership",
        title: "Community partnership",
        category: "Collaboration",
        description: "Invites a local organization into a program, event, education, or food-access partnership.",
        subject: "A local partnership idea for {{organization}}",
        body: "Dear {{contact_name}},\n\nSun Brewed Homestead is growing more than food—we are creating a place where practical land stewardship and community care reinforce one another. We see a natural connection with {{organization}} and its {{program_name}} work.\n\nWe would love to explore a partnership shaped around our shared goals, whether that means a hands-on learning opportunity, a community event, resource sharing, or another format that serves the people we both care about.\n\nCould we schedule a brief conversation to compare priorities and see whether there is a useful project to build together?\n\nWarmly,\nSun Brewed Homestead",
        design: "meadow",
    },
    LetterTemplate {
        key: "corporate-sponsorship",
        title: "Corporate sponsorship",
        category: "Sponsorship",
        description: "A concise sponsorship invitation centered on visible, practical community impact.",
        subject: "Sponsoring local resilience with Sun Brewed Homestead",
        body: "Dear {{contact_name}},\n\nWe’re reaching out to {{organization}} because your {{program_name}} work suggests a commitment to strong local communities. Sun Brewed Homestead turns that commitment into something tangible: a working place for growing food, sharing skills, and bringing neighbors into meaningful participation.\n\nWe are seeking values-aligned sponsors who want to help make this work accessible and durable. We would be glad to shape a clear sponsorship proposal with defined uses, recognition, and reporting that fit your organization’s goals.\n\nWould a short introductory conversation be useful?\n\nSincerely,\nSun Brewed Homestead",
        design: "ink",
    },
    LetterTemplate {
        key: "in-kind-equipment",
        title: "Equipment or in-kind request",
        category: "In-kind support",
        description: "Requests tools, materials, expertise, or services without assuming a cash-grant process.",
        subject: "An in-kind partnership opportunity with Sun Brewed Homestead",
        body: "Dear {{contact_name}},\n\nSun Brewed Homestead is looking for practical partners who can help turn community support into lasting farm capacity. Because of {{organization}}’s work through {{program_name}}, we thought an in-kind partnership might be a particularly good fit.\n\nUseful support can take many forms—equipment, materials, professional expertise, transportation, or services. We would define the need and intended use clearly, acknowledge the contribution appropriately, and report back on what it made possible.\n\nMay we send a short, specific needs list or speak with the person who coordinates community partnerships?\n\nWith appreciation,\nSun Brewed Homestead",
        design: "orchard",
    },
    LetterTemplate {
        key: "relationship-follow-up",
        title: "Thoughtful follow-up",
        category: "Follow-up",
        description: "A respectful follow-up that adds context without manufacturing urgency or pressure.",
        subject: "Following up: Sun Brewed Homestead and {{organization}}",
        body: "Dear {{contact_name}},\n\nI wanted to gently follow up on my note about a possible connection between Sun Brewed Homestead and {{organization}}’s {{program_name}} work.\n\nWe remain interested because the alignment feels practical, not merely thematic: both efforts invest in stronger communities through work people can see and participate in. If another person or a future funding cycle would be a better fit, that guidance would be genuinely helpful.\n\nThank you for your time and for the work your organization is already doing.\n\nWarmly,\nSun Brewed Homestead",
        design: "sunrise",
    },
];

/// Printed at the foot of every message. Replies are suppressed by hand
/// with **Suppress** on the Grants page.
pub const OPT_OUT: &str = "Not the right fit? Reply \"unsubscribe\" and we won't write again.";

pub const DESIGNS: &[(&str, &str)] =
    &[("sunrise", "Sunrise gold"), ("meadow", "Meadow green"), ("ink", "Field notes"), ("orchard", "Orchard clay")];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    #[error("template variable {0:?} has no value for this recipient")]
    Missing(String),
    #[error("template has an unclosed variable")]
    Unclosed,
    #[error("template variable names may contain only letters, numbers, underscores and hyphens")]
    InvalidName,
    #[error("unknown letter design {0:?}")]
    InvalidDesign(String),
}

fn values(recipient: &Recipient<'_>) -> BTreeMap<String, String> {
    let mut values = recipient.custom_fields.clone();
    values.insert("organization".into(), recipient.organization.into());
    values.insert(
        "contact_name".into(),
        if recipient.contact_name.trim().is_empty() { "Grants Team".into() } else { recipient.contact_name.into() },
    );
    values.insert("email".into(), recipient.email.into());
    values.insert(
        "program_name".into(),
        if recipient.program_name.trim().is_empty() {
            "community investment".into()
        } else {
            recipient.program_name.into()
        },
    );
    values
}

fn interpolate(template: &str, values: &BTreeMap<String, String>) -> Result<String, RenderError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { return Err(RenderError::Unclosed) };
        let name = after[..end].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) {
            return Err(RenderError::InvalidName);
        }
        let value =
            values.get(name).filter(|v| !v.trim().is_empty()).ok_or_else(|| RenderError::Missing(name.into()))?;
        out.push_str(value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn paragraphs(text: &str) -> String {
    text.split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<p style=\"margin:0 0 18px\">{}</p>", escape(p.trim()).replace('\n', "<br>")))
        .collect()
}

pub fn render_with_design(
    design: &str,
    subject: &str,
    body: &str,
    recipient: &Recipient<'_>,
) -> Result<Rendered, RenderError> {
    let values = values(recipient);
    let subject = interpolate(subject, &values)?;
    let letter = interpolate(body, &values)?;
    let text = format!("{letter}\n\n--\n{OPT_OUT}");
    let (page, accent, label) = match design {
        "sunrise" => ("#fbf7ef", "#c7771b", "A note from Sun Brewed Homestead"),
        "meadow" => ("#f1f6ef", "#3d7a3a", "Growing community together"),
        "ink" => ("#f3f1ec", "#29404a", "Sun Brewed Homestead · Partnership"),
        "orchard" => ("#fff5ed", "#9b4935", "Rooted locally · Growing together"),
        other => return Err(RenderError::InvalidDesign(other.into())),
    };
    let html = format!(
        "<!doctype html><html><body style=\"margin:0;background:{page};color:#2b2a26;font:16px/1.65 Georgia,ui-serif,serif\"><div style=\"max-width:680px;margin:0 auto;padding:42px 20px\"><div style=\"background:#fff;border:1px solid #ded8cb;border-radius:16px;overflow:hidden;box-shadow:0 12px 32px rgba(43,42,38,.08)\"><div style=\"height:9px;background:{accent}\"></div><div style=\"padding:42px\"><div style=\"color:{accent};font:700 12px/1.4 system-ui,-apple-system,Segoe UI,sans-serif;letter-spacing:.11em;text-transform:uppercase;margin-bottom:28px\">{label}</div>{}<div style=\"border-top:1px solid #e6dfd0;margin-top:32px;padding-top:20px;color:#6f6a5f;font:13px/1.6 system-ui,-apple-system,Segoe UI,sans-serif\">Sowing sustainability, growing community.<br>If another person handles this opportunity, a reply pointing us to them would be deeply appreciated.<br>{OPT_OUT}</div></div></div></div></body></html>",
        paragraphs(&letter)
    );
    Ok(Rendered { subject, text, html })
}

pub fn render(subject: &str, body: &str, recipient: &Recipient<'_>) -> Result<Rendered, RenderError> {
    render_with_design("sunrise", subject, body, recipient)
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("temporary email-provider failure: {0}")]
    Retryable(String),
    #[error("email provider rejected the message: {0}")]
    Permanent(String),
}

#[derive(Clone)]
pub struct ResendClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
    from: String,
    reply_to: String,
}

impl ResendClient {
    pub fn new(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        api_key: String,
        from: String,
        reply_to: String,
    ) -> Self {
        Self { http, endpoint: endpoint.into(), api_key, from, reply_to }
    }

    pub async fn send(
        &self,
        message_id: Uuid,
        to: &str,
        subject: &str,
        text: &str,
        html: &str,
    ) -> Result<String, SendError> {
        #[derive(Serialize)]
        struct Body<'a> {
            from: &'a str,
            to: [&'a str; 1],
            reply_to: &'a str,
            subject: &'a str,
            text: &'a str,
            html: &'a str,
            headers: BTreeMap<&'static str, String>,
        }
        // Mail clients show an "Unsubscribe" button for this header, and
        // Gmail/Yahoo expect it from anyone sending in bulk.
        let headers = BTreeMap::from([("List-Unsubscribe", format!("<mailto:{}?subject=unsubscribe>", self.reply_to))]);
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .header("Idempotency-Key", format!("grant-message/{message_id}"))
            .json(&Body { from: &self.from, to: [to], reply_to: &self.reply_to, subject, text, html, headers })
            .send()
            .await
            .map_err(|e| SendError::Retryable(e.to_string()))?;
        let status = response.status();
        let body = response.text().await.map_err(|e| SendError::Retryable(e.to_string()))?;
        if !status.is_success() {
            let message = format!("HTTP {status}: {}", body.chars().take(300).collect::<String>());
            return if status.is_server_error()
                || matches!(status, StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS)
            {
                Err(SendError::Retryable(message))
            } else {
                Err(SendError::Permanent(message))
            };
        }
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("id")?.as_str().map(str::to_owned))
            .ok_or_else(|| SendError::Retryable("successful response had no email id".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipient<'a>(custom_fields: &'a BTreeMap<String, String>) -> Recipient<'a> {
        Recipient {
            organization: "Valley & Foothill Fund",
            contact_name: "Dr. <Rivera>",
            email: "grants@example.org",
            program_name: "Food Resilience",
            custom_fields,
        }
    }

    #[test]
    fn renders_personalized_plain_text_and_escaped_html() {
        let custom = BTreeMap::from([("amount".into(), "$25,000".into())]);
        let rendered = render(
            "{{organization}} — partnership",
            "Dear {{contact_name}},\n\nWe are applying to {{program_name}} for {{amount}}.",
            &recipient(&custom),
        )
        .expect("render");
        assert_eq!(rendered.subject, "Valley & Foothill Fund — partnership");
        assert!(rendered.text.contains("Dear Dr. <Rivera>"));
        assert!(rendered.html.contains("Dr. &lt;Rivera&gt;"));
        assert!(!rendered.html.contains("Dr. <Rivera>"));
    }

    #[test]
    fn missing_and_malformed_variables_fail_closed() {
        let custom = BTreeMap::new();
        assert_eq!(render("Hi", "{{amount}}", &recipient(&custom)), Err(RenderError::Missing("amount".into())));
        assert_eq!(render("Hi", "{{oops", &recipient(&custom)), Err(RenderError::Unclosed));
        assert_eq!(render("Hi", "{{not valid}}", &recipient(&custom)), Err(RenderError::InvalidName));
    }

    #[test]
    fn every_library_letter_renders_in_its_selected_design() {
        // Letters may ask for extra CSV columns (the thank-you needs {{update}}).
        let custom = BTreeMap::from([("update".to_owned(), "We built a coop.".to_owned())]);
        for template in LETTER_LIBRARY {
            let rendered = render_with_design(template.design, template.subject, template.body, &recipient(&custom))
                .unwrap_or_else(|e| panic!("{}: {e}", template.key));
            assert!(rendered.html.contains("box-shadow"), "{} has the branded letter frame", template.key);
            assert!(rendered.text.contains("Sun Brewed Homestead"), "{} identifies the sender", template.key);
        }
        assert!(matches!(
            render_with_design("neon", "Hi", "Body", &recipient(&custom)),
            Err(RenderError::InvalidDesign(_))
        ));
    }

    #[test]
    fn optional_recipient_names_have_human_fallbacks() {
        let custom = BTreeMap::new();
        let blank = Recipient {
            organization: "Valley Fund",
            contact_name: "",
            email: "grants@example.org",
            program_name: "",
            custom_fields: &custom,
        };
        let rendered =
            render("Hello", "Dear {{contact_name}}, your {{program_name}} matters.", &blank).expect("render");
        assert_eq!(rendered.text, format!("Dear Grants Team, your community investment matters.\n\n--\n{OPT_OUT}"));
    }

    #[test]
    fn every_message_carries_an_opt_out_in_both_copies() {
        let fields = BTreeMap::from([("update".to_owned(), "We built a coop.".to_owned())]);
        for letter in LETTER_LIBRARY {
            let r = render_with_design(letter.design, letter.subject, letter.body, &recipient(&fields))
                .unwrap_or_else(|e| panic!("{}: {e}", letter.key));
            assert!(r.text.ends_with(OPT_OUT), "{}: text", letter.key);
            assert!(
                r.html.contains("Reply &quot;unsubscribe&quot;") || r.html.contains(OPT_OUT),
                "{}: html",
                letter.key
            );
        }
    }

    /// Letters may state the org's legal facts, but only the true ones.
    #[test]
    fn library_letters_state_only_the_real_ein() {
        let campaign = include_str!("../../../zeffy/campaign.toml");
        for letter in LETTER_LIBRARY {
            if let Some(at) = letter.body.find("EIN ") {
                let ein: String = letter.body[at + 4..].chars().take(10).collect();
                assert!(campaign.contains(&format!("EIN {ein}")), "{}: EIN {ein}", letter.key);
            }
            assert!(!letter.body.contains("sunbrewed.org"), "{}: wrong domain", letter.key);
        }
    }

    #[tokio::test]
    async fn resend_delivery_uses_a_stable_idempotency_key() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path("/emails"))
            .and(header("Authorization", "Bearer re_test"))
            .and(header("Idempotency-Key", format!("grant-message/{id}")))
            .and(body_partial_json(serde_json::json!({
                "from": "Sun Brewed <grants@example.org>",
                "to": ["avery@example.org"],
                "reply_to": "hello@example.org",
                "subject": "A proposal",
                "headers": { "List-Unsubscribe": "<mailto:hello@example.org?subject=unsubscribe>" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "email_1"})))
            .expect(1)
            .mount(&server)
            .await;
        let client = ResendClient::new(
            reqwest::Client::new(),
            format!("{}/emails", server.uri()),
            "re_test".into(),
            "Sun Brewed <grants@example.org>".into(),
            "hello@example.org".into(),
        );
        assert_eq!(
            client.send(id, "avery@example.org", "A proposal", "Text", "<p>Text</p>").await.expect("send"),
            "email_1"
        );
    }
}
