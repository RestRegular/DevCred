//! Credential types, auto-detection, and format validation.
//!
//! When a user pastes a secret, DevCred sniffs its type by matching well-known
//! prefixes (GitHub `ghp_`, PyPI `pypi-`, npm `npm_`, ...) and validates the
//! format so malformed tokens are flagged immediately.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Well-known credential kinds that DevCred can detect and validate.
///
/// `Custom` holds a user-defined kind name for secrets that don't match any
/// known prefix — auto-detection never produces it, but the user can pick it
/// in the form and it round-trips through the database.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// GitHub personal access / app tokens (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`).
    Github,
    /// PyPI API tokens (`pypi-`).
    Pypi,
    /// npm access tokens (`npm_`).
    Npm,
    /// GitLab personal access tokens (`glpat-`).
    Gitlab,
    /// Docker Hub personal access tokens (`dckr_pat_`).
    Docker,
    /// AWS access key IDs (`AKIA...`) and secret keys.
    Aws,
    /// Stripe secret keys (`sk_live_`, `sk_test_`, `rk_live_`).
    Stripe,
    /// Slack tokens (`xoxb-`, `xoxp-`, `xoxa-`, `xoxr-`).
    Slack,
    /// DigitalOcean tokens (`dop_v1_`, `doo_v1_`, `dor_v1_`).
    DigitalOcean,
    /// Linear API keys (`lin_api_`).
    Linear,
    /// Vercel tokens (`vercel_`).
    Vercel,
    /// 2FA recovery codes (numeric, grouped digits).
    TwoFactorRecovery,
    /// Generic bearer token (`Bearer ...`).
    Bearer,
    /// Unrecognized secret stored as-is.
    #[default]
    Generic,
    /// User-defined kind name (never produced by auto-detection).
    Custom(String),
}

impl CredentialKind {
    /// All variants in display order, for pickers/cycling.
    pub fn all() -> &'static [CredentialKind] {
        &[
            CredentialKind::Github,
            CredentialKind::Pypi,
            CredentialKind::Npm,
            CredentialKind::Gitlab,
            CredentialKind::Docker,
            CredentialKind::Aws,
            CredentialKind::Stripe,
            CredentialKind::Slack,
            CredentialKind::DigitalOcean,
            CredentialKind::Linear,
            CredentialKind::Vercel,
            CredentialKind::TwoFactorRecovery,
            CredentialKind::Bearer,
            CredentialKind::Generic,
        ]
    }
    /// Human-readable label.
    pub fn label(&self) -> &str {
        match self {
            CredentialKind::Github => "GitHub",
            CredentialKind::Pypi => "PyPI",
            CredentialKind::Npm => "npm",
            CredentialKind::Gitlab => "GitLab",
            CredentialKind::Docker => "Docker",
            CredentialKind::Aws => "AWS",
            CredentialKind::Stripe => "Stripe",
            CredentialKind::Slack => "Slack",
            CredentialKind::DigitalOcean => "DigitalOcean",
            CredentialKind::Linear => "Linear",
            CredentialKind::Vercel => "Vercel",
            CredentialKind::TwoFactorRecovery => "2FA Recovery",
            CredentialKind::Bearer => "Bearer",
            CredentialKind::Generic => "Generic",
            CredentialKind::Custom(s) => s,
        }
    }

    /// Suggested environment variable name to inject when running subcommands.
    pub fn env_var(&self) -> &str {
        match self {
            CredentialKind::Github => "GITHUB_TOKEN",
            CredentialKind::Pypi => "TWINE_PASSWORD",
            CredentialKind::Npm => "NODE_AUTH_TOKEN",
            CredentialKind::Gitlab => "GITLAB_TOKEN",
            CredentialKind::Docker => "DOCKER_PASSWORD",
            CredentialKind::Aws => "AWS_SECRET_ACCESS_KEY",
            CredentialKind::Stripe => "STRIPE_SECRET_KEY",
            CredentialKind::Slack => "SLACK_TOKEN",
            CredentialKind::DigitalOcean => "DIGITALOCEAN_ACCESS_TOKEN",
            CredentialKind::Linear => "LINEAR_API_KEY",
            CredentialKind::Vercel => "VERCEL_TOKEN",
            CredentialKind::TwoFactorRecovery => "RECOVERY_CODE",
            CredentialKind::Bearer => "BEARER_TOKEN",
            CredentialKind::Generic => "API_KEY",
            // No sensible default for a user-defined kind.
            CredentialKind::Custom(_) => "API_KEY",
        }
    }
}

impl fmt::Display for CredentialKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Result of sniffing a pasted secret.
#[derive(Debug, Clone)]
pub struct Detection {
    /// Detected kind, or `Generic` if nothing matched.
    pub kind: CredentialKind,
    /// Whether the format looks valid for the detected kind.
    pub valid: bool,
    /// Human-readable note (e.g. reason for invalidity).
    pub note: String,
}

impl Detection {
    fn ok(kind: CredentialKind, note: impl Into<String>) -> Self {
        Detection {
            kind,
            valid: true,
            note: note.into(),
        }
    }

    fn bad(kind: CredentialKind, note: impl Into<String>) -> Self {
        Detection {
            kind,
            valid: false,
            note: note.into(),
        }
    }
}

/// Sniff the credential kind from a raw secret and validate its format.
///
/// Detection is prefix-based for vendor tokens and heuristic for 2FA recovery
/// codes. Anything unrecognized falls back to `Generic`.
pub fn detect(secret: &str) -> Detection {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return Detection::bad(CredentialKind::Generic, "empty secret");
    }

    // GitHub: ghp_ / gho_ / ghu_ / ghs_ / ghr_ (classic) or github_pat_ (fine-grained).
    if let Some(rest) = trimmed
        .strip_prefix("ghp_")
        .or_else(|| trimmed.strip_prefix("gho_"))
        .or_else(|| trimmed.strip_prefix("ghu_"))
        .or_else(|| trimmed.strip_prefix("ghs_"))
        .or_else(|| trimmed.strip_prefix("ghr_"))
        .or_else(|| trimmed.strip_prefix("github_pat_"))
    {
        return validate_len(CredentialKind::Github, rest, 36, "GitHub token");
    }

    // PyPI: pypi- (legacy) — typically "pypi-AgEI..." with a long payload.
    if let Some(rest) = trimmed.strip_prefix("pypi-") {
        return validate_len(CredentialKind::Pypi, rest, 16, "PyPI token");
    }

    // npm: npm_ followed by 36+ chars.
    if let Some(rest) = trimmed.strip_prefix("npm_") {
        return validate_len(CredentialKind::Npm, rest, 36, "npm token");
    }

    // GitLab: glpat- followed by 20+ chars.
    if let Some(rest) = trimmed.strip_prefix("glpat-") {
        return validate_len(CredentialKind::Gitlab, rest, 20, "GitLab token");
    }

    // Docker: dckr_pat_ followed by 27+ chars.
    if let Some(rest) = trimmed.strip_prefix("dckr_pat_") {
        return validate_len(CredentialKind::Docker, rest, 27, "Docker token");
    }

    // Stripe: sk_live_ / sk_test_ / rk_live_
    if let Some(rest) = trimmed
        .strip_prefix("sk_live_")
        .or_else(|| trimmed.strip_prefix("sk_test_"))
        .or_else(|| trimmed.strip_prefix("rk_live_"))
    {
        return validate_len(CredentialKind::Stripe, rest, 24, "Stripe key");
    }

    // Slack: xoxb- / xoxp- / xoxa- / xoxr-
    if trimmed.starts_with("xoxb-")
        || trimmed.starts_with("xoxp-")
        || trimmed.starts_with("xoxa-")
        || trimmed.starts_with("xoxr-")
    {
        return validate_len(CredentialKind::Slack, trimmed, 24, "Slack token");
    }

    // DigitalOcean: dop_v1_ / doo_v1_ / dor_v1_
    if let Some(rest) = trimmed
        .strip_prefix("dop_v1_")
        .or_else(|| trimmed.strip_prefix("doo_v1_"))
        .or_else(|| trimmed.strip_prefix("dor_v1_"))
    {
        return validate_len(CredentialKind::DigitalOcean, rest, 32, "DigitalOcean token");
    }

    // Linear: lin_api_
    if let Some(rest) = trimmed.strip_prefix("lin_api_") {
        return validate_len(CredentialKind::Linear, rest, 32, "Linear key");
    }

    // Vercel: vercel_ (also vercel_ro_ / vercel_rw_)
    if let Some(rest) = trimmed.strip_prefix("vercel_") {
        return validate_len(CredentialKind::Vercel, rest, 24, "Vercel token");
    }

    // AWS access key id: AKIA followed by 16 base32 chars.
    if trimmed.starts_with("AKIA") && trimmed.len() == 20 && is_ascii_alnum_upper(trimmed) {
        return Detection::ok(CredentialKind::Aws, "AWS access key id");
    }

    // Bearer token.
    if let Some(rest) = trimmed.strip_prefix("Bearer ") {
        return validate_len(CredentialKind::Bearer, rest, 16, "Bearer token");
    }

    // 2FA recovery codes: typically 8-10 digits, optionally grouped by dashes/spaces.
    if looks_like_2fa_recovery(trimmed) {
        return Detection::ok(
            CredentialKind::TwoFactorRecovery,
            "2FA recovery code (store one code per entry)",
        );
    }

    Detection::ok(CredentialKind::Generic, "unrecognized secret stored as generic")
}

fn validate_len(kind: CredentialKind, rest: &str, min: usize, label: &str) -> Detection {
    if rest.len() >= min && rest.chars().all(is_token_char) {
        Detection::ok(kind, format!("{label} looks well-formed"))
    } else if rest.len() < min {
        Detection::bad(kind, format!("{label} too short (need ≥{min} chars after prefix)"))
    } else {
        Detection::bad(kind, format!("{label} contains unexpected characters"))
    }
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_ascii_alnum_upper(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Heuristic: a single 2FA recovery code is 6-10 digits, optionally with one
/// separator group (e.g. `1234-5678` or `123 456`).
fn looks_like_2fa_recovery(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    let seps = s
        .chars()
        .filter(|c| *c == '-' || *c == ' ')
        .count();
    let other = s
        .chars()
        .filter(|c| !c.is_ascii_digit() && *c != '-' && *c != ' ')
        .count();
    (6..=10).contains(&digits) && seps <= 1 && other == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_github() {
        let d = detect("ghp_0123456789012345678901234567890123456789");
        assert_eq!(d.kind, CredentialKind::Github);
        assert!(d.valid);
    }

    #[test]
    fn detects_pypi() {
        let d = detect("pypi-AgEIcHlZeS1wcm9qZWN0X3Rva2Vu");
        assert_eq!(d.kind, CredentialKind::Pypi);
        assert!(d.valid);
    }

    #[test]
    fn detects_npm() {
        let d = detect("npm_0123456789012345678901234567890123456789");
        assert_eq!(d.kind, CredentialKind::Npm);
        assert!(d.valid);
    }

    #[test]
    fn detects_aws() {
        let d = detect("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(d.kind, CredentialKind::Aws);
        assert!(d.valid);
    }

    #[test]
    fn detects_2fa() {
        let d = detect("1234-5678");
        assert_eq!(d.kind, CredentialKind::TwoFactorRecovery);
        assert!(d.valid);
    }

    #[test]
    fn flags_short_github() {
        let d = detect("ghp_short");
        assert_eq!(d.kind, CredentialKind::Github);
        assert!(!d.valid);
    }

    #[test]
    fn falls_back_to_generic() {
        let d = detect("some-random-opaque-value");
        assert_eq!(d.kind, CredentialKind::Generic);
        assert!(d.valid);
    }
}
