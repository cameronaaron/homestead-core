//! Bidirectional reconciliation of the repository's system specs with live
//! integrations.
//!
//! A three-way merge needs the state both sides last agreed on. That is not
//! the previous Git revision: a spec edit the provisioner could not apply
//! (a missing token, a setting only a person can change) is in every recent
//! revision but was never live, and treating it as the ancestor makes the
//! live value look like a new edit and silently reverts the repo's intent.
//! The ancestor is instead a snapshot of what each application actually
//! reported, taken after the last apply (`--record`) and kept as a commit
//! chain under [`BASELINE_REF`]. A live value counts as an edit made in the
//! application only if it differs from that snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Context;
use homestead_integrations::{buffer, discord, github, http_client, linear, zeffy};
use serde_json::Value as Json;
use toml::Value;
use toml_edit::DocumentMut;

use crate::{file_config, nonblank, read};

type Table = toml::map::Map<String, Value>;
type Overlay = Box<dyn Fn(&mut Value) -> anyhow::Result<()>>;
type Validate = fn(&str) -> Result<(), String>;

/// Commit chain of observed live state, one file per spec path. CI fetches
/// and pushes it explicitly; it is not a branch or tag, so nothing deploys
/// or mirrors it.
pub const BASELINE_REF: &str = "refs/homestead/live";

/// The spec files systems-sync reconciles, in the order it visits them.
struct System {
    path: String,
    overlay: Overlay,
    validate: Validate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Show what would be pulled.
    Plan,
    /// Pull live edits into the spec files.
    Write,
    /// Fail if anything would be pulled.
    Check,
    /// Snapshot live state as the next run's common ancestor.
    Record,
}

pub async fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let flags: Vec<Mode> = [("--write", Mode::Write), ("--check", Mode::Check), ("--record", Mode::Record)]
        .into_iter()
        .filter(|(flag, _)| args.iter().any(|a| a == flag))
        .map(|(_, mode)| mode)
        .collect();
    anyhow::ensure!(flags.len() <= 1, "systems-sync takes at most one of --write, --check or --record");
    let mode = flags.first().copied().unwrap_or(Mode::Plan);
    let base = args.iter().position(|a| a == "--base").map(|i| args.get(i + 1).context("--base needs a Git revision"));
    let base = base.transpose()?.map(String::as_str);
    anyhow::ensure!(!(mode == Mode::Record && base.is_some()), "--record writes the baseline; it takes no --base");

    let repo = Path::new(".");
    let systems = fetch_systems().await?;
    if mode == Mode::Record {
        let mut snapshots = Vec::new();
        for system in &systems {
            let local = read(&system.path)?;
            snapshots.push((system.path.clone(), snapshot(&local, &system.overlay)?));
        }
        let message = format!("Live state after {}", git(repo, &["rev-parse", "HEAD"], None)?.trim());
        match record_baseline(repo, &snapshots, &message)? {
            Some(commit) => println!("Recorded live state as {BASELINE_REF} ({commit})."),
            None => println!("Live state is unchanged since the last recorded baseline."),
        }
        return Ok(());
    }

    let mut updates = Vec::new();
    for system in &systems {
        let path = &system.path;
        let base_text = match base {
            Some(reference) => Some(git(repo, &["show", &format!("{reference}:{path}")], None)?),
            None => baseline_text(repo, path)?,
        };
        let Some(base_text) = base_text else {
            println!("· {path}: no recorded live baseline yet, so nothing is pulled and the repository wins");
            continue;
        };
        let local = read(path)?;
        if let Some(pull) = reconcile(path, &base_text, &local, &system.overlay, system.validate)? {
            println!("~ {path}: live changes reconcile into the repository");
            for change in &pull.changes {
                println!("    {change}");
            }
            updates.push((path.clone(), pull.rendered));
        }
    }

    if updates.is_empty() {
        println!("Systems are synchronized with the TOML specs.");
        return Ok(());
    }
    anyhow::ensure!(mode != Mode::Check, "{} system spec(s) need inbound synchronization", updates.len());
    if mode == Mode::Write {
        for (path, rendered) in &updates {
            atomic_write(path, rendered.as_bytes())?;
        }
        println!("Updated {} TOML spec(s) from live state.", updates.len());
    } else {
        println!("Run systems-sync --write to update them.");
    }
    Ok(())
}

/// The first of `keys` that is set. A missing credential skips that system,
/// as the provisioning jobs do: nothing is pulled (the repository wins) and
/// `--record` keeps its previous snapshot, so skipping never loses an edit.
fn credential(path: &str, keys: &[&str]) -> Option<String> {
    let found = keys.iter().find_map(|key| nonblank(key));
    if found.is_none() {
        let prefix = if std::env::var("GITHUB_ACTIONS").is_ok() { "::warning title=systems-sync::" } else { "! " };
        println!("{prefix}{path}: {} is not set, so this system was not synchronized", keys.join(" or "));
    }
    found
}

/// Reads every system's live state once, up front, so a failed read stops
/// the run before any file is touched.
async fn fetch_systems() -> anyhow::Result<Vec<System>> {
    let fc = file_config()?;
    let client = http_client();
    let mut systems = Vec::new();
    'discord: {
        use discord::provision::{Spec, fetch, http::DiscordHttp};
        let path = fc.discord.spec.clone();
        let local = Spec::parse(&read(&path)?).map_err(|e| anyhow::anyhow!("{path} is invalid:\n{e}"))?;
        let (Some(token), Some(guild)) =
            (credential(&path, &["DISCORD_BOT_TOKEN"]), credential(&path, &["DISCORD_GUILD_ID"]))
        else {
            break 'discord;
        };
        let api = DiscordHttp::new(client.clone(), discord::provision::http::DEFAULT_BASE.to_owned(), token, guild);
        let state = fetch(&api, &local).await.map_err(anyhow::Error::msg)?;
        let overlay: Overlay = Box::new(move |v| overlay_discord(v, &state));
        systems.push(System { path, overlay, validate: |text| Spec::parse(text).map(drop) });
    }
    'linear: {
        use linear::provision::{Client, Spec};
        let path = fc.linear.spec.clone();
        let local = Spec::parse(&read(&path)?).map_err(|e| anyhow::anyhow!("{path} is invalid:\n{e}"))?;
        let Some(key) = credential(&path, &["LINEAR_API_KEY"]) else { break 'linear };
        let state = Client::new(client.clone(), None, key).fetch_spec(&local).await.map_err(anyhow::Error::msg)?;
        let overlay: Overlay = Box::new(move |v| overlay_linear(v, &state));
        systems.push(System { path, overlay, validate: |text| Spec::parse(text).map(drop) });
    }
    'github: {
        use github::{Client, Spec};
        let path = fc.github.spec.clone();
        let local = Spec::parse(&read(&path)?).map_err(|e| anyhow::anyhow!("{path} is invalid:\n{e}"))?;
        let Some(token) = credential(&path, &["GH_ORG_ADMIN_TOKEN", "GITHUB_TOKEN"]) else { break 'github };
        let state = Client::new(client.clone(), None, token).fetch_spec(&local).await.map_err(anyhow::Error::msg)?;
        let overlay: Overlay = Box::new(move |v| overlay_github(v, &state));
        systems.push(System { path, overlay, validate: |text| Spec::parse(text).map(drop) });
    }
    'zeffy: {
        use zeffy::{API_BASE, Spec, list_campaigns};
        let path = fc.zeffy.spec.clone();
        let local = Spec::parse(&read(&path)?).map_err(|e| anyhow::anyhow!("{path} is invalid:\n{e}"))?;
        let Some(key) = credential(&path, &["ZEFFY_API_KEY"]) else { break 'zeffy };
        let live = list_campaigns(&client, API_BASE, &key).await.map_err(anyhow::Error::msg)?;
        let title = local.campaign.title;
        let overlay: Overlay = Box::new(move |v| overlay_zeffy(v, &title, &live));
        systems.push(System { path, overlay, validate: |text| Spec::parse(text).map(drop) });
    }
    'buffer: {
        use buffer::{Client, Spec};
        let path = fc.buffer.spec.clone();
        Spec::parse(&read(&path)?).map_err(|e| anyhow::anyhow!("{path} is invalid:\n{e}"))?;
        let Some(key) = credential(&path, &["BUFFER_API_KEY"]) else { break 'buffer };
        let live = Client::new(client, None, key).organization().await.map_err(anyhow::Error::msg)?;
        let overlay: Overlay = Box::new(move |v| overlay_buffer(v, &live));
        systems.push(System { path, overlay, validate: |text| Spec::parse(text).map(drop) });
    }
    Ok(systems)
}

/// What one spec file would gain from live state.
#[derive(Debug)]
struct Pull {
    rendered: String,
    changes: Vec<String>,
}

/// Merges live edits (live vs `base`) into `local`. `None` means live has
/// nothing the repository doesn't already say.
fn reconcile(
    path: &str,
    base_text: &str,
    local_text: &str,
    overlay: impl Fn(&mut Value) -> anyhow::Result<()>,
    validate: Validate,
) -> anyhow::Result<Option<Pull>> {
    let base: Value = toml::from_str(base_text).with_context(|| format!("parsing the {path} baseline"))?;
    let local: Value = toml::from_str(local_text).with_context(|| format!("parsing local {path}"))?;
    let mut remote = base.clone();
    overlay(&mut remote)?;
    let merged = merge(&base, &local, &remote, "").map_err(|paths| {
        anyhow::anyhow!(
            "{path} has repository and live edits to the same value(s) since the last recorded live state: {}",
            paths.join(", ")
        )
    })?;
    if merged == local {
        return Ok(None);
    }
    let rendered = render_preserving_comments(local_text, &local, &merged)?;
    validate(&rendered).map_err(|error| anyhow::anyhow!("live reconciliation produced invalid {path}:\n{error}"))?;
    Ok(Some(Pull { rendered, changes: changes(&local, &merged, "") }))
}

/// The spec as live state currently shows it: repo-only values (comments,
/// write-only fields, objects live doesn't have) kept, readable ones replaced.
fn snapshot(local_text: &str, overlay: impl Fn(&mut Value) -> anyhow::Result<()>) -> anyhow::Result<String> {
    let mut value: Value = toml::from_str(local_text).context("parsing a spec to snapshot")?;
    overlay(&mut value)?;
    toml::to_string(&value).context("encoding a live snapshot")
}

/// Human-readable leaf differences, `path: old → new`.
fn changes(old: &Value, new: &Value, path: &str) -> Vec<String> {
    let child = |key: &str| if path.is_empty() { key.to_owned() } else { format!("{path}.{key}") };
    match (old, new) {
        _ if old == new => vec![],
        (Value::Table(a), Value::Table(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter()
                .flat_map(|key| match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => changes(x, y, &child(key)),
                    (Some(x), None) => vec![format!("{}: {x} → (removed)", child(key))],
                    (None, Some(y)) => vec![format!("{}: (added) → {y}", child(key))],
                    (None, None) => vec![],
                })
                .collect()
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() && a.iter().chain(b).all(Value::is_table) => {
            let ids = identity_key(&[a, b]);
            a.iter()
                .zip(b)
                .enumerate()
                .flat_map(|(index, (x, y))| {
                    let label = match ids.and_then(|key| Some((key, x.get(key)?.as_str()?))) {
                        Some((key, id)) => format!("{path}[{key}={id}]"),
                        None => format!("{path}[{index}]"),
                    };
                    changes(x, y, &label)
                })
                .collect()
        }
        _ => vec![format!("{path}: {old} → {new}")],
    }
}

fn render_preserving_comments(source: &str, old: &Value, new: &Value) -> anyhow::Result<String> {
    let mut document: DocumentMut = source.parse().context("parsing TOML with formatting")?;
    let old = old.as_table().context("old TOML root is not a table")?;
    let new = new.as_table().context("new TOML root is not a table")?;
    patch_edit_table(document.as_table_mut(), old, new)?;
    Ok(document.to_string())
}

fn patch_edit_table(edit: &mut toml_edit::Table, old: &Table, new: &Table) -> anyhow::Result<()> {
    let keys: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
    for key in keys {
        match (old.get(&key), new.get(&key)) {
            (Some(before), Some(after)) if before == after => {}
            (Some(Value::Table(before)), Some(Value::Table(after))) => {
                let item = edit.get_mut(&key).with_context(|| format!("missing TOML item {key}"))?;
                if let Some(table) = item.as_table_mut() {
                    patch_edit_table(table, before, after)?;
                } else {
                    replace_item(edit, &key, &Value::Table(after.clone()))?;
                }
            }
            (Some(Value::Array(before)), Some(Value::Array(after)))
                if before.len() == after.len()
                    && before.iter().all(|v| v.is_table())
                    && after.iter().all(|v| v.is_table()) =>
            {
                let item = edit.get_mut(&key).with_context(|| format!("missing TOML array {key}"))?;
                if let Some(tables) = item.as_array_of_tables_mut() {
                    for (index, (before, after)) in before.iter().zip(after).enumerate() {
                        patch_edit_table(
                            tables.get_mut(index).with_context(|| format!("missing {key}[{index}]"))?,
                            before.as_table().context("array item is not a table")?,
                            after.as_table().context("array item is not a table")?,
                        )?;
                    }
                } else {
                    replace_item(edit, &key, &Value::Array(after.clone()))?;
                }
            }
            (_, Some(after)) => replace_item(edit, &key, after)?,
            (Some(_), None) => {
                edit.remove(&key);
            }
            (None, None) => {}
        }
    }
    Ok(())
}

fn replace_item(edit: &mut toml_edit::Table, key: &str, value: &Value) -> anyhow::Result<()> {
    let mut wrapper = Table::new();
    wrapper.insert("value".to_owned(), value.clone());
    let encoded = toml::to_string(&Value::Table(wrapper)).context("encoding a TOML value")?;
    let mut parsed: DocumentMut = encoded.parse().context("converting a TOML value")?;
    let mut replacement = parsed.remove("value").context("converted TOML value is missing")?;
    if let (Some(existing), Some(replacement_value)) = (edit.get_mut(key), replacement.as_value_mut())
        && let Some(existing_value) = existing.as_value_mut()
    {
        *replacement_value.decor_mut() = existing_value.decor().clone();
        *existing_value = replacement_value.clone();
        return Ok(());
    }
    edit.insert(key, replacement);
    Ok(())
}

fn git(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> anyhow::Result<String> {
    git_env(repo, args, stdin, &[])
}

fn git_env(repo: &Path, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> anyhow::Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .envs(env.iter().copied())
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if let Some(bytes) = stdin {
        child.stdin.take().context("git stdin")?.write_all(bytes)?;
    }
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("git returned non-UTF-8 output")
}

fn resolve(repo: &Path, revision: &str) -> Option<String> {
    git(repo, &["rev-parse", "--verify", "--quiet", revision], None).ok().map(|s| s.trim().to_owned())
}

/// `path` as last recorded under [`BASELINE_REF`], if it has been.
fn baseline_text(repo: &Path, path: &str) -> anyhow::Result<Option<String>> {
    let object = format!("{BASELINE_REF}:{path}");
    if resolve(repo, &format!("{BASELINE_REF}^{{commit}}")).is_none() || resolve(repo, &object).is_none() {
        return Ok(None);
    }
    git(repo, &["show", &object], None).map(Some)
}

/// Commits `files` on top of [`BASELINE_REF`] (keeping any file not listed)
/// and moves the ref, refusing if it moved underneath. Returns the new
/// commit, or `None` when the snapshot is identical to the last one.
fn record_baseline(repo: &Path, files: &[(String, String)], message: &str) -> anyhow::Result<Option<String>> {
    let parent = resolve(repo, &format!("{BASELINE_REF}^{{commit}}"));
    let index = git(repo, &["rev-parse", "--git-path", "homestead-live.index"], None)?;
    let index = repo.join(index.trim());
    let _ = fs::remove_file(&index);
    let index_str = index.to_str().context("index path is not UTF-8")?.to_owned();
    let env = [
        ("GIT_INDEX_FILE", index_str.as_str()),
        ("GIT_AUTHOR_NAME", "homestead-systems[bot]"),
        ("GIT_AUTHOR_EMAIL", "homestead-systems[bot]@users.noreply.github.com"),
        ("GIT_COMMITTER_NAME", "homestead-systems[bot]"),
        ("GIT_COMMITTER_EMAIL", "homestead-systems[bot]@users.noreply.github.com"),
    ];
    let result = (|| -> anyhow::Result<Option<String>> {
        match &parent {
            Some(commit) => git_env(repo, &["read-tree", commit], None, &env)?,
            None => git_env(repo, &["read-tree", "--empty"], None, &env)?,
        };
        for (path, text) in files {
            let blob = git(repo, &["hash-object", "-w", "--stdin"], Some(text.as_bytes()))?;
            git_env(
                repo,
                &["update-index", "--add", "--cacheinfo", &format!("100644,{},{path}", blob.trim())],
                None,
                &env,
            )?;
        }
        let tree = git_env(repo, &["write-tree"], None, &env)?.trim().to_owned();
        if let Some(commit) = &parent
            && resolve(repo, &format!("{commit}^{{tree}}")).as_deref() == Some(tree.as_str())
        {
            return Ok(None);
        }
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
        if let Some(commit) = &parent {
            args.extend(["-p", commit.as_str()]);
        }
        let commit = git_env(repo, &args, None, &env)?.trim().to_owned();
        // The expected old value makes this a compare-and-swap; an empty
        // one asserts the ref doesn't exist yet.
        git(repo, &["update-ref", BASELINE_REF, &commit, parent.as_deref().unwrap_or("")], None)?;
        Ok(Some(commit))
    })();
    let _ = fs::remove_file(&index);
    result
}

fn atomic_write(path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let target = Path::new(path);
    let name = target.file_name().and_then(|x| x.to_str()).context("spec path has no file name")?;
    let temp = target.with_file_name(format!(".{name}.sync-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("creating {}", temp.display()))?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("atomically writing {path}"))
}

/// Three-way merge over TOML values. An array of tables whose items share a
/// unique identity (`name`, `login`, …) merges item by item under that
/// identity, so adding a channel in the repo while someone edits another
/// channel's topic live is not a conflict. Without an identity, arrays are
/// values: any concurrent change to one is a conflict, because matching items
/// by position after a reorder would apply an edit to the wrong object.
fn merge(base: &Value, local: &Value, remote: &Value, path: &str) -> Result<Value, Vec<String>> {
    if local == remote {
        return Ok(local.clone());
    }
    if local == base {
        return Ok(remote.clone());
    }
    if remote == base {
        return Ok(local.clone());
    }
    match (base, local, remote) {
        (Value::Table(b), Value::Table(l), Value::Table(r)) => {
            let keys: BTreeSet<&String> = b.keys().chain(l.keys()).chain(r.keys()).collect();
            let mut out = Table::new();
            let mut conflicts = Vec::new();
            for key in keys {
                let here = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
                match merge_entry(b.get(key), l.get(key), r.get(key), &here) {
                    Ok(Some(v)) => {
                        out.insert(key.clone(), v);
                    }
                    Ok(None) => {}
                    Err(mut e) => conflicts.append(&mut e),
                }
            }
            if conflicts.is_empty() { Ok(Value::Table(out)) } else { Err(conflicts) }
        }
        (Value::Array(b), Value::Array(l), Value::Array(r)) if b.iter().chain(l).chain(r).all(Value::is_table) => {
            match identity_key(&[b, l, r]) {
                Some(key) => merge_keyed(b, l, r, key, path),
                None if b.len() == l.len() && b.len() == r.len() => {
                    let mut out = Vec::with_capacity(b.len());
                    let mut conflicts = Vec::new();
                    for (index, ((bv, lv), rv)) in b.iter().zip(l).zip(r).enumerate() {
                        match merge(bv, lv, rv, &format!("{path}[{index}]")) {
                            Ok(value) => out.push(value),
                            Err(mut errors) => conflicts.append(&mut errors),
                        }
                    }
                    if conflicts.is_empty() { Ok(Value::Array(out)) } else { Err(conflicts) }
                }
                None => Err(vec![path.to_owned()]),
            }
        }
        _ => Err(vec![path.to_owned()]),
    }
}

/// One key or array item across the three sides; `Ok(None)` means absent
/// from the result (deleted on one side, untouched on the other).
fn merge_entry(
    b: Option<&Value>,
    l: Option<&Value>,
    r: Option<&Value>,
    here: &str,
) -> Result<Option<Value>, Vec<String>> {
    match (b, l, r) {
        (Some(bv), Some(lv), Some(rv)) => merge(bv, lv, rv, here).map(Some),
        (None, Some(v), None) | (None, None, Some(v)) => Ok(Some(v.clone())),
        (None, Some(lv), Some(rv)) if lv == rv => Ok(Some(lv.clone())),
        // Live only noticed the key exists (a newcomer listed with no
        // roles); the repository says what it should hold.
        (None, Some(lv), Some(Value::Array(rv))) if rv.is_empty() => Ok(Some(lv.clone())),
        (Some(bv), None, Some(rv)) if bv == rv => Ok(None),
        (Some(bv), Some(lv), None) if bv == lv => Ok(None),
        (Some(_), None, None) | (None, None, None) => Ok(None),
        _ => Err(vec![here.to_owned()]),
    }
}

fn merge_keyed(b: &[Value], l: &[Value], r: &[Value], key: &str, path: &str) -> Result<Value, Vec<String>> {
    let id = |v: &Value| v.get(key).and_then(Value::as_str).map(str::to_owned).unwrap_or_default();
    let index = |items: &[Value]| items.iter().map(|v| (id(v), v.clone())).collect::<BTreeMap<_, _>>();
    let (bi, li, ri) = (index(b), index(l), index(r));
    // The repository's order wins; anything only live has goes last.
    let mut order: Vec<String> = l.iter().map(id).collect();
    order.extend(r.iter().map(id).filter(|x| !li.contains_key(x)));
    order.extend(b.iter().map(id).filter(|x| !li.contains_key(x) && !ri.contains_key(x)));
    let mut out = Vec::new();
    let mut conflicts = Vec::new();
    for item in order {
        match merge_entry(bi.get(&item), li.get(&item), ri.get(&item), &format!("{path}[{key}={item}]")) {
            Ok(Some(v)) => out.push(v),
            Ok(None) => {}
            Err(mut e) => conflicts.append(&mut e),
        }
    }
    if conflicts.is_empty() { Ok(Value::Array(out)) } else { Err(conflicts) }
}

/// The first field that names every item uniquely in all of `arrays`.
fn identity_key(arrays: &[&[Value]]) -> Option<&'static str> {
    ["key", "name", "login", "service", "fund", "kind"].into_iter().find(|key| {
        arrays.iter().all(|items| {
            let ids: Option<Vec<&str>> = items.iter().map(|v| v.as_table()?.get(*key)?.as_str()).collect();
            ids.is_some_and(|ids| ids.iter().collect::<BTreeSet<_>>().len() == ids.len())
        })
    })
}

fn as_table(v: &mut Value) -> anyhow::Result<&mut Table> {
    v.as_table_mut().context("spec root is not a TOML table")
}

fn set(table: &mut Table, key: &str, value: impl Into<Value>) {
    table.insert(key.to_owned(), value.into());
}

fn set_nonempty_or_managed(table: &mut Table, key: &str, value: String) {
    if !value.is_empty() || table.contains_key(key) {
        set(table, key, value);
    }
}

fn update_json_settings(table: &mut Table, live: &Json) {
    for (key, current) in table.iter_mut() {
        if let Some(value) = json_to_toml(&live[key])
            && !toml_to_json(current)
                .is_some_and(|wanted| homestead_integrations::settings::covers(&wanted, &live[key]))
        {
            *current = value;
        }
    }
}

fn update_settings_map(table: &mut Table, live: &BTreeMap<String, Json>) {
    for (key, current) in table.iter_mut() {
        let Some(live_value) = live.get(key) else { continue };
        if !toml_to_json(current).is_some_and(|wanted| homestead_integrations::settings::covers(&wanted, live_value))
            && let Some(value) = json_to_toml(live_value)
        {
            *current = value;
        }
    }
}

fn toml_to_json(v: &Value) -> Option<Json> {
    match v {
        Value::Boolean(x) => Some(Json::Bool(*x)),
        Value::String(x) => Some(Json::String(x.clone())),
        Value::Integer(x) => Some((*x).into()),
        Value::Float(x) => serde_json::Number::from_f64(*x).map(Json::Number),
        Value::Datetime(x) => Some(Json::String(x.to_string())),
        Value::Array(a) => a.iter().map(toml_to_json).collect::<Option<Vec<_>>>().map(Json::Array),
        Value::Table(t) => t
            .iter()
            .map(|(k, v)| Some((k.clone(), toml_to_json(v)?)))
            .collect::<Option<serde_json::Map<_, _>>>()
            .map(Json::Object),
    }
}

fn set_string_set(table: &mut Table, key: &str, values: impl IntoIterator<Item = String>) {
    let values: Vec<String> = values.into_iter().collect();
    let live: std::collections::BTreeSet<_> = values.iter().map(String::as_str).collect();
    let current: std::collections::BTreeSet<_> =
        table.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    if current != live {
        set(table, key, values.into_iter().map(Value::String).collect::<Vec<_>>());
    }
}

fn set_string_set_value(current: &mut Value, values: impl IntoIterator<Item = String>) {
    let values: Vec<String> = values.into_iter().collect();
    let live: std::collections::BTreeSet<_> = values.iter().map(String::as_str).collect();
    let present: std::collections::BTreeSet<_> =
        current.as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if present != live {
        *current = Value::Array(values.into_iter().map(Value::String).collect());
    }
}

fn discord_name_refs(value: &Json, channels: &[discord::provision::ChannelState]) -> Json {
    let Some(id) = value.as_str() else { return value.clone() };
    channels
        .iter()
        .find(|channel| channel.id == id)
        .map_or_else(|| value.clone(), |channel| Json::String(channel.name.clone()))
}

fn update_discord_settings(
    table: &mut Table,
    live: &BTreeMap<String, Json>,
    channels: &[discord::provision::ChannelState],
) {
    let translated: BTreeMap<String, Json> = live
        .iter()
        .map(|(key, value)| {
            let value = if key.ends_with("_channel_id") { discord_name_refs(value, channels) } else { value.clone() };
            (key.clone(), value)
        })
        .collect();
    update_settings_map(table, &translated);
}

fn discord_topic(text: &str, channels: &[discord::provision::ChannelState]) -> String {
    channels.iter().fold(text.to_owned(), |text, channel| {
        text.replace(&format!("<#{}>", channel.id), &format!("<#{}>", channel.name))
    })
}

fn update_linear_settings(table: &mut Table, live: &linear::provision::State) {
    let translated: BTreeMap<String, Json> = live
        .team_settings
        .iter()
        .map(|(key, value)| {
            let value = if key.ends_with("StateId") {
                value
                    .as_str()
                    .and_then(|id| live.states.iter().find(|state| state.id == id))
                    .map_or_else(|| value.clone(), |state| Json::String(state.name.clone()))
            } else {
                value.clone()
            };
            (key.clone(), value)
        })
        .collect();
    update_settings_map(table, &translated);
}

fn json_to_toml(v: &Json) -> Option<Value> {
    match v {
        Json::Bool(x) => Some(Value::Boolean(*x)),
        Json::String(x) => Some(Value::String(x.clone())),
        Json::Number(x) => x.as_i64().map(Value::Integer).or_else(|| x.as_f64().map(Value::Float)),
        Json::Array(a) => a.iter().map(json_to_toml).collect::<Option<Vec<_>>>().map(Value::Array),
        Json::Object(o) => {
            o.iter().map(|(k, v)| Some((k.clone(), json_to_toml(v)?))).collect::<Option<Table>>().map(Value::Table)
        }
        Json::Null => None,
    }
}

fn overlay_discord(v: &mut Value, live: &discord::provision::GuildState) -> anyhow::Result<()> {
    use discord::provision::spec::{CATEGORY_TYPE, PERMISSIONS};
    let root = as_table(v)?;
    let server = root.get_mut("server").and_then(Value::as_table_mut).context("missing [server]")?;
    set(server, "name", live.guild.name.clone());
    set(server, "verification_level", i64::from(live.guild.verification_level));
    set(server, "default_message_notifications", i64::from(live.guild.default_message_notifications));
    set(server, "explicit_content_filter", i64::from(live.guild.explicit_content_filter));
    let system = live
        .guild
        .system_channel_id
        .as_ref()
        .and_then(|id| live.channels.iter().find(|c| &c.id == id))
        .map(|c| c.name.clone());
    match system {
        Some(name) => set(server, "system_channel", name),
        None => {
            server.remove("system_channel");
        }
    }
    let everyone = live.roles.iter().find(|r| r.id == live.guild.id).map(|r| r.permissions).unwrap_or(0);
    set_string_set(
        server,
        "everyone_permissions",
        PERMISSIONS.iter().filter(|(_, bit)| everyone & bit != 0).map(|(name, _)| (*name).to_owned()),
    );
    if let Some(settings) = server.get_mut("settings").and_then(Value::as_table_mut) {
        update_discord_settings(settings, &live.guild.fields, &live.channels);
    }
    if let Some(roles) = root.get_mut("roles").and_then(Value::as_array_mut) {
        for role in roles.iter_mut().filter_map(Value::as_table_mut) {
            let Some(name) = role.get("name").and_then(Value::as_str) else { continue };
            let Some(found) = live.roles.iter().find(|r| r.name == name && !r.managed) else { continue };
            set(role, "color", format!("#{:06X}", found.color));
            set(role, "hoist", found.hoist);
            set(role, "mentionable", found.mentionable);
            set_string_set(
                role,
                "permissions",
                PERMISSIONS
                    .iter()
                    .filter(|(_, bit)| found.permissions & bit != 0)
                    .map(|(permission, _)| (*permission).to_owned()),
            );
        }
    }
    if let Some(categories) = root.get_mut("categories").and_then(Value::as_array_mut) {
        for category in categories.iter_mut().filter_map(Value::as_table_mut) {
            let Some(category_name) = category.get("name").and_then(Value::as_str) else { continue };
            let parent = live.channels.iter().find(|c| c.kind == CATEGORY_TYPE && c.name == category_name);
            let Some(channels) = category.get_mut("channels").and_then(Value::as_array_mut) else { continue };
            for channel in channels.iter_mut().filter_map(Value::as_table_mut) {
                let Some(name) = channel.get("name").and_then(Value::as_str) else { continue };
                let kind = match channel.get("kind").and_then(Value::as_str) {
                    Some("voice") => 2,
                    _ => 0,
                };
                let found = live.channels.iter().find(|c| {
                    c.kind == kind && c.name == name && parent.is_none_or(|p| c.parent_id.as_deref() == Some(&p.id))
                });
                let Some(found) = found else { continue };
                if kind == 0 {
                    set_nonempty_or_managed(
                        channel,
                        "topic",
                        discord_topic(found.topic.as_deref().unwrap_or_default(), &live.channels),
                    );
                }
                if let Some(settings) = channel.get_mut("settings").and_then(Value::as_table_mut) {
                    update_discord_settings(settings, &found.fields, &live.channels);
                }
            }
        }
    }
    let spec_roles: BTreeSet<String> = root
        .get("roles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    if let Some(members) = root.get_mut("members").and_then(Value::as_table_mut) {
        let role_names: BTreeMap<&str, &str> =
            live.roles.iter().filter(|r| !r.managed).map(|r| (r.id.as_str(), r.name.as_str())).collect();
        for (username, roles) in members.iter_mut() {
            let Some(member) = live.members.iter().find(|m| m.username.eq_ignore_ascii_case(username)) else {
                continue;
            };
            set_string_set_value(
                roles,
                member.roles.iter().filter_map(|id| role_names.get(id.as_str())).map(|name| (*name).to_owned()),
            );
        }
        // Everyone in the server is written in, so a steward assigns roles
        // by editing this file, including people who arrived through an old
        // invite with no role at all (`name = []`, which changes nothing).
        // Only spec roles: the provisioner leaves the rest alone.
        let listed: BTreeSet<String> = members.keys().map(|u| u.to_lowercase()).collect();
        for member in live.members.iter().filter(|m| !m.bot && !listed.contains(&m.username.to_lowercase())) {
            let held: Vec<Value> = member
                .roles
                .iter()
                .filter_map(|id| role_names.get(id.as_str()))
                .filter(|name| spec_roles.contains(**name))
                .map(|name| Value::String((*name).to_owned()))
                .collect();
            members.insert(member.username.clone(), Value::Array(held));
        }
    }
    Ok(())
}

fn overlay_linear(v: &mut Value, live: &linear::provision::State) -> anyhow::Result<()> {
    let root = as_table(v)?;
    let team = root.get_mut("team").and_then(Value::as_table_mut).context("missing [team]")?;
    set(team, "name", live.team_name.clone());
    set(team, "description", live.team_description.clone());
    set(team, "timezone", live.timezone.clone());
    if let Some(settings) = team.get_mut("settings").and_then(Value::as_table_mut) {
        update_linear_settings(settings, live);
    }
    if let Some(states) = root.get_mut("states").and_then(Value::as_array_mut) {
        for state in states.iter_mut().filter_map(Value::as_table_mut) {
            let Some(name) = state.get("name").and_then(Value::as_str) else { continue };
            let Some(found) = live.states.iter().find(|x| x.name == name) else { continue };
            set(state, "type", found.kind.clone());
            set(state, "color", found.color.clone());
            set_nonempty_or_managed(state, "description", found.description.clone());
        }
    }
    if let Some(groups) = root.get_mut("label_groups").and_then(Value::as_array_mut) {
        for group in groups.iter_mut().filter_map(Value::as_table_mut) {
            let Some(name) = group.get("name").and_then(Value::as_str).map(str::to_owned) else { continue };
            if let Some(found) = live.labels.iter().find(|x| x.is_group && x.name == name) {
                set(group, "color", found.color.clone());
                set_nonempty_or_managed(group, "description", found.description.clone());
            }
            let Some(labels) = group.get_mut("labels").and_then(Value::as_array_mut) else { continue };
            for label in labels.iter_mut().filter_map(Value::as_table_mut) {
                let Some(label_name) = label.get("name").and_then(Value::as_str) else { continue };
                let Some(found) =
                    live.labels.iter().find(|x| x.name == label_name && x.parent.as_deref() == Some(name.as_str()))
                else {
                    continue;
                };
                set(label, "color", found.color.clone());
                set_nonempty_or_managed(label, "description", found.description.clone());
            }
        }
    }
    if let Some(projects) = root.get_mut("projects").and_then(Value::as_array_mut) {
        for project in projects.iter_mut().filter_map(Value::as_table_mut) {
            let Some(name) = project.get("name").and_then(Value::as_str) else { continue };
            let Some(found) = live.projects.iter().find(|x| x.name == name) else { continue };
            set(project, "status", found.status.clone());
            set(project, "color", found.color.clone());
            set(project, "description", found.description.clone());
        }
    }
    if let Some(members) = root.get_mut("members").and_then(Value::as_table_mut) {
        for (email, role) in members.iter_mut() {
            if let Some(found) = live.users.iter().find(|x| x.email.eq_ignore_ascii_case(email) && x.active) {
                *role = Value::String(found.role.to_string());
            }
        }
    }
    Ok(())
}

fn overlay_github(v: &mut Value, live: &github::State) -> anyhow::Result<()> {
    let root = as_table(v)?;
    let org = root.get_mut("org").and_then(Value::as_table_mut).context("missing [org]")?;
    for (key, live_key) in [
        ("name", "name"),
        ("description", "description"),
        ("blog", "blog"),
        ("location", "location"),
        ("default_repository_permission", "default_repository_permission"),
        ("members_can_create_public_repositories", "members_can_create_public_repositories"),
        ("members_can_create_private_repositories", "members_can_create_private_repositories"),
    ] {
        if let Some(value) = json_to_toml(&live.org[live_key]) {
            org.insert(key.to_owned(), value);
        }
    }
    if let Some(value) = live.org["two_factor_requirement_enabled"].as_bool() {
        set(org, "require_two_factor", value);
    }
    if let Some(settings) = org.get_mut("settings").and_then(Value::as_table_mut) {
        update_json_settings(settings, &live.org);
    }
    if let Some(members) = root.get_mut("members").and_then(Value::as_array_mut) {
        for member in members.iter_mut().filter_map(Value::as_table_mut) {
            let Some(login) = member.get("login").and_then(Value::as_str) else { continue };
            if let Some(role) = live.members.get(&login.to_lowercase()) {
                set(member, "role", role.clone());
            }
        }
    }
    if let Some(teams) = root.get_mut("teams").and_then(Value::as_array_mut) {
        for team in teams.iter_mut().filter_map(Value::as_table_mut) {
            let Some(name) = team.get("name").and_then(Value::as_str) else { continue };
            let Some(found) = live.teams.iter().find(|x| x.name.eq_ignore_ascii_case(name)) else { continue };
            set(team, "description", found.description.clone());
            let listed: std::collections::BTreeSet<String> = ["maintainers", "members"]
                .into_iter()
                .filter_map(|key| team.get(key).and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_lowercase)
                .collect();
            for (key, want_role) in [("maintainers", "maintainer"), ("members", "member")] {
                let Some(current) = team.get_mut(key).and_then(Value::as_array_mut) else { continue };
                *current = found
                    .members
                    .iter()
                    .filter(|(login, role)| role.as_str() == want_role && listed.contains(login.as_str()))
                    .map(|(login, _)| Value::String(login.clone()))
                    .collect();
            }
            if let Some(repos) = team.get_mut("repos").and_then(Value::as_table_mut) {
                for (name, access) in repos.iter_mut() {
                    if let Some(live_access) = found.repos.get(name) {
                        *access = Value::String(live_access.clone());
                    }
                }
            }
        }
    }
    if let Some(repos) = root.get_mut("repos").and_then(Value::as_array_mut) {
        for repo in repos.iter_mut().filter_map(Value::as_table_mut) {
            let Some(name) = repo.get("name").and_then(Value::as_str) else { continue };
            let Some(detail) = live.repo_details.get(name) else { continue };
            if let Some(settings) = repo.get_mut("settings").and_then(Value::as_table_mut) {
                update_json_settings(settings, detail);
            }
        }
    }
    Ok(())
}

fn overlay_zeffy(v: &mut Value, title: &str, live: &[zeffy::LiveCampaign]) -> anyhow::Result<()> {
    let Some(found) = live.iter().find(|x| !x.archived && x.title.trim().eq_ignore_ascii_case(title.trim())) else {
        return Ok(());
    };
    let campaign = as_table(v)?.get_mut("campaign").and_then(Value::as_table_mut).context("missing [campaign]")?;
    set(campaign, "status", if found.status == zeffy::PUBLIC_STATUS { "active" } else { "draft" });
    if let Some(cents) = found.goal_cents {
        anyhow::ensure!(
            cents % 100 == 0,
            "Zeffy campaign goal has fractional dollars; campaign.goal_usd cannot represent it"
        );
        set(campaign, "goal_usd", i64::try_from(cents / 100).context("Zeffy goal is too large")?);
    }
    set(campaign, "currency", found.currency.to_lowercase());
    Ok(())
}

fn overlay_buffer(v: &mut Value, live: &buffer::LiveOrg) -> anyhow::Result<()> {
    let root = as_table(v)?;
    let account = root.get_mut("account").and_then(Value::as_table_mut).context("missing [account]")?;
    set(account, "organization", live.name.clone());
    // Buffer returns channel display names, not stable account handles.
    // Presence is verifiable, but copying those labels into `handle` would
    // turn lossy API output into desired configuration.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Value {
        toml::from_str(text).expect("valid TOML")
    }

    #[test]
    fn three_way_merge_keeps_independent_repo_and_live_edits() {
        let base = t("[x]\na = 1\nb = 2\n");
        let local = t("[x]\na = 3\nb = 2\n");
        let remote = t("[x]\na = 1\nb = 4\n");
        assert_eq!(merge(&base, &local, &remote, "").expect("merge"), t("[x]\na = 3\nb = 4\n"));
    }

    #[test]
    fn three_way_merge_reports_same_field_conflicts() {
        let base = t("[x]\na = 1\n");
        let local = t("[x]\na = 2\n");
        let remote = t("[x]\na = 3\n");
        assert_eq!(merge(&base, &local, &remote, ""), Err(vec!["x.a".to_owned()]));
    }

    #[test]
    fn three_way_merge_combines_independent_fields_inside_array_tables() {
        let base = t("[[things]]\nname = \"one\"\na = 1\nb = 2\n");
        let local = t("[[things]]\nname = \"one\"\na = 3\nb = 2\n");
        let remote = t("[[things]]\nname = \"one\"\na = 1\nb = 4\n");
        assert_eq!(merge(&base, &local, &remote, "").expect("merge"), t("[[things]]\nname = \"one\"\na = 3\nb = 4\n"));
    }

    #[test]
    fn three_way_merge_applies_a_live_edit_to_the_same_named_object_after_a_reorder() {
        let base = t("[[things]]\nname = \"one\"\na = 1\n[[things]]\nname = \"two\"\na = 2\n");
        let local = t("[[things]]\nname = \"two\"\na = 2\n[[things]]\nname = \"one\"\na = 1\n");
        let remote = t("[[things]]\nname = \"one\"\na = 3\n[[things]]\nname = \"two\"\na = 2\n");
        assert_eq!(
            merge(&base, &local, &remote, "").expect("merge"),
            t("[[things]]\nname = \"two\"\na = 2\n[[things]]\nname = \"one\"\na = 3\n")
        );
    }

    #[test]
    fn three_way_merge_refuses_a_reorder_plus_edit_when_items_have_no_identity() {
        let base = t("[[things]]\na = 1\n[[things]]\na = 2\n");
        let local = t("[[things]]\na = 2\n[[things]]\na = 1\n");
        let remote = t("[[things]]\na = 3\n[[things]]\na = 2\n");
        assert_eq!(merge(&base, &local, &remote, ""), Err(vec!["things[0].a".to_owned()]));
    }

    #[test]
    fn three_way_merge_keeps_a_repo_added_object_alongside_a_live_edit() {
        let base = t("[[roles]]\nname = \"Lead\"\ncolor = \"#111111\"\n");
        let local =
            t("[[roles]]\nname = \"Lead\"\ncolor = \"#111111\"\n[[roles]]\nname = \"New\"\ncolor = \"#222222\"\n");
        let remote = t("[[roles]]\nname = \"Lead\"\ncolor = \"#333333\"\n");
        assert_eq!(
            merge(&base, &local, &remote, "").expect("merge"),
            t("[[roles]]\nname = \"Lead\"\ncolor = \"#333333\"\n[[roles]]\nname = \"New\"\ncolor = \"#222222\"\n")
        );
    }

    #[test]
    fn three_way_merge_refuses_to_drop_an_object_that_was_edited_live() {
        let base =
            t("[[roles]]\nname = \"Lead\"\ncolor = \"#111111\"\n[[roles]]\nname = \"Old\"\ncolor = \"#111111\"\n");
        let local = t("[[roles]]\nname = \"Lead\"\ncolor = \"#111111\"\n");
        let remote =
            t("[[roles]]\nname = \"Lead\"\ncolor = \"#111111\"\n[[roles]]\nname = \"Old\"\ncolor = \"#999999\"\n");
        assert_eq!(merge(&base, &local, &remote, ""), Err(vec!["roles[name=Old]".to_owned()]));
    }

    fn github_spec(permission: &str) -> String {
        format!(
            "[org]\nname = \"Sun Brewed Homestead\"\ndescription = \"\"\nblog = \"\"\nlocation = \"\"\n\
             default_repository_permission = \"{permission}\" # proprietary code\n\
             members_can_create_public_repositories = false\nmembers_can_create_private_repositories = true\n\
             require_two_factor = true\n"
        )
    }

    fn github_live(name: &str, permission: &str) -> github::State {
        github::State {
            org: serde_json::json!({
                "name": name, "description": "", "blog": "", "location": "",
                "default_repository_permission": permission,
                "members_can_create_public_repositories": true,
                "members_can_create_private_repositories": true,
                "two_factor_requirement_enabled": false
            }),
            ..Default::default()
        }
    }

    /// The defect this module once had: with the previous Git revision as
    /// the ancestor, hardening that was never applied (no admin token; 2FA
    /// is a person's job) looked like a live edit and was reverted.
    #[test]
    fn repo_intent_that_never_went_live_is_not_reverted() {
        let local = github_spec("none");
        let live = github_live("Sun Brewed Homestead", "read");
        let overlay = |v: &mut Value| overlay_github(v, &live);
        let old_ancestor = reconcile("org.toml", &local, &local, overlay, |_| Ok(())).expect("reconcile");
        assert!(old_ancestor.is_some(), "the previous revision as ancestor must reproduce the defect");

        let baseline = snapshot(&local, overlay).expect("snapshot");
        assert!(reconcile("org.toml", &baseline, &local, overlay, |_| Ok(())).expect("reconcile").is_none());
    }

    #[test]
    fn an_edit_made_in_the_application_after_the_baseline_is_pulled() {
        let local = github_spec("none");
        let before = github_live("Sun Brewed Homestead", "read");
        let baseline = snapshot(&local, |v: &mut Value| overlay_github(v, &before)).expect("snapshot");
        let after = github_live("Sunbrewed Homestead", "read");
        let pull = reconcile("org.toml", &baseline, &local, |v: &mut Value| overlay_github(v, &after), |_| Ok(()))
            .expect("reconcile")
            .expect("a live rename is pulled");
        assert!(pull.rendered.contains("name = \"Sunbrewed Homestead\""));
        assert!(pull.rendered.contains("default_repository_permission = \"none\" # proprietary code"));
        assert!(pull.rendered.contains("require_two_factor = true"), "pending intent survives the pull");
        assert_eq!(pull.changes, ["org.name: \"Sun Brewed Homestead\" → \"Sunbrewed Homestead\""]);
    }

    #[test]
    fn a_live_change_to_a_value_the_repo_still_wants_changed_is_a_conflict() {
        let local = github_spec("none");
        let before = github_live("Sun Brewed Homestead", "read");
        let baseline = snapshot(&local, |v: &mut Value| overlay_github(v, &before)).expect("snapshot");
        let after = github_live("Sun Brewed Homestead", "write");
        let error = reconcile("org.toml", &baseline, &local, |v: &mut Value| overlay_github(v, &after), |_| Ok(()))
            .expect_err("both sides moved");
        assert!(error.to_string().contains("org.default_repository_permission"), "{error}");
    }

    fn temp_repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("homestead-sync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        git(&dir, &["init", "-q"], None).expect("git init");
        dir
    }

    #[test]
    fn the_baseline_is_a_commit_chain_that_keeps_unlisted_files() {
        let repo = temp_repo("baseline");
        assert_eq!(baseline_text(&repo, "a.toml").expect("read"), None, "no baseline until one is recorded");

        let first = [("a.toml".to_owned(), "x = 1\n".to_owned()), ("dir/b.toml".to_owned(), "y = 1\n".to_owned())];
        let c1 = record_baseline(&repo, &first, "one").expect("record").expect("first commit");
        assert_eq!(record_baseline(&repo, &first, "same").expect("record"), None, "no empty commits");

        let c2 = record_baseline(&repo, &[("a.toml".to_owned(), "x = 2\n".to_owned())], "two")
            .expect("record")
            .expect("second commit");
        assert_eq!(baseline_text(&repo, "a.toml").expect("read").as_deref(), Some("x = 2\n"));
        assert_eq!(baseline_text(&repo, "dir/b.toml").expect("read").as_deref(), Some("y = 1\n"));
        assert_eq!(git(&repo, &["rev-parse", &format!("{c2}^")], None).expect("parent").trim(), c1);
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn rendering_live_values_preserves_the_human_spec_comments() {
        let source =
            "# guide\n[x]\n# why this is one\na = 1 # units\n\n[[things]]\nname = \"kept\"\nvalue = 2 # more units\n";
        let old = t(source);
        let new = t("[x]\na = 3\n[[things]]\nname = \"kept\"\nvalue = 4\n");
        let rendered = render_preserving_comments(source, &old, &new).expect("render");
        assert!(rendered.contains("# guide"));
        assert!(rendered.contains("# why this is one"));
        assert!(rendered.contains("a = 3 # units"));
        assert!(rendered.contains("value = 4 # more units"));
    }

    #[test]
    fn buffer_overlay_changes_only_live_readable_fields() {
        let mut spec = t(
            "[account]\norganization = \"Old\"\n[[channels]]\nservice = \"instagram\"\nhandle = \"old\"\n[drafts]\nhashtags = [\"#kept\"]\n",
        );
        let live = buffer::LiveOrg {
            id: "org".into(),
            name: "New".into(),
            channels: vec![buffer::LiveChannel { service: "instagram".into(), name: "@new".into(), paused: true }],
        };
        overlay_buffer(&mut spec, &live).expect("overlay");
        assert_eq!(spec["account"]["organization"].as_str(), Some("New"));
        assert_eq!(spec["channels"][0]["handle"].as_str(), Some("old"));
        assert_eq!(spec["drafts"]["hashtags"][0].as_str(), Some("#kept"));
    }

    #[test]
    fn discord_overlay_preserves_permission_order_when_the_bits_match() {
        use discord::provision::{BotUser, GuildInfo, GuildState, RoleState};
        let mut spec = t(
            "[server]\nname = \"Old\"\nverification_level = 0\ndefault_message_notifications = 0\nexplicit_content_filter = 0\nsystem_channel = \"welcome\"\neveryone_permissions = [\"send_messages\", \"view_channel\"]\n[server.settings]\nafK = 1\nroles = []\ncategories = []\n[members]\n",
        );
        let live = GuildState {
            guild: GuildInfo {
                id: "guild".into(),
                name: "New".into(),
                verification_level: 1,
                default_message_notifications: 1,
                explicit_content_filter: 2,
                system_channel_id: None,
                icon: None,
                fields: Default::default(),
            },
            bot: BotUser { id: "bot".into(), avatar: None },
            roles: vec![RoleState {
                id: "guild".into(),
                name: "@everyone".into(),
                permissions: discord::provision::spec::VIEW | (1 << 11),
                color: 0,
                hoist: false,
                mentionable: false,
                position: 0,
                managed: false,
                bot_id: None,
            }],
            channels: vec![],
            messages: Default::default(),
            webhooks: Default::default(),
            members: vec![],
        };
        overlay_discord(&mut spec, &live).expect("overlay");
        assert_eq!(spec["server"]["name"].as_str(), Some("New"));
        assert_eq!(
            spec["server"]["everyone_permissions"].as_array().expect("permissions"),
            &vec![Value::String("send_messages".into()), Value::String("view_channel".into())]
        );
    }

    #[test]
    fn discord_overlay_writes_in_every_unlisted_person() {
        use discord::provision::{BotUser, GuildInfo, GuildState, MemberState, RoleState};
        let mut spec = t(
            "[server]\nname = \"S\"\nverification_level = 1\ndefault_message_notifications = 1\nexplicit_content_filter = 2\n[[roles]]\nname = \"Volunteer\"\n[[roles]]\nname = \"Steward\"\n[members]\n\"Listed.One\" = [\"Steward\"]\n",
        );
        let role = |id: &str, name: &str, managed: bool| RoleState {
            id: id.into(),
            name: name.into(),
            permissions: 0,
            color: 0,
            hoist: false,
            mentionable: false,
            position: 1,
            managed,
            bot_id: None,
        };
        let member = |username: &str, roles: &[&str], bot: bool| MemberState {
            user_id: format!("id-{username}"),
            username: username.into(),
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            bot,
        };
        let live = GuildState {
            guild: GuildInfo {
                id: "guild".into(),
                name: "S".into(),
                verification_level: 1,
                default_message_notifications: 1,
                explicit_content_filter: 2,
                system_channel_id: None,
                icon: None,
                fields: Default::default(),
            },
            bot: BotUser { id: "bot".into(), avatar: None },
            roles: vec![role("v", "Volunteer", false), role("s", "Steward", false), role("x", "Hand-made", false)],
            channels: vec![],
            messages: Default::default(),
            webhooks: Default::default(),
            members: vec![
                member("listed.one", &["s"], false),
                member("new.volunteer", &["v", "x"], false),
                member("no_roles_yet", &[], false),
                member("only_outside_roles", &["x"], false),
                member("some_bot", &["v"], true),
            ],
        };
        overlay_discord(&mut spec, &live).expect("overlay");
        let members = spec["members"].as_table().expect("members");
        let names: Vec<&str> = members.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["Listed.One", "new.volunteer", "no_roles_yet", "only_outside_roles"],
            "listed once, case-insensitively; people without roles too; no bots"
        );
        assert_eq!(members["new.volunteer"], Value::Array(vec![Value::String("Volunteer".into())]));
        assert_eq!(members["no_roles_yet"], Value::Array(vec![]), "listed so a steward can assign roles");
        assert_eq!(members["only_outside_roles"], Value::Array(vec![]), "roles outside the spec aren't written");
    }

    #[test]
    fn a_steward_can_assign_roles_to_a_newcomer_before_the_sync_lists_them() {
        let base = "[members]\n\"Listed.One\" = [\"Steward\"]\n";
        let local = "[members]\n\"Listed.One\" = [\"Steward\"]\nnewcomer = [\"Volunteer\"]\n";
        let pull = reconcile(
            "server.toml",
            base,
            local,
            |v: &mut Value| {
                v["members"].as_table_mut().expect("members").insert("newcomer".into(), Value::Array(vec![]));
                Ok(())
            },
            |_| Ok(()),
        )
        .expect("no conflict: live listing someone with no roles says nothing");
        assert!(pull.is_none(), "the repository's roles stand");
        // A real live grant still conflicts with a different repository edit.
        let conflict = reconcile(
            "server.toml",
            base,
            local,
            |v: &mut Value| {
                let crew = Value::Array(vec![Value::String("Crew".into())]);
                v["members"].as_table_mut().expect("members").insert("newcomer".into(), crew);
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(conflict.is_err());
    }

    #[test]
    fn linear_overlay_treats_equal_integer_and_float_settings_as_converged() {
        let mut spec =
            t("[team]\nname = \"A\"\ndescription = \"D\"\ntimezone = \"UTC\"\n[team.settings]\nmonths = 6\n");
        let live = linear::provision::State {
            team_name: "A".into(),
            team_description: "D".into(),
            timezone: "UTC".into(),
            team_settings: BTreeMap::from([("months".into(), serde_json::json!(6.0))]),
            ..Default::default()
        };
        assert!(homestead_integrations::settings::covers(
            &toml_to_json(&spec["team"]["settings"]["months"]).expect("JSON"),
            &serde_json::json!(6.0)
        ));
        overlay_linear(&mut spec, &live).expect("overlay");
        assert_eq!(spec["team"]["settings"]["months"], Value::Integer(6));
    }

    #[test]
    fn linear_overlay_keeps_human_state_references() {
        let mut spec = t(
            "[team]\nname = \"A\"\ndescription = \"D\"\ntimezone = \"UTC\"\n[team.settings]\nautoCloseStateId = \"Canceled\"\n",
        );
        let live = linear::provision::State {
            team_name: "A".into(),
            team_description: "D".into(),
            timezone: "UTC".into(),
            states: vec![linear::provision::LiveState {
                id: "state-id".into(),
                name: "Canceled".into(),
                kind: "canceled".into(),
                color: "#000000".into(),
                description: String::new(),
            }],
            team_settings: BTreeMap::from([("autoCloseStateId".into(), Json::String("state-id".into()))]),
            ..Default::default()
        };
        overlay_linear(&mut spec, &live).expect("overlay");
        assert_eq!(spec["team"]["settings"]["autoCloseStateId"].as_str(), Some("Canceled"));
    }

    #[test]
    fn discord_topics_are_serialized_with_channel_names_not_ids() {
        let channels = vec![discord::provision::ChannelState {
            id: "123".into(),
            name: "rules".into(),
            kind: 0,
            parent_id: None,
            topic: None,
            position: 0,
            overwrites: vec![],
            fields: Default::default(),
        }];
        assert_eq!(discord_topic("Read <#123>.", &channels), "Read <#rules>.");
    }

    #[test]
    fn github_overlay_only_updates_objects_already_managed_by_the_spec() {
        let mut spec = t(
            "[org]\nname = \"Old\"\ndescription = \"\"\nblog = \"\"\nlocation = \"\"\ndefault_repository_permission = \"none\"\nmembers_can_create_public_repositories = false\nmembers_can_create_private_repositories = true\nrequire_two_factor = false\n[[members]]\nlogin = \"listed\"\nrole = \"member\"\n",
        );
        let live = github::State {
            org: serde_json::json!({
                "name": "New", "description": "", "blog": "", "location": "",
                "default_repository_permission": "none",
                "members_can_create_public_repositories": false,
                "members_can_create_private_repositories": true,
                "two_factor_requirement_enabled": true
            }),
            members: BTreeMap::from([("listed".into(), "admin".into()), ("unmanaged".into(), "member".into())]),
            ..Default::default()
        };
        overlay_github(&mut spec, &live).expect("overlay");
        assert_eq!(spec["org"]["name"].as_str(), Some("New"));
        assert_eq!(spec["members"].as_array().expect("members").len(), 1);
        assert_eq!(spec["members"][0]["role"].as_str(), Some("admin"));
    }

    #[test]
    fn zeffy_overlay_refuses_unrepresentable_fractional_goal() {
        let mut spec = t("[campaign]\ntitle = \"Grow\"\nstatus = \"draft\"\ngoal_usd = 1\ncurrency = \"usd\"\n");
        let live = vec![zeffy::LiveCampaign {
            id: "one".into(),
            title: "Grow".into(),
            status: "active".into(),
            url: String::new(),
            goal_cents: Some(125),
            currency: "USD".into(),
            archived: false,
        }];
        assert!(overlay_zeffy(&mut spec, "Grow", &live).is_err());
    }
}
