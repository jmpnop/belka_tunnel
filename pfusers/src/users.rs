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

/// Compact summary of what a user can do over SSH, used by the sidebar
/// dot + pill so the visual state matches reality (e.g., a tunnel-only
/// user is still "active SSH" even though they have no interactive shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAccess {
    /// `user-shell-access` or `page-all` — pfSense assigns `/bin/tcsh`.
    Shell,
    /// `user-ssh-tunnel` — `/usr/local/sbin/ssh_tunnel_shell`, port-forward
    /// only, no exec. Still SSH; the SOCKS5 / BelkaTunnel use case.
    Tunnel,
    /// `user-copy-files` or `user-copy-files-chroot` — SCP/SFTP only,
    /// `/usr/local/{bin/scponly,sbin/scponlyc}`.
    Scp,
    /// None of the SSH-granting privileges — pfSense assigns
    /// `/sbin/nologin` and `pw lock`s the account.
    None,
}

impl PfUser {
    /// Classify the user's SSH access into the single mode pfSense's
    /// `local_user_set` priv-ladder would pick — Shell ⊃ Tunnel ⊃ Scp ⊃ None.
    /// Order mirrors auth.inc:706 so the answer matches what `pw`
    /// actually configured on the OS side.
    pub fn ssh_access(&self) -> SshAccess {
        let p = &self.priv_list;
        if p.iter()
            .any(|x| x == "user-shell-access" || x == "page-all")
        {
            SshAccess::Shell
        } else if p
            .iter()
            .any(|x| x == "user-copy-files-chroot" || x == "user-copy-files")
        {
            SshAccess::Scp
        } else if p.iter().any(|x| x == "user-ssh-tunnel") {
            SshAccess::Tunnel
        } else {
            SshAccess::None
        }
    }

    /// True if the user has ANY SSH-granting privilege — green-dot semantic
    /// for the sidebar. Distinct from `ssh_access() == Shell` because a
    /// tunnel-only user is still actively using SSH (the SOCKS5 case).
    pub fn has_ssh_access(&self) -> bool {
        !matches!(self.ssh_access(), SshAccess::None)
    }

    /// True if `user-shell-access` or `page-all` is granted — pfSense's
    /// `local_user_set` uses the same rule to assign `/bin/tcsh` instead of
    /// `/sbin/nologin`. Currently only the test suite needs this directly
    /// (the GUI matches on `ssh_access()` to also surface Tunnel/Scp).
    #[allow(dead_code)]
    pub fn has_shell_access(&self) -> bool {
        matches!(self.ssh_access(), SshAccess::Shell)
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
    fn ssh_access_classification_matches_priv_ladder() {
        let mut u = PfUser {
            name: "x".into(),
            descr: "".into(),
            uid: 1,
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
        assert_eq!(u.ssh_access(), SshAccess::None);
        assert!(!u.has_ssh_access());

        u.priv_list = vec!["user-ssh-tunnel".into()];
        assert_eq!(u.ssh_access(), SshAccess::Tunnel);
        assert!(u.has_ssh_access(), "tunnel should count as ssh access");
        assert!(!u.has_shell_access(), "tunnel is NOT interactive shell");

        u.priv_list = vec!["user-copy-files".into()];
        assert_eq!(u.ssh_access(), SshAccess::Scp);
        assert!(u.has_ssh_access());
        assert!(!u.has_shell_access());

        // page-all implies shell per pfSense's priv ladder, regardless of
        // what other priv strings are present.
        u.priv_list = vec!["page-all".into(), "user-ssh-tunnel".into()];
        assert_eq!(u.ssh_access(), SshAccess::Shell);

        // user-shell-access wins over a co-present tunnel grant.
        u.priv_list = vec!["user-shell-access".into(), "user-ssh-tunnel".into()];
        assert_eq!(u.ssh_access(), SshAccess::Shell);
    }

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
