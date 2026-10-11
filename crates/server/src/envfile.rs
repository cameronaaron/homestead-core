//! Minimal `.env` editing for values the tooling discovers (e.g. a webhook
//! URL created by `discord-provision`), so secrets land in the git-ignored
//! file instead of the terminal.

/// Sets `key=value` if the key is absent or empty. Never overwrites a value
/// someone already put there. Returns `None` when nothing changes.
pub fn fill_if_empty(contents: &str, key: &str, value: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let mut found = false;
    let mut changed = false;
    let mut out: Vec<String> = contents
        .lines()
        .map(|line| {
            if let Some(rest) = line.trim_start().strip_prefix(&prefix) {
                found = true;
                if rest.trim().is_empty() {
                    changed = true;
                    return format!("{prefix}{value}");
                }
            }
            line.to_owned()
        })
        .collect();
    if !found {
        out.push(format!("{prefix}{value}"));
        changed = true;
    }
    changed.then(|| out.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::fill_if_empty;

    #[test]
    fn fills_blank_appends_missing_and_never_overwrites() {
        assert_eq!(fill_if_empty("A=1\nKEY=\n", "KEY", "v").as_deref(), Some("A=1\nKEY=v\n"));
        assert_eq!(fill_if_empty("A=1\n", "KEY", "v").as_deref(), Some("A=1\nKEY=v\n"));
        assert_eq!(fill_if_empty("KEY=existing\n", "KEY", "v"), None);
        assert_eq!(
            fill_if_empty("# KEY= is documented\nMYKEY=\n", "KEY", "v").as_deref(),
            Some("# KEY= is documented\nMYKEY=\nKEY=v\n")
        );
    }
}
