//! The Terms of Service and Privacy Policy, public at `/terms` and
//! `/privacy` and linked from every page's footer. Discord asks for both
//! links before an app can be listed or verified.
//!
//! Every claim here describes the code as it is: what each form stores, the
//! one cookie, the Discord OAuth scopes. Change the text in the same commit
//! as the behaviour it describes; `tests/legal.rs` pins the facts most
//! likely to drift.

use axum::Router;
use axum::routing::get;
use maud::{Markup, html};

use crate::AppState;
use crate::ui::layout;
use homestead_domain::gifts::{ORG_EIN, ORG_LEGAL_NAME, ORG_MAILING_ADDRESS};

/// When the text last changed in substance.
pub const EFFECTIVE: &str = "September 30, 2026";

/// The contact address published on sunbrewedhomestead.org.
pub const CONTACT_EMAIL: &str = "sunbrewedhomestead@gmail.com"; // oss-publish:allow: the org's published contact

/// Reachable without signing in: people read these before they have an account.
pub fn public_routes() -> Router<AppState> {
    Router::new().route("/terms", get(terms)).route("/privacy", get(privacy))
}

fn contact() -> Markup {
    html! {
        p {
            (ORG_LEGAL_NAME) ", " (ORG_MAILING_ADDRESS) ". Email "
            a href={ "mailto:" (CONTACT_EMAIL) } { (CONTACT_EMAIL) } "."
        }
    }
}

async fn terms() -> Markup {
    layout(
        "Terms of Service",
        html! {
            div.legal {
                p.muted { "Effective " (EFFECTIVE) }
                p {
                    "These terms cover the Sun Brewed Homestead app: this website, the join and adoption "
                    "pages, the QR tags around the farm, and our Discord bot. "
                    (ORG_LEGAL_NAME) " (\"the Homestead\", \"we\") is a California nonprofit and 501(c)(3) "
                    "public charity, EIN " (ORG_EIN) ". By using the app you agree to these terms and to our "
                    a href="/privacy" { "Privacy Policy" } "."
                }

                h2 { "Accounts" }
                p {
                    "Accounts are by invitation: a steward adds you and sends a one-time setup link. "
                    "Keep your password to yourself, and tell us if you think someone else has used your account. "
                    "What your account can see and do depends on your role. We may change your role, or suspend "
                    "or close your account, for example when you leave a program or break these terms."
                }

                h2 { "Using the app" }
                p { "Please:" }
                ul {
                    li { "give true information, and only your own (or information you have permission to share);" }
                    li { "use only your own account, and only for Homestead activities;" }
                    li { "not try to get around access controls, probe for weaknesses, overload the service or scrape it;" }
                    li { "treat other people's details you see here as confidential, as your volunteer agreement asks." }
                }
                p {
                    "In our Discord server, Discord's own "
                    a href="https://discord.com/terms" { "Terms of Service" } " and "
                    a href="https://discord.com/guidelines" { "Community Guidelines" }
                    " apply alongside these terms."
                }

                h2 { "Agreements you sign" }
                p {
                    "Some steps, like joining as a volunteer, ask you to sign an agreement (for example a volunteer "
                    "agreement, liability waiver or photo release). Those are separate documents. Where one of them "
                    "and these terms disagree, the signed agreement wins for what it covers."
                }

                h2 { "What you add" }
                p {
                    "You keep ownership of what you enter (notes, reports, photos, files). You let the Homestead store, "
                    "use and share it as needed to run its programs, as the Privacy Policy describes. Don't upload "
                    "anything you don't have the right to share."
                }

                h2 { "Adoption applications and joining" }
                p {
                    "Applying to adopt an animal doesn't guarantee an adoption; we decide each application ourselves. "
                    "Joining through the join page makes you a volunteer once you've signed the agreement; "
                    "stewards may change that role later."
                }

                h2 { "Other services" }
                p {
                    "Parts of the app work through services we don't run, such as Discord, and donations go through "
                    "Zeffy. Their own terms and privacy policies apply to what you do on them."
                }

                h2 { "No warranty" }
                p {
                    "The app is provided free, \"as is\" and \"as available\". We work to keep it running and your "
                    "information safe, but we can't promise it will always be available or free of errors. "
                    "To the extent the law allows, the Homestead is not liable for indirect or consequential "
                    "losses from using the app, and its total liability for any claim about the app is limited to $100."
                }

                h2 { "Changes" }
                p {
                    "We may update these terms. The date at the top changes when we do, and for a significant change "
                    "we will tell members in Discord. Continuing to use the app after a change means you accept it."
                }

                h2 { "Law" }
                p { "These terms are governed by the laws of California and the United States." }

                h2 { "Contact" }
                (contact())
            }
        },
    )
}

async fn privacy() -> Markup {
    layout(
        "Privacy Policy",
        html! {
            div.legal {
                p.muted { "Effective " (EFFECTIVE) }
                p {
                    "This policy explains what the Sun Brewed Homestead app (this website, the join and adoption "
                    "pages, the QR tags around the farm, and our Discord bot) collects, why, and who can see it. "
                    "It's run by " (ORG_LEGAL_NAME) ", a nonprofit. We don't sell or rent personal information, "
                    "show ads, or use analytics or tracking scripts."
                }

                h2 { "What we collect" }
                dl {
                    dt { "Joining from the join page" }
                    dd {
                        "Your name and email, and the agreement you sign. When you connect Discord we receive your "
                        "Discord user ID and username and keep those two. We ask Discord only for "
                        code { "identify" } " and " code { "guilds.join" } " (not your email or your servers). "
                        "The access token Discord gives us is used once to add you to our server with the Volunteer "
                        "role, and is not kept."
                    }
                    dt { "Adoption applications" }
                    dd { "Your name, email, phone (optional), what you tell us, and which animal you asked about." }
                    dt { "QR tags" }
                    dd {
                        "When you scan a tag without signing in: the name you type, and what you record "
                        "(signing in or out at the gate, borrowing or returning a tool, reporting a problem, "
                        "eggs collected, compost added), with the time."
                    }
                    dt { "Accounts" }
                    dd {
                        "Your name, email, role, and a password stored only as an Argon2id hash. "
                        "What you do while signed in (tasks, hours, notes, files) is recorded with your name."
                    }
                    dt { "Discord commands" }
                    dd {
                        "When you use one of our slash commands, Discord sends us your user ID and username "
                        "to tell whose request it is."
                    }
                    dt { "Mailing list" }
                    dd {
                        "Your email, name if given, interests, where you signed up, and when you agreed and "
                        "when you unsubscribed. Once you unsubscribe, an import can't add you back."
                    }
                    dt { "Donations" }
                    dd {
                        "Online donations are made on Zeffy, which handles payment. We record the gifts we receive "
                        "(donor name, email, and the amount or what was given) for thank-you and "
                        "tax-acknowledgment letters."
                    }
                }

                h2 { "Cookies" }
                p {
                    "One cookie, " code { "sbh_session" } ", set only when you sign in. It keeps you signed in "
                    "for up to 30 days, can't be read by scripts, and isn't sent to other sites. "
                    "No advertising or analytics cookies."
                }

                h2 { "How we use it" }
                p {
                    "To run the farm and its programs: organizing volunteers and tasks, processing adoptions, "
                    "sending the agreements you sign, keeping records a nonprofit must keep (such as donation "
                    "acknowledgments), and keeping the app secure. We email you only about things you asked for "
                    "or took part in, or because you're on the mailing list."
                }

                h2 { "Who can see it" }
                p {
                    "Inside the Homestead, people see only what their role needs: for example, adoption "
                    "applications are visible to people who manage animals, and the mailing list to people who "
                    "manage contacts. Every change is recorded with who made it."
                }
                p { "We use these services to run the app, and they handle data only on our behalf:" }
                ul {
                    li { b { "Cloudflare" } ": hosting, and storage of files you upload." }
                    li { b { "Supabase" } " (on Amazon Web Services in the United States): the database." }
                    li { b { "Discord" } ": our community server. Members' Discord usernames and roles are also kept in our private configuration on " b { "GitHub" } "." }
                    li { b { "Documenso" } " (a copy we run ourselves): sending and signing agreements." }
                    li { b { "Linear" } ": farm task tracking, which can include who a task is assigned to." }
                    li { b { "Resend" } ": delivering email we send." }
                }
                p {
                    "We share information outside the Homestead only when the law requires it, to protect "
                    "someone's safety, or with your permission."
                }

                h2 { "How long we keep it" }
                p {
                    "We keep records while they're needed for the program they belong to, and longer where the law "
                    "or a signed agreement requires (for example, signed agreements and donation records). "
                    "Deleted files are removed from storage after 30 days. For accountability, the app keeps a "
                    "change history, and a deleted record's last contents stay in that history."
                }

                h2 { "Security" }
                p {
                    "The app is served only over HTTPS. Passwords, sign-in sessions, setup links and join links "
                    "are stored as hashes, not in readable form. Access is limited by role, and every change is recorded."
                }

                h2 { "Your choices" }
                p {
                    "You can ask us for a copy of your information, to correct it, or to delete it, by emailing "
                    a href={ "mailto:" (CONTACT_EMAIL) } { (CONTACT_EMAIL) }
                    ". We'll do what we can; some records, like signed agreements and donation records, we "
                    "may have to keep. Ask us to take you off the mailing list at any time; an unsubscribe is permanent."
                }

                h2 { "Children" }
                p {
                    "The app isn't meant for children under 13, and we don't knowingly collect their information. "
                    "If you think we have, email us and we'll delete it."
                }

                h2 { "Changes" }
                p {
                    "We'll update this policy when what we collect or how we use it changes. The date at the "
                    "top changes when we do, and for a significant change we'll tell members in Discord."
                }

                h2 { "Contact" }
                (contact())
            }
        },
    )
}
