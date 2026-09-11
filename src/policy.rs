use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::display::sanitize;
use crate::protocol::{AccessMode, KnownApprovalRequest, WebhookDecision};

pub const MAX_SESSION_RULES: usize = 128;
pub const MAX_RULE_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    Path,
    Directory,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PolicyError {
    #[error("session rules require a capability request")]
    NotCapability,
    #[error(
        "rule path must be absolute, contain no parent components or NUL, and fit in {MAX_RULE_PATH_BYTES} bytes"
    )]
    InvalidPath,
    #[error("wildcards must occupy a whole path component")]
    InvalidPattern,
    #[error("session rule limit reached ({MAX_SESSION_RULES})")]
    Capacity,
    #[error("rule must cover the source request path and its exact access mode")]
    DoesNotCoverSource,
    #[error("policy file contains duplicate path/scope/access rule: {0}")]
    DuplicateRule(String),
    #[error("could not read policy file {path}: {detail}")]
    ReadPolicy { path: String, detail: String },
    #[error("invalid policy TOML: {0}")]
    PolicyToml(#[from] toml::de::Error),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFile {
    #[serde(default)]
    pub rules: Vec<RuleDraft>,
}

/// Loads and validates a TOML policy file.
///
/// # Errors
///
/// Returns an error when the file cannot be read, parsed, or contains invalid or duplicate rules.
pub fn load_policy(path: &Path) -> Result<Vec<RuleDraft>, PolicyError> {
    let contents = fs::read_to_string(path).map_err(|source| PolicyError::ReadPolicy {
        path: path.display().to_string(),
        detail: source.to_string(),
    })?;
    let file: PolicyFile = toml::from_str(&contents)?;
    let mut keys = HashSet::new();
    let mut rules = Vec::with_capacity(file.rules.len());
    for draft in file.rules {
        let rule = draft.compile()?;
        let key = (rule.path.clone(), rule.scope, rule.access);
        if !keys.insert(key) {
            return Err(PolicyError::DuplicateRule(rule.path));
        }
        rules.push(RuleDraft {
            action: rule.action,
            path: rule.path,
            scope: rule.scope,
            access: rule.access,
        });
    }
    Ok(rules)
}

/// Editable literal-path rule data, independent of requests, sessions, storage, and UI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDraft {
    pub action: RuleAction,
    pub path: String,
    pub scope: RuleScope,
    pub access: AccessMode,
}

impl RuleDraft {
    /// Creates a draft from a capability without deciding it.
    ///
    /// # Errors
    ///
    /// Rejects non-capability requests and invalid source paths.
    pub fn from_request(
        request: &KnownApprovalRequest,
        action: RuleAction,
        scope: RuleScope,
    ) -> Result<Self, PolicyError> {
        let KnownApprovalRequest::Capability { path, access, .. } = request else {
            return Err(PolicyError::NotCapability);
        };
        components(path)?;
        Ok(Self {
            action,
            path: path.clone(),
            scope,
            access: *access,
        })
    }

    /// Validates and normalizes a draft without consulting the filesystem.
    ///
    /// # Errors
    ///
    /// Rejects relative, parent-traversing, NUL-containing, and oversized paths.
    pub fn compile(&self) -> Result<SessionRule, PolicyError> {
        let parts = components(&self.path)?;
        Ok(SessionRule {
            path: format!("/{}", parts.join("/")),
            access: self.access,
            scope: self.scope,
            action: self.action,
            pattern: PathPattern::literal(&parts, self.scope),
        })
    }

    /// Validates that this draft can be used to decide its source request.
    ///
    /// # Errors
    ///
    /// Rejects invalid drafts, non-capability sources, mismatched access, and non-covering paths.
    pub fn validate_source(&self, request: &KnownApprovalRequest) -> Result<(), PolicyError> {
        let KnownApprovalRequest::Capability { path, access, .. } = request else {
            return Err(PolicyError::NotCapability);
        };
        let rule = self.compile()?;
        if self.access != *access || !rule.pattern.matches(path) {
            return Err(PolicyError::DoesNotCoverSource);
        }
        Ok(())
    }

    /// Returns normalized self/ancestor choices without guessing a project root or scanning.
    ///
    /// # Errors
    ///
    /// Rejects invalid draft paths.
    pub fn ancestors(&self) -> Result<Vec<String>, PolicyError> {
        let mut parts = components(&self.path)?;
        let mut ancestors = Vec::with_capacity(parts.len() + 1);
        loop {
            ancestors.push(format!("/{}", parts.join("/")));
            if parts.pop().is_none() {
                break;
            }
        }
        Ok(ancestors)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Segment {
    Literal(String),
    One,
    Many,
    Descendants,
}

impl Segment {
    fn rank(&self) -> u8 {
        match self {
            Self::Literal(_) => 3,
            Self::One => 2,
            Self::Many => 1,
            Self::Descendants => 0,
        }
    }
}

/// Component glob: `*` consumes one component, `**` one or more, trailing `/` zero or more.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathPattern(Vec<Segment>);

impl PathPattern {
    /// Compiles an absolute component glob without accessing the filesystem.
    ///
    /// # Errors
    ///
    /// Rejects unsafe paths and wildcards embedded in literal components.
    pub fn parse(value: &str) -> Result<Self, PolicyError> {
        let parts = components(value)?;
        let mut segments = Vec::with_capacity(parts.len() + 1);
        for part in parts {
            segments.push(match part {
                "*" => Segment::One,
                "**" => Segment::Many,
                part if part.contains('*') => return Err(PolicyError::InvalidPattern),
                part => Segment::Literal(part.to_owned()),
            });
        }
        if value.ends_with('/') {
            segments.push(Segment::Descendants);
        }
        Ok(Self(segments))
    }

    fn literal(parts: &[&str], scope: RuleScope) -> Self {
        // Observed filenames are literals, never glob source (including names containing '*').
        let mut segments = parts
            .iter()
            .map(|part| Segment::Literal((*part).to_owned()))
            .collect::<Vec<_>>();
        if scope == RuleScope::Directory {
            segments.push(Segment::Descendants);
        }
        Self(segments)
    }

    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        components(path).is_ok_and(|parts| self.matches_components(&parts))
    }

    fn matches_components(&self, parts: &[&str]) -> bool {
        let prefix_len = self
            .0
            .iter()
            .take_while(|part| matches!(part, Segment::Literal(_)))
            .count();
        if parts.len() < prefix_len
            || self.0[..prefix_len]
                .iter()
                .zip(parts)
                .any(|(part, value)| !matches!(part, Segment::Literal(literal) if literal == value))
        {
            return false;
        }
        let remaining = &self.0[prefix_len..];
        let parts = &parts[prefix_len..];
        if remaining.is_empty() {
            return parts.is_empty();
        }
        if remaining == [Segment::Descendants] {
            return true;
        }
        // Dynamic programming avoids recursive glob backtracking on untrusted request paths.
        let mut previous = vec![false; parts.len() + 1];
        previous[0] = true;
        for segment in remaining {
            let mut next = vec![false; previous.len()];
            for index in 0..next.len() {
                next[index] = match segment {
                    Segment::Literal(value) => {
                        index > 0 && previous[index - 1] && parts[index - 1] == value
                    }
                    Segment::One => index > 0 && previous[index - 1],
                    Segment::Many => index > 0 && (previous[index - 1] || next[index - 1]),
                    Segment::Descendants => previous[index] || (index > 0 && next[index - 1]),
                };
            }
            previous = next;
        }
        previous[parts.len()]
    }

    fn specificity(&self, other: &Self) -> Ordering {
        for (left, right) in self.0.iter().zip(&other.0) {
            let ordering = left.rank().cmp(&right.rank());
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        match (self.0.get(other.0.len()), other.0.get(self.0.len())) {
            (Some(Segment::Descendants), None)
            | (None, Some(Segment::Literal(_) | Segment::One | Segment::Many)) => Ordering::Less,
            (None, Some(Segment::Descendants)) | (Some(_), None) => Ordering::Greater,
            _ => Ordering::Equal,
        }
    }
}

fn components(value: &str) -> Result<Vec<&str>, PolicyError> {
    let path = Path::new(value);
    if !path.is_absolute() || value.len() > MAX_RULE_PATH_BYTES || value.contains('\0') {
        return Err(PolicyError::InvalidPath);
    }
    path.components()
        .filter(|part| !matches!(part, Component::RootDir | Component::CurDir))
        .map(|part| match part {
            Component::Normal(value) => value.to_str().ok_or(PolicyError::InvalidPath),
            _ => Err(PolicyError::InvalidPath),
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct SessionRule {
    pub path: String,
    pub access: AccessMode,
    pub scope: RuleScope,
    pub action: RuleAction,
    pattern: PathPattern,
}

impl SessionRule {
    /// Derives a literal rule from the actual pending request, inheriting its exact access mode.
    ///
    /// # Errors
    ///
    /// Rejects non-capability requests and invalid paths.
    pub fn from_request(
        request: &KnownApprovalRequest,
        action: RuleAction,
        scope: RuleScope,
    ) -> Result<Self, PolicyError> {
        RuleDraft::from_request(request, action, scope)?.compile()
    }

    #[must_use]
    pub fn decision(&self) -> WebhookDecision {
        match self.action {
            RuleAction::Allow => WebhookDecision::Granted,
            RuleAction::Deny => WebhookDecision::Denied {
                reason: format!(
                    "denied by session rule \"{}\" ({:?})",
                    sanitize(&self.path),
                    self.scope
                ),
            },
        }
    }
}

#[derive(Default)]
pub struct SessionRules(Vec<SessionRule>);

impl SessionRules {
    #[must_use]
    pub fn drafts(&self) -> Vec<RuleDraft> {
        self.0
            .iter()
            .map(|rule| RuleDraft {
                action: rule.action,
                path: rule.path.clone(),
                scope: rule.scope,
                access: rule.access,
            })
            .collect()
    }
    pub fn replace(&mut self, rules: Vec<SessionRule>) {
        self.0 = rules;
    }
    /// Replaces an identical path/scope/access rule, or inserts a new rule.
    ///
    /// # Errors
    ///
    /// Returns an error when inserting beyond the rule limit; replacements remain possible.
    pub fn insert(&mut self, rule: SessionRule) -> Result<(), PolicyError> {
        if let Some(existing) = self.0.iter_mut().find(|existing| {
            existing.path == rule.path
                && existing.scope == rule.scope
                && existing.access == rule.access
        }) {
            *existing = rule;
        } else {
            if self.0.len() == MAX_SESSION_RULES {
                return Err(PolicyError::Capacity);
            }
            self.0.push(rule);
        }
        Ok(())
    }

    #[must_use]
    pub fn evaluate(&self, request: &KnownApprovalRequest) -> Option<&SessionRule> {
        let KnownApprovalRequest::Capability { path, access, .. } = request else {
            return None;
        };
        let parts = components(path).ok()?;
        self.0
            .iter()
            .filter(|rule| rule.access == *access && rule.pattern.matches_components(&parts))
            .max_by(|left, right| left.pattern.specificity(&right.pattern))
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.0.len()
    }

    pub fn clear(&mut self) -> usize {
        let count = self.count();
        self.0.clear();
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &str, access: AccessMode) -> KnownApprovalRequest {
        KnownApprovalRequest::Capability {
            request_id: "r".to_owned(),
            path: path.to_owned(),
            access,
            reason: None,
            child_pid: 1,
            session_id: "s".to_owned(),
        }
    }

    #[test]
    fn editable_draft_validates_coverage_separately_from_general_rule_validity() {
        let source = request("/work/project/src/main.rs", AccessMode::Read);
        let mut draft =
            RuleDraft::from_request(&source, RuleAction::Allow, RuleScope::Path).unwrap();
        assert!(draft.validate_source(&source).is_ok());
        draft.path = "/work/project".to_owned();
        assert!(draft.compile().is_ok());
        assert_eq!(
            draft.validate_source(&source),
            Err(PolicyError::DoesNotCoverSource)
        );
        draft.scope = RuleScope::Directory;
        assert!(draft.validate_source(&source).is_ok());
        assert_eq!(draft.ancestors().unwrap(), ["/work/project", "/work", "/"]);
        draft.access = AccessMode::ReadWrite;
        assert_eq!(
            draft.validate_source(&source),
            Err(PolicyError::DoesNotCoverSource)
        );
        draft.access = AccessMode::Read;
        draft.path = "/unrelated".to_owned();
        assert_eq!(
            draft.validate_source(&source),
            Err(PolicyError::DoesNotCoverSource)
        );
    }

    fn rule(path: &str, action: RuleAction, scope: RuleScope) -> SessionRule {
        SessionRule::from_request(&request(path, AccessMode::Read), action, scope).unwrap()
    }

    #[test]
    fn component_glob_semantics() {
        for (pattern, path, expected) in [
            ("/a", "/a/", true),
            ("/a", "/a/b", false),
            ("/a/", "/a", true),
            ("/a/", "/a/b/c", true),
            ("/a/", "/ab/c", false),
            ("/a/*", "/a", false),
            ("/a/*", "/a/b", true),
            ("/a/*", "/a/b/c", false),
            ("/a/**", "/a", false),
            ("/a/**", "/a/b", true),
            ("/a/**", "/a/b/c", true),
            ("/a/*/test", "/a/b/test", true),
            ("/a/*/test", "/a/b/c/test", false),
            ("/a/**/test", "/a/test", false),
            ("/a/**/test", "/a/b/c/test", true),
            ("/a/**/**", "/a/b", false),
            ("/a/**/**", "/a/b/c", true),
            ("/", "/", true),
            ("/", "/a", true),
            ("/a/", "/a/../secret", false),
            ("/a/", "a/b", false),
            ("/a/", "/a/\0", false),
            ("/a/", "/A/b", false),
        ] {
            assert_eq!(
                PathPattern::parse(pattern).unwrap().matches(path),
                expected,
                "{pattern} vs {path}"
            );
        }
        for invalid in ["", "a", "/a/../b", "/a\0", "/a/*.log", "/a/b*c"] {
            assert!(PathPattern::parse(invalid).is_err(), "{invalid:?}");
        }
        assert!(PathPattern::parse(&format!("/{}", "a".repeat(MAX_RULE_PATH_BYTES))).is_err());
    }

    #[test]
    fn specificity_and_last_write_are_independent_of_insertion_order() {
        for reverse in [false, true] {
            let mut rules = vec![
                rule("/a", RuleAction::Deny, RuleScope::Directory),
                rule("/a/b", RuleAction::Allow, RuleScope::Directory),
                rule("/a/b/private", RuleAction::Deny, RuleScope::Path),
            ];
            if reverse {
                rules.reverse();
            }
            let mut store = SessionRules::default();
            for rule in rules {
                store.insert(rule).unwrap();
            }
            for (path, action) in [
                ("/a", RuleAction::Deny),
                ("/a/b", RuleAction::Allow),
                ("/a/b/file", RuleAction::Allow),
                ("/a/b/private", RuleAction::Deny),
            ] {
                assert_eq!(
                    store
                        .evaluate(&request(path, AccessMode::Read))
                        .unwrap()
                        .action,
                    action
                );
            }
            assert!(
                store
                    .evaluate(&request("/a/b", AccessMode::ReadWrite))
                    .is_none()
            );
            store
                .insert(rule("/a/b", RuleAction::Deny, RuleScope::Directory))
                .unwrap();
            assert_eq!(store.count(), 3);
            assert_eq!(
                store
                    .evaluate(&request("/a/b", AccessMode::Read))
                    .unwrap()
                    .action,
                RuleAction::Deny
            );
        }
    }

    #[test]
    fn derived_paths_are_literal_and_do_not_expand_variables_or_parent_directories() {
        let mut store = SessionRules::default();
        store
            .insert(rule(
                "/a/*/$HOME/[x]",
                RuleAction::Allow,
                RuleScope::Directory,
            ))
            .unwrap();
        assert!(
            store
                .evaluate(&request("/a/foo/$HOME/[x]", AccessMode::Read))
                .is_none()
        );
        assert!(
            store
                .evaluate(&request("/a/*/$HOME/[x]/child", AccessMode::Read))
                .is_some()
        );
        assert!(
            store
                .evaluate(&request("/a/*/$HOME", AccessMode::Read))
                .is_none()
        );
        assert!(
            store
                .evaluate(&request("/a/*/$HOME/[x]2", AccessMode::Read))
                .is_none()
        );
        store
            .insert(rule("/a//b/./", RuleAction::Deny, RuleScope::Path))
            .unwrap();
        store
            .insert(rule("/a/b", RuleAction::Allow, RuleScope::Path))
            .unwrap();
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn capacity_allows_replacement_and_clear_resets_everything() {
        let mut store = SessionRules::default();
        for index in 0..MAX_SESSION_RULES {
            store
                .insert(rule(
                    &format!("/a/{index}"),
                    RuleAction::Allow,
                    RuleScope::Path,
                ))
                .unwrap();
        }
        assert_eq!(
            store.insert(rule("/extra", RuleAction::Deny, RuleScope::Path)),
            Err(PolicyError::Capacity)
        );
        store
            .insert(rule("/a/0", RuleAction::Deny, RuleScope::Path))
            .unwrap();
        assert_eq!(store.count(), MAX_SESSION_RULES);
        assert_eq!(store.clear(), MAX_SESSION_RULES);
        assert_eq!(store.clear(), 0);
    }

    #[test]
    fn access_modes_are_separate_keys_and_deny_reasons_are_sanitized() {
        let mut store = SessionRules::default();
        for access in [AccessMode::Read, AccessMode::Write, AccessMode::ReadWrite] {
            store
                .insert(
                    SessionRule::from_request(
                        &request("/a\n", access),
                        RuleAction::Deny,
                        RuleScope::Path,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(store.count(), 3);
        let WebhookDecision::Denied { reason } = store
            .evaluate(&request("/a\n", AccessMode::Read))
            .unwrap()
            .decision()
        else {
            panic!("expected denial");
        };
        assert!(!reason.contains('\n'));
        assert!(reason.contains("\\n"));
    }
}
