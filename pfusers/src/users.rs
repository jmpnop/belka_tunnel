//! Typed representation of a pfSense user row plus the PHP templates that
//! drive each CRUD operation through `pfSsh.php`. Built so the GUI layer
//! never sees raw PHP — it talks to `pfsh::PfSshClient` which delegates to
//! these templates.
//!
//! Reference for field semantics: pfSense 2.8.1's
//! `/etc/inc/auth.inc` (local_user_set, local_user_set_password,
//! local_user_set_groups, local_user_del).

use serde::{Deserialize, Serialize};

/// A pfSense user row. Mirrors the JSON shape emitted by our LIST_USERS_PHP
/// template, which in turn is a curated projection of
/// `$config['system']['user'][N]`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PfUser {
    pub name: String,
    #[serde(default)]
    pub descr: String,
    pub uid: i64,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub priv_list: Vec<String>,
    /// Decoded text of `$user['authorizedkeys']` — one OpenSSH-format line
    /// per public key.
    #[serde(default)]
    pub authorized_keys: String,
    #[serde(default)]
    pub has_bcrypt: bool,
    #[serde(default)]
    pub has_sha512: bool,
    #[serde(default)]
    pub has_legacy_md5: bool,
}

fn default_scope() -> String {
    "user".to_string()
}

impl PfUser {
    /// True if `user-shell-access` or `page-all` is granted — pfSense's
    /// `local_user_set` uses the same rule to assign `/bin/tcsh` instead of
    /// `/sbin/nologin`.
    pub fn has_shell_access(&self) -> bool {
        self.priv_list
            .iter()
            .any(|p| p == "user-shell-access" || p == "page-all")
    }

    /// True if the recorded password hash uses bcrypt or sha512. The
    /// distinction surfaces in the UI so Pasha can flag legacy md5 hashes.
    #[allow(dead_code)]
    pub fn has_password(&self) -> bool {
        self.has_bcrypt || self.has_sha512 || self.has_legacy_md5
    }

    /// Count of authorized_keys lines (non-empty, non-blank).
    pub fn key_count(&self) -> usize {
        self.authorized_keys
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    }
}

/// Canonical pfSense privilege strings the UI surfaces. Curated from
/// `/etc/inc/priv.defs.inc` + `/etc/inc/priv/user.priv.inc`; the full set
/// (~1500 entries) is enumerable live on the router but pfUsers exposes
/// the day-to-day subset.
pub const CANONICAL_PRIVILEGES: &[(&str, &str)] = &[
    ("page-all", "Web UI — full access (admin-equivalent)"),
    ("page-dashboard-all", "Web UI — dashboard"),
    ("page-system-usermanager", "Web UI — manage users"),
    (
        "page-system-usermanager-passwordmg",
        "Web UI — change own password",
    ),
    (
        "page-diagnostics-command",
        "Web UI — shell-exec page (root-equivalent)",
    ),
    (
        "page-diagnostics-backup-restore",
        "Web UI — backup/restore (root-equivalent)",
    ),
    ("user-shell-access", "SSH — interactive shell (/bin/tcsh)"),
    ("user-copy-files", "SSH — SCP/SFTP system-wide"),
    ("user-copy-files-chroot", "SSH — SCP/SFTP chrooted to home"),
    ("user-ssh-tunnel", "SSH — port-forward only, no shell"),
    ("user-ipsec-xauth-dialin", "VPN — IPsec xauth"),
    ("user-l2tp-dialin", "VPN — L2TP dial-in"),
    ("user-pppoe-dialin", "VPN — PPPoE dial-in"),
    (
        "user-config-readonly",
        "Config — block writes from this user",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_access_detection() {
        let mut u = PfUser {
            name: "olga".into(),
            descr: "".into(),
            uid: 2000,
            scope: "user".into(),
            expires: None,
            disabled: false,
            groups: vec![],
            priv_list: vec![],
            authorized_keys: "".into(),
            has_bcrypt: false,
            has_sha512: false,
            has_legacy_md5: false,
        };
        assert!(!u.has_shell_access());
        u.priv_list.push("user-shell-access".into());
        assert!(u.has_shell_access());
        u.priv_list.clear();
        u.priv_list.push("page-all".into());
        assert!(u.has_shell_access(), "page-all also implies shell");
    }

    #[test]
    fn key_count_counts_lines() {
        let u = PfUser {
            name: "x".into(),
            descr: "".into(),
            uid: 1,
            scope: "user".into(),
            expires: None,
            disabled: false,
            groups: vec![],
            priv_list: vec![],
            authorized_keys: "ssh-ed25519 AAA…\n\nssh-rsa BBB…\n  \nssh-ecdsa CCC…\n".into(),
            has_bcrypt: false,
            has_sha512: false,
            has_legacy_md5: false,
        };
        assert_eq!(u.key_count(), 3);
    }
}
