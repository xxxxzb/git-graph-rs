//! Grouping and sorting logic for commit ref badges (branches, remotes, tags, stashes).

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::model::{RefKind, RefLabel};

/// A grouped display badge rendered in the Graph/Message column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefBadge {
    /// Local branch, optionally merged with matching remote-tracking branch(es)
    /// (e.g. `master` + `origin/master` -> `[master | origin]`).
    Branch {
        name: String,
        remotes: Vec<String>,
        is_head: bool,
    },
    /// Standalone remote-tracking branch (including `<remote>/HEAD` or remote branches without a
    /// matching local branch).
    RemoteBranch {
        remote: String,
        name: String,
        is_remote_head: bool,
    },
    /// Tag badge.
    Tag { name: String, annotated: bool },
    /// Stash badge (e.g. `stash@{0}`).
    Stash { selector: String },
}

/// Groups raw [`RefLabel`] entries into merged [`RefBadge`] pills and sorts them for display.
///
/// Grouping rules:
/// - A `RemoteBranch` whose short name (with optional `"<remote>/"` prefix stripped) matches a
///   `LocalBranch` is merged into that `RefBadge::Branch`'s `remotes` list (except `HEAD` pointers
///   like `origin/HEAD`, which always remain standalone `RemoteBranch` badges).
/// - Unmatched remote branches remain standalone `RefBadge::RemoteBranch` badges.
///
/// Ordering:
/// 1. Current `head_branch` first
/// 2. Other local branches (alphabetical)
/// 3. Standalone remote branches (by remote, then branch name)
/// 4. Tags (alphabetical)
/// 5. Stashes (by stash index, then selector)
pub fn group_ref_badges(refs: &[RefLabel], head_branch: Option<&str>) -> Vec<RefBadge> {
    let mut local_branches: HashMap<&str, Vec<String>> = HashMap::new();
    let mut remote_entries: Vec<(&str, &str)> = Vec::new();
    let mut tags: HashMap<&str, bool> = HashMap::new();
    let mut stashes: Vec<&str> = Vec::new();

    for label in refs {
        match &label.kind {
            RefKind::LocalBranch => {
                local_branches.entry(label.name.as_str()).or_default();
            }
            RefKind::RemoteBranch { remote } => {
                let short_name = strip_remote_prefix(&label.name, remote);
                remote_entries.push((remote.as_str(), short_name));
            }
            RefKind::Tag { annotated } => {
                tags.entry(label.name.as_str())
                    .and_modify(|existing| *existing |= *annotated)
                    .or_insert(*annotated);
            }
            RefKind::Stash => {
                if !stashes.contains(&label.name.as_str()) {
                    stashes.push(label.name.as_str());
                }
            }
        }
    }

    let mut standalone_remotes: Vec<RefBadge> = Vec::new();
    for (remote, short_name) in remote_entries {
        let is_remote_head = short_name == "HEAD";
        if !is_remote_head && let Some(remotes) = local_branches.get_mut(short_name) {
            if !remotes.iter().any(|r| r == remote) {
                remotes.push(remote.to_string());
            }
        } else {
            let duplicate = standalone_remotes.iter().any(|badge| {
                matches!(
                    badge,
                    RefBadge::RemoteBranch { remote: r, name: n, .. }
                        if r == remote && n == short_name
                )
            });
            if !duplicate {
                standalone_remotes.push(RefBadge::RemoteBranch {
                    remote: remote.to_string(),
                    name: short_name.to_string(),
                    is_remote_head,
                });
            }
        }
    }

    let mut badges: Vec<RefBadge> = Vec::with_capacity(
        local_branches.len() + standalone_remotes.len() + tags.len() + stashes.len(),
    );

    for (name, mut remotes) in local_branches {
        remotes.sort();
        let is_head = head_branch.is_some_and(|head| head == name);
        badges.push(RefBadge::Branch {
            name: name.to_string(),
            remotes,
            is_head,
        });
    }

    badges.extend(standalone_remotes);

    for (name, annotated) in tags {
        badges.push(RefBadge::Tag {
            name: name.to_string(),
            annotated,
        });
    }

    for selector in stashes {
        badges.push(RefBadge::Stash {
            selector: selector.to_string(),
        });
    }

    badges.sort_by(compare_badges);
    badges
}

fn strip_remote_prefix<'a>(name: &'a str, remote: &str) -> &'a str {
    if !remote.is_empty()
        && let Some(rest) = name.strip_prefix(remote)
        && let Some(short) = rest.strip_prefix('/')
    {
        return short;
    }
    name
}

fn badge_rank(badge: &RefBadge) -> u8 {
    match badge {
        RefBadge::Branch { is_head: true, .. } => 0,
        RefBadge::Branch { is_head: false, .. } => 1,
        RefBadge::RemoteBranch { .. } => 2,
        RefBadge::Tag { .. } => 3,
        RefBadge::Stash { .. } => 4,
    }
}

fn compare_badges(a: &RefBadge, b: &RefBadge) -> Ordering {
    badge_rank(a)
        .cmp(&badge_rank(b))
        .then_with(|| match (a, b) {
            (RefBadge::Branch { name: na, .. }, RefBadge::Branch { name: nb, .. }) => na.cmp(nb),
            (
                RefBadge::RemoteBranch {
                    remote: ra,
                    name: na,
                    is_remote_head: ha,
                },
                RefBadge::RemoteBranch {
                    remote: rb,
                    name: nb,
                    is_remote_head: hb,
                },
            ) => ra.cmp(rb).then_with(|| hb.cmp(ha)).then_with(|| na.cmp(nb)),
            (RefBadge::Tag { name: na, .. }, RefBadge::Tag { name: nb, .. }) => na.cmp(nb),
            (RefBadge::Stash { selector: sa }, RefBadge::Stash { selector: sb }) => {
                parse_stash_index(sa)
                    .cmp(&parse_stash_index(sb))
                    .then_with(|| sa.cmp(sb))
            }
            _ => Ordering::Equal,
        })
}

fn parse_stash_index(selector: &str) -> Option<usize> {
    let inner = selector.strip_prefix("stash@{")?.strip_suffix('}')?;
    inner.parse::<usize>().ok()
}
