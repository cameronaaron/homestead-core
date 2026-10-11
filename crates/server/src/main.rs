//! `homestead [serve | migrate | user-add | setup-link | discord-register | zeffy-verify | buffer-verify
//!            | documenso-verify | systems-sync | discord-provision | linear-provision
//!            | github-provision  [--check | --apply]]`

use std::sync::Arc;

use anyhow::Context;
use homestead_bus::{Dispatcher, Sink};
use homestead_integrations::{discord, http_client, linear};
use homestead_server::{AppState, app, config::Config};

use homestead_store::Store;
use tracing_subscriber::EnvFilter;

mod systems_sync;

/// Discord user ids to append when an event is posted. A lookup failure
/// posts the message with nobody pinged.
struct Pings(Store);

impl discord::MentionSource for Pings {
    fn user_ids<'a>(&'a self, kind: &'a str) -> homestead_bus::BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            match self.0.discord_mentions(kind).await {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!(error = %e, "could not look up Discord pings");
                    Vec::new()
                }
            }
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")))
        .init();
    match std::env::args().nth(1).as_deref().unwrap_or("serve") {
        "serve" => serve().await,
        "migrate" => {
            let url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
            Store::connect(&url).await?.migrate().await?;
            tracing::info!("migrations applied");
            Ok(())
        }
        "discord-register" => {
            let app_id = nonblank("DISCORD_APPLICATION_ID").context("DISCORD_APPLICATION_ID is required")?;
            let token = nonblank("DISCORD_BOT_TOKEN").context("DISCORD_BOT_TOKEN is required")?;
            discord::register_commands(&http_client(), &app_id, &token).await.map_err(anyhow::Error::msg)?;
            tracing::info!("slash commands registered");
            Ok(())
        }
        "discord-provision" => discord_provision(mode()).await,
        "linear-provision" => linear_provision(mode()).await,
        "zeffy-verify" => zeffy_verify().await,
        "buffer-verify" => buffer_verify().await,
        "github-provision" => github_provision(mode()).await,
        "documenso-verify" => documenso_verify().await,
        "systems-sync" => systems_sync::run().await,
        "user-add" => user_add().await,
        "setup-link" => setup_link().await,
        "attachment-restore" => attachment_restore().await,
        "attachments-backup" => attachments_backup().await,
        other => anyhow::bail!(
            "unknown command {other:?}; expected serve, migrate, user-add, setup-link, discord-register, \
             discord-provision, linear-provision, github-provision, zeffy-verify, buffer-verify, systems-sync, \
             attachment-restore or attachments-backup"
        ),
    }
}

async fn serve() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let f = &cfg.file;
    let secrets = &cfg.secrets;
    let store = Store::connect(&secrets.database_url).await.context("connecting to Postgres")?;
    store.migrate().await.context("applying migrations")?;
    let store = store.with_timezone(&f.server.timezone).await.context("[server] timezone")?;

    let client = http_client();
    let routed = |sink: Arc<dyn Sink>, events: &[String]| -> Arc<dyn Sink> {
        Arc::new(homestead_bus::Routed::new(sink, events.iter().cloned()))
    };
    let mut sinks: Vec<Arc<dyn Sink>> = Vec::new();
    let mut linear_pull = None;
    let mut social_feed = None;
    if let (true, Some(url)) = (f.discord.enabled, &secrets.discord_webhook_url) {
        let sink = discord::WebhookSink::new(client.clone(), url.clone())
            .with_mentions(Arc::new(Pings(store.clone())))
            .except_projects(f.discord.deadline_channels.keys().cloned());
        sinks.push(routed(Arc::new(sink), &f.discord.events));
    }
    if let (true, Some(key)) = (f.linear.enabled, &secrets.linear_api_key) {
        let team = linear::provision::Client::new(client.clone(), None, key.clone())
            .fetch(&f.linear.team)
            .await
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("looking up Linear team {}", f.linear.team))?;
        let mut sink = linear::LinearSink::new(
            client.clone(),
            linear::DEFAULT_ENDPOINT.to_owned(),
            key.clone(),
            team.team_id.clone(),
            store.clone(),
        );
        // Finished tasks move to the team's completed state ("Done").
        match team.states.iter().filter(|s| s.kind == "completed").min_by_key(|s| s.name != "Done") {
            Some(done) => sink = sink.with_done_state(done.id.clone()),
            None => tracing::warn!("Linear team {} has no completed state; finished tasks stay open", f.linear.team),
        }
        sinks.push(routed(Arc::new(sink), &f.linear.events));
        if f.linear.pull_secs > 0 {
            let puller = linear::Puller::new(
                client.clone(),
                linear::DEFAULT_ENDPOINT.to_owned(),
                key.clone(),
                f.linear.team.clone(),
            );
            let every = std::time::Duration::from_secs(f.linear.pull_secs);
            linear_pull = Some(Arc::new(homestead_server::linear_pull::LinearPull::new(puller, every)));
        }
    }
    if let (true, Some(key)) = (f.buffer.enabled, &secrets.buffer_api_key) {
        use homestead_integrations::buffer::{Client, DraftsSink, Spec};
        let spec = Spec::parse(&read(&f.buffer.spec)?).map_err(anyhow::Error::msg)?;
        let buffer = Client::new(client.clone(), None, key.clone());
        let org_id = match &f.buffer.organization_id {
            Some(id) => id.clone(),
            None => {
                buffer.organization().await.map_err(anyhow::Error::msg).context("finding the Buffer organization")?.id
            }
        };
        social_feed = Some(Arc::new(homestead_server::feeds::SocialFeed {
            client: Client::new(client.clone(), None, key.clone()),
            organization_id: org_id.clone(),
            cache: Default::default(),
        }));
        sinks.push(routed(Arc::new(DraftsSink::new(buffer, org_id, spec.drafts.hashtags)), &f.buffer.events));
    }
    if sinks.is_empty() {
        tracing::warn!("no integrations enabled in homestead.toml; events will queue until one is");
    }
    if let (true, Some((token, guild))) = (f.discord.enabled, &secrets.discord_reader)
        && !f.discord.deadline_channels.is_empty()
    {
        use homestead_integrations::discord::{ChannelSink, check_deadline_channels, provision::Spec};
        let spec = Spec::parse(&read(&f.discord.spec)?).map_err(anyhow::Error::msg)?;
        check_deadline_channels(&spec, f.discord.deadline_channels.values())
            .map_err(anyhow::Error::msg)
            .context("[discord] deadline_channels")?;
        let routes = f.discord.deadline_channels.clone().into_iter().collect();
        sinks.push(Arc::new(ChannelSink::new(client.clone(), None, token.clone(), guild.clone(), routes)));
    }
    // Signing the welcome agreement grants the role asked for with /onboard.
    if let (true, true, Some((token, guild))) = (f.discord.enabled, f.documenso.enabled, &secrets.discord_reader) {
        use homestead_integrations::discord::{RoleSink, provision::Spec};
        let spec = Spec::parse(&read(&f.discord.spec)?).map_err(anyhow::Error::msg)?;
        for role in homestead_domain::onboarding::OnboardingRole::ALL {
            anyhow::ensure!(
                spec.roles.iter().any(|r| r.name == role.as_str()),
                "/onboard grants {role:?}, which isn't a role in {}",
                f.discord.spec
            );
        }
        sinks.push(Arc::new(RoleSink::new(client.clone(), None, token.clone(), guild.clone())));
    }
    let discord_feed = match &secrets.discord_reader {
        Some((token, guild)) => {
            use homestead_integrations::discord::{feed, provision::Spec};
            let spec = Spec::parse(&read(&f.discord.spec)?).map_err(anyhow::Error::msg)?;
            feed::check_channels(&spec, &f.discord.feed_channels)
                .map_err(anyhow::Error::msg)
                .context("[discord] feed_channels")?;
            Some(Arc::new(homestead_server::feeds::DiscordFeed {
                reader: feed::Reader::new(client.clone(), None, token.clone(), guild.clone()),
                channels: f.discord.feed_channels.clone(),
                cache: Default::default(),
                holders: Default::default(),
            }))
        }
        None => None,
    };
    let discord_join = match (&secrets.discord_reader, &secrets.discord_client_secret, &f.server.public_url) {
        (Some((token, guild)), Some(client_secret), Some(public_url)) if f.discord.enabled && f.documenso.enabled => {
            Some(Arc::new(homestead_server::join::DiscordJoin {
                reader: homestead_integrations::discord::feed::Reader::new(
                    client.clone(),
                    None,
                    token.clone(),
                    guild.clone(),
                ),
                application_id: f.discord.application_id.clone(),
                client_secret: client_secret.clone(),
                redirect_uri: format!("{}/join/discord", public_url.trim_end_matches('/')),
            }))
        }
        _ => None,
    };
    let discord_verifier = match &f.discord.public_key {
        Some(k) if f.discord.enabled => {
            Some(Arc::new(discord::InteractionVerifier::from_hex(k).map_err(anyhow::Error::msg)?))
        }
        _ => None,
    };
    let releases = match (f.documenso.enabled, &secrets.documenso_api_token, &secrets.documenso_webhook_secret) {
        (true, Some(token), Some(secret)) => {
            use homestead_integrations::documenso::{Client, Spec, load_layouts};
            let spec = Spec::parse(&read(&f.documenso.spec)?).map_err(anyhow::Error::msg)?;
            let layouts = load_layouts(&spec, std::path::Path::new(".")).map_err(anyhow::Error::msg)?;
            let mut pdfs = std::collections::BTreeMap::new();
            for r in &spec.releases {
                pdfs.insert(r.key.clone(), std::fs::read(&r.pdf).with_context(|| format!("reading {}", r.pdf))?);
            }
            if !spec.approved {
                tracing::warn!("legal releases are drafts (approved = false); sending is disabled");
            }
            let client = Client::new(client.clone(), &f.documenso.base_url, token.clone());
            Some(Arc::new(homestead_server::releases::Releases {
                client,
                spec,
                pdfs,
                layouts,
                webhook_secret: secret.clone(),
            }))
        }
        _ => None,
    };
    let grant_delivery = match (f.grants.enabled, &secrets.resend_api_key) {
        (true, Some(key)) => Some(Arc::new(homestead_server::grants::GrantDelivery {
            client: homestead_integrations::grants::ResendClient::new(
                client.clone(),
                &f.grants.endpoint,
                key.clone(),
                f.grants.from.clone(),
                f.grants.reply_to.clone(),
            ),
            max_batch: f.grants.max_batch,
        })),
        _ => None,
    };

    let attachments = {
        use homestead_integrations::blobs::Blobs;
        let a = &f.attachments;
        let blobs = match (a.backend.as_str(), &secrets.attachments_key) {
            ("s3", Some((id, secret))) => {
                Some(Blobs::s3(client.clone(), &a.endpoint, &a.bucket, &a.region, id, secret))
            }
            ("filesystem", _) => Some(Blobs::Filesystem(a.path.clone().into())),
            _ => None,
        };
        if blobs.is_none() {
            tracing::warn!("[attachments] is disabled; uploads will be refused");
        }
        blobs.map(|blobs| homestead_server::attachments::Storage {
            blobs,
            max_bytes: a.max_bytes,
            link_secs: a.link_secs,
            retain_deleted_days: a.retain_deleted_days,
        })
    };

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let stop_rx_for_schedules = stop_rx.clone();
    let stop_rx_for_linear = stop_rx.clone();
    let dispatcher = Dispatcher::new(store.clone(), sinks, f.dispatcher());
    let worker = tokio::spawn(dispatcher.run(stop_rx));

    let state = AppState {
        admin_token: secrets.admin_token.clone().map(Into::into),
        secure_cookies: f.server.secure_cookies,
        discord: discord_verifier,
        releases,
        grant_delivery,
        zeffy_webhook_secret: secrets.zeffy_webhook_secret.clone().map(Into::into),
        attachments,
        linear_pull,
        discord_feed,
        discord_join,
        social_feed,
        public_url: f.server.public_url.clone().map(|u| u.trim_end_matches('/').into()),
        pay_links: f.stand.pay_links.iter().map(|l| (l.label.clone(), l.url.clone())).collect(),
        ..AppState::new(store)
    };
    let linear_pulls = homestead_server::linear_pull::spawn(&state, stop_rx_for_linear);
    // Recurring tasks and deadline reminders: on start (a woken container
    // catches up at once), then hourly for as long as it stays awake. Pages
    // also make recurring tasks, so those don't depend on this timer.
    // Reminders do: deploy/cloudflare's daily cron wakes the core for them.
    let schedules = {
        let state = state.clone();
        let mut stop = stop_rx_for_schedules;
        tokio::spawn(async move {
            loop {
                homestead_server::run_schedules(&state).await;
                homestead_server::send_reminders(&state).await;
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {}
                    _ = stop.changed() => break,
                }
            }
        })
    };
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await.with_context(|| format!("binding {}", cfg.bind))?;
    tracing::info!(addr = %cfg.bind, "listening");
    axum::serve(listener, app(state)).with_graceful_shutdown(shutdown_signal()).await?;

    let _ = stop_tx.send(true);
    worker.await.context("dispatcher task")?;
    schedules.await.context("schedule task")?;
    if let Some(pulls) = linear_pulls {
        pulls.await.context("Linear pull task")?;
    }
    Ok(())
}

/// The store for a command-line action, attributed to `cli:<login>`.
async fn cli_store() -> anyhow::Result<Store> {
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let store = Store::connect(&url).await?;
    store.migrate().await?;
    let who = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    Ok(store.acting_as(homestead_store::Actor { id: None, label: format!("cli:{who}") }))
}

/// Prints a fresh one-time setup link for a person.
async fn print_setup_link(store: &Store, actor: &homestead_domain::auth::ActorRecord) -> anyhow::Result<()> {
    use homestead_server::auth::{SETUP_LINK_TTL, new_secret};
    let (token, hash) = new_secret();
    store.issue_setup_link(actor.id, &hash, SETUP_LINK_TTL).await?;
    let base = file_config().ok().and_then(|f| f.server.public_url).unwrap_or_default();
    println!(
        "Setup link for {} (valid {} hours, works once; send it privately):\n  {}/setup/{token}",
        actor.display_name,
        SETUP_LINK_TTL.whole_hours(),
        base.trim_end_matches('/')
    );
    Ok(())
}

/// `homestead user-add <email> <name> <role>[,<role>…]`: adds a person and
/// prints their setup link. This is how the first steward gets in.
async fn user_add() -> anyhow::Result<()> {
    use homestead_domain::auth::{ActorKind, NewActor, Role};
    let args: Vec<String> = std::env::args().skip(2).collect();
    let [email, name, roles] = args.as_slice() else {
        anyhow::bail!(
            "usage: homestead user-add <email> <name> <role>[,<role>…]  (roles: steward, board, farm_lead, crew, volunteer)"
        );
    };
    let roles = roles.split(',').map(|r| r.trim().parse::<Role>()).collect::<Result<Vec<_>, _>>()?;
    let store = cli_store().await?;
    let actor = store
        .create_actor(NewActor {
            display_name: name.clone(),
            email: Some(email.clone()),
            kind: ActorKind::Human,
            roles,
        })
        .await?;
    print_setup_link(&store, &actor).await
}

/// `homestead setup-link <email>`: a new link for someone who has lost
/// their password (their old sessions end when they use it).
async fn setup_link() -> anyhow::Result<()> {
    let email = std::env::args().nth(2).context("usage: homestead setup-link <email>")?;
    let store = cli_store().await?;
    let email = homestead_domain::contacts::normalise_email(&email)?;
    let actor = store
        .list_actors()
        .await?
        .into_iter()
        .find(|a| a.email.as_deref() == Some(email.as_str()))
        .with_context(|| format!("nobody has the email {email}"))?;
    print_setup_link(&store, &actor).await
}

/// `homestead attachment-restore <id>`: undoes an attachment's deletion
/// while its bytes are still retained (`[attachments] retain_deleted_days`).
async fn attachment_restore() -> anyhow::Result<()> {
    let id: uuid::Uuid =
        std::env::args().nth(2).context("usage: homestead attachment-restore <attachment id>")?.parse()?;
    let att = cli_store().await?.restore_attachment(id).await?;
    println!("Restored {} ({} bytes) on {} {}", att.original_name, att.byte_size, att.owner, att.owner_id);
    Ok(())
}

/// `homestead attachments-backup [--verify] [--prune] [--restore]`: copies
/// attachment bytes the backup bucket lacks, checking each against its
/// recorded SHA-256 (see `attachment_backup`). Exits non-zero when a file
/// is lost, corrupt, or survives only in the backup.
async fn attachments_backup() -> anyhow::Result<()> {
    use homestead_integrations::blobs::Blobs;
    use homestead_server::attachment_backup::{Options, run};

    let f = file_config()?;
    let a = &f.attachments;
    anyhow::ensure!(a.backend == "s3", "[attachments] backend must be s3 to back it up");
    anyhow::ensure!(!a.backup_bucket.is_empty(), "set [attachments] backup_bucket in homestead.toml");
    let need = |k: &str| nonblank(k).with_context(|| format!("{k} is required"));
    let bucket = |name: &str, id: &str, secret: &str| -> anyhow::Result<_> {
        match Blobs::s3(http_client(), &a.endpoint, name, &a.region, &need(id)?, &need(secret)?) {
            Blobs::S3(s) => Ok(s),
            _ => unreachable!("Blobs::s3 builds an S3 store"),
        }
    };
    let primary = bucket(&a.bucket, "ATTACHMENTS_ACCESS_KEY_ID", "ATTACHMENTS_SECRET_ACCESS_KEY")?;
    let backup = bucket(&a.backup_bucket, "ATTACHMENTS_BACKUP_ACCESS_KEY_ID", "ATTACHMENTS_BACKUP_SECRET_ACCESS_KEY")?;
    // Read-only use of the database: never migrates it.
    let store = Store::connect(&need("DATABASE_URL")?).await?;
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opts = Options { verify: flag("--verify"), prune: flag("--prune"), restore: flag("--restore") };

    let r = run(&store.accounted_blobs().await?, &primary, &backup, opts).await.map_err(anyhow::Error::msg)?;
    println!(
        "{} files accounted for: {} copied to {}, {} restored, {} verified, {} pruned, {} held by the backup lock",
        r.accounted, r.copied, a.backup_bucket, r.restored, r.verified, r.pruned, r.held_by_lock
    );
    for (label, keys) in [
        ("LOST (in neither bucket)", &r.lost),
        ("CORRUPT (hash mismatch)", &r.corrupt),
        ("only in the backup (run with --restore)", &r.missing_from_primary),
        ("orphaned in primary (no record)", &r.orphans),
    ] {
        for k in keys {
            println!("  {label}: {k}");
        }
    }
    if let Some(why) = &r.prune_skipped {
        println!("  prune skipped: {why}");
    }
    anyhow::ensure!(r.healthy(), "attachment storage needs attention (see above)");
    Ok(())
}

fn read(path: &str) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {path}"))
}

/// `homestead.toml` without secrets: CLI commands only need spec paths.
fn file_config() -> anyhow::Result<homestead_server::config::File> {
    let path = std::env::var("HOMESTEAD_CONFIG").unwrap_or_else(|_| homestead_server::config::DEFAULT_PATH.to_owned());
    homestead_server::config::File::parse(&read(&path)?)
}

/// How a provisioning command runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Print the plan.
    Plan,
    /// Print the plan and fail if it isn't empty (drift detection in CI).
    Check,
    /// Make the changes.
    Apply,
}

fn mode() -> Mode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--apply") {
        Mode::Apply
    } else if args.iter().any(|a| a == "--check") {
        Mode::Check
    } else {
        Mode::Plan
    }
}

/// An env var, treating blank (as `just` loads `KEY=` lines) as unset.
fn nonblank(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Makes the Discord server match `discord/server.toml` (plan by default).
async fn discord_provision(mode: Mode) -> anyhow::Result<()> {
    use discord::provision::{Spec, apply as apply_spec, fetch, http::DiscordHttp, plan};

    let fc = file_config()?;
    let spec_path = fc.discord.spec.clone();
    let text = read(&spec_path)?;
    let spec = Spec::parse(&text).map_err(|e| anyhow::anyhow!("{spec_path} is invalid:\n{e}"))?;

    let app_id = fc.discord.application_id.clone();
    let invite = format!(
        "https://discord.com/oauth2/authorize?client_id={app_id}&scope=bot+applications.commands&permissions=8"
    );
    let (Some(token), Some(guild_id)) = (nonblank("DISCORD_BOT_TOKEN"), nonblank("DISCORD_GUILD_ID")) else {
        anyhow::bail!(
            "set DISCORD_BOT_TOKEN and DISCORD_GUILD_ID in .env.\n\
             1. Create the server in Discord (+ → Create My Own).\n\
             2. Invite the bot with Administrator: {invite}\n\
             3. Server ID: enable Developer Mode, right-click the server → Copy Server ID."
        );
    };
    let api = DiscordHttp::new(http_client(), discord::provision::http::DEFAULT_BASE.to_owned(), token, guild_id);

    if mode != Mode::Apply {
        let state = fetch(&api, &spec).await.map_err(anyhow::Error::msg)?;
        let p = plan(&spec, &state);
        println!("Plan for {:?} ({} change(s)):", state.guild.name, p.actions.len());
        for a in &p.actions {
            println!("  {a}");
        }
        for u in &p.unmanaged {
            println!("  ? unmanaged {u} (left alone)");
        }
        print_pending(&p.pending);
        if !p.actions.is_empty() {
            anyhow::ensure!(
                mode != Mode::Check,
                "Discord has drifted from {spec_path}: {} change(s) pending",
                p.actions.len()
            );
            println!(
                "\nRun with --apply to make these changes. (Some follow-ups, like links to new channels, appear only after the first pass.)"
            );
        }
        return Ok(());
    }

    let mut images = discord::provision::Images::new();
    for path in [&spec.server.icon, &spec.server.bot_avatar].into_iter().flatten() {
        use base64::Engine;
        let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        images.insert(path.clone(), format!("data:image/png;base64,{b64}"));
    }
    let report = apply_spec(&api, &spec, &images).await.map_err(anyhow::Error::msg)?;
    for a in &report.executed {
        println!("  {a}");
    }
    println!("Converged: the server matches {spec_path} ({} change(s) made).", report.executed.len());
    for u in &report.unmanaged {
        println!("  ? unmanaged {u} (left alone)");
    }
    print_pending(&report.pending);
    for (channel, url) in &report.webhooks {
        if std::env::var("GITHUB_ACTIONS").is_ok() {
            println!("#{channel} webhook is in place (URL not printed in CI).");
            continue;
        }
        let env = std::fs::read_to_string(".env").unwrap_or_default();
        match homestead_server::envfile::fill_if_empty(&env, "DISCORD_WEBHOOK_URL", url) {
            Some(updated) => {
                std::fs::write(".env", updated).context("writing .env")?;
                println!("Saved the #{channel} webhook to .env as DISCORD_WEBHOOK_URL.");
            }
            None => println!("DISCORD_WEBHOOK_URL is already set in .env; left unchanged (#{channel} webhook exists)."),
        }
    }
    Ok(())
}

/// Listed Discord members who haven't joined. Not drift: a bot can't add
/// people to a server, so their roles are granted on the first run after
/// they accept an invite.
fn print_pending(pending: &[String]) {
    for u in pending {
        println!("  … @{u} hasn't joined yet; send them an invite and re-run after they accept");
    }
}

/// Makes Linear match `linear/workspace.toml` (plan by default).
async fn linear_provision(mode: Mode) -> anyhow::Result<()> {
    use linear::provision::{Client, Spec, people, plan};

    let text = read(&file_config()?.linear.spec)?;
    let spec = Spec::parse(&text).map_err(|e| anyhow::anyhow!("linear/workspace.toml is invalid:\n{e}"))?;
    let key = nonblank("LINEAR_API_KEY").context("LINEAR_API_KEY is required")?;
    let client = Client::new(http_client(), None, key);
    if mode == Mode::Apply {
        let done = client.apply(&spec).await.map_err(anyhow::Error::msg)?;
        for a in &done {
            println!("  {a}");
        }
        println!(
            "Converged: Linear team {} matches linear/workspace.toml ({} change(s) made).",
            spec.team.key,
            done.len()
        );
        for line in people(&spec, &client.fetch_spec(&spec).await.map_err(anyhow::Error::msg)?) {
            println!("  {line}");
        }
    } else {
        let state = client.fetch_spec(&spec).await.map_err(anyhow::Error::msg)?;
        let actions = plan(&spec, &state);
        println!("Plan for Linear team {} ({} change(s)):", spec.team.key, actions.len());
        for a in &actions {
            println!("  {a}");
        }
        for line in people(&spec, &state) {
            println!("  {line}");
        }
        anyhow::ensure!(
            mode != Mode::Check || actions.is_empty(),
            "Linear has drifted from linear/workspace.toml: {} change(s) pending",
            actions.len()
        );
    }
    Ok(())
}

/// Prints verify findings; returns how many were violations.
fn report(system: &str, findings: &[homestead_integrations::Finding]) -> usize {
    use homestead_integrations::Finding;
    let in_ci = std::env::var("GITHUB_ACTIONS").is_ok();
    let mut violations = 0;
    for f in findings {
        match f {
            Finding::Violation(m) => {
                violations += 1;
                println!("{}{m}", if in_ci { format!("::error title={system}::") } else { "✗ ".to_owned() });
            }
            Finding::Pending(m) => {
                println!("{}{m}", if in_ci { format!("::warning title={system}::") } else { "! ".to_owned() })
            }
            Finding::Note(m) => println!("✓ {m}"),
        }
    }
    violations
}

/// Checks the Buffer account against `buffer/account.toml`. Buffer can't
/// connect channels through its API, so everything missing is a warning.
async fn buffer_verify() -> anyhow::Result<()> {
    use homestead_integrations::buffer::{Client, Spec, verify};
    let path = file_config()?.buffer.spec;
    let spec = Spec::parse(&read(&path)?).map_err(|e| anyhow::anyhow!("{path} is invalid:\n{e}"))?;
    let key = nonblank("BUFFER_API_KEY").context("BUFFER_API_KEY is required")?;
    let org = Client::new(http_client(), None, key).organization().await.map_err(anyhow::Error::msg)?;
    let violations = report("Buffer", &verify(&spec, &org));
    anyhow::ensure!(violations == 0, "{violations} Buffer violation(s)");
    Ok(())
}

/// Checks the release PDFs match the spec and, if a token is set, that the
/// Documenso instance accepts it.
async fn documenso_verify() -> anyhow::Result<()> {
    use homestead_integrations::Finding;
    use homestead_integrations::documenso::{Client, Spec, load_layouts};
    let fc = file_config()?;
    let spec = Spec::parse(&read(&fc.documenso.spec)?)
        .map_err(|e| anyhow::anyhow!("{} is invalid:\n{e}", fc.documenso.spec))?;
    load_layouts(&spec, std::path::Path::new(".")).map_err(anyhow::Error::msg)?;
    let mut findings = Vec::new();
    for r in &spec.releases {
        let pdf = std::fs::read(&r.pdf).with_context(|| format!("reading {}", r.pdf))?;
        anyhow::ensure!(pdf.starts_with(b"%PDF-"), "{} is not a PDF", r.pdf);
        findings.push(Finding::Note(format!("{} ({}) ready", r.title, r.pdf)));
    }
    match (spec.approved, spec.reviewed_by_counsel) {
        (false, _) => findings.push(Finding::Pending(
            "releases are DRAFTS: review legal/*.typ, then set approved = true to allow sending".to_owned(),
        )),
        (true, false) => findings.push(Finding::Pending(
            "releases are approved and sending, but no attorney has reviewed them: have a California attorney \
             review legal/*.typ, then set reviewed_by_counsel = true"
                .to_owned(),
        )),
        (true, true) => findings.push(Finding::Note("releases approved and reviewed by counsel".to_owned())),
    }
    match nonblank("DOCUMENSO_API_TOKEN") {
        Some(token) => {
            let client = Client::new(http_client(), &fc.documenso.base_url, token);
            client.ping().await.map_err(anyhow::Error::msg)?;
            findings.push(Finding::Note(format!("Documenso at {} accepts the API token", fc.documenso.base_url)));
            let openapi = client.openapi().await.map_err(anyhow::Error::msg)?;
            for k in homestead_integrations::documenso::unknown_meta(&spec, &openapi).map_err(anyhow::Error::msg)? {
                findings.push(Finding::Violation(format!(
                    "meta {k:?} isn't a Documenso envelope setting; it would be ignored"
                )));
            }
        }
        None => findings.push(Finding::Pending("DOCUMENSO_API_TOKEN not set; instance not checked".to_owned())),
    }
    let violations = report("Documenso", &findings);
    anyhow::ensure!(violations == 0, "{violations} Documenso violation(s)");
    Ok(())
}

/// Makes the GitHub org match `github/org.toml` (plan by default).
async fn github_provision(mode: Mode) -> anyhow::Result<()> {
    use homestead_integrations::github::{Client, Spec, advisories, plan};
    let path = file_config()?.github.spec;
    let spec = Spec::parse(&read(&path)?).map_err(|e| anyhow::anyhow!("{path} is invalid:\n{e}"))?;
    let token = nonblank("GH_ORG_ADMIN_TOKEN")
        .or_else(|| nonblank("GITHUB_TOKEN"))
        .context("GH_ORG_ADMIN_TOKEN (or GITHUB_TOKEN) is required")?;
    let client = Client::new(http_client(), None, token);
    if mode == Mode::Apply {
        let done = client.apply(&spec).await.map_err(anyhow::Error::msg)?;
        for a in &done {
            println!("  {a}");
        }
        println!("Converged: GitHub org {} matches github/org.toml ({} change(s) made).", spec.org.login, done.len());
    } else {
        let state = client.fetch_spec(&spec).await.map_err(anyhow::Error::msg)?;
        let actions = plan(&spec, &state);
        println!("Plan for GitHub org {} ({} change(s)):", spec.org.login, actions.len());
        for a in &actions {
            println!("  {a}");
        }
        anyhow::ensure!(
            mode != Mode::Check || actions.is_empty(),
            "GitHub has drifted from github/org.toml: {} change(s) pending",
            actions.len()
        );
    }
    let state = client.fetch_spec(&spec).await.map_err(anyhow::Error::msg)?;
    report("GitHub", &advisories(&spec, &state));
    Ok(())
}

/// Checks the live Zeffy campaign against `zeffy/campaign.toml`. Exits
/// non-zero on violations (e.g. a private campaign went live); pending
/// manual steps are printed as GitHub Actions warnings.
async fn zeffy_verify() -> anyhow::Result<()> {
    use homestead_integrations::zeffy::{API_BASE, Spec, list_campaigns, verify};

    let text = read(&file_config()?.zeffy.spec)?;
    let spec = Spec::parse(&text).map_err(|e| anyhow::anyhow!("zeffy/campaign.toml is invalid:\n{e}"))?;
    for f in [&spec.appearance.banner, &spec.appearance.logo] {
        anyhow::ensure!(std::path::Path::new(f).is_file(), "zeffy/campaign.toml refers to missing file {f}");
    }
    let key = nonblank("ZEFFY_API_KEY").context("ZEFFY_API_KEY is required")?;
    let live = list_campaigns(&http_client(), API_BASE, &key).await.map_err(anyhow::Error::msg)?;
    let violations = report("Zeffy", &verify(&spec, &live));
    anyhow::ensure!(violations == 0, "{violations} Zeffy violation(s)");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}
