//! Native Rust pfSense user management. No PHP on the wire — we read and
//! write `/cf/conf/config.xml` directly over SSH, then apply the OS-side
//! side effects (`pw` for passwd entries, the on-disk authorized_keys file)
//! via channel-exec.
//!
//! ## Design
//!
//! pfSense stores all configuration as `<pfsense>...</pfsense>` XML at
//! `/cf/conf/config.xml`, owned root:wheel mode 600. The on-disk file is
//! the authoritative source of truth: pfSense's parser re-loads it on
//! demand and on every `parse_config(true)`. The OS-level passwd database
//! (`/etc/passwd`, `/etc/master.passwd`) is downstream — pfSense's
//! `local_user_set` materialises rows there from the XML, but pfSense
//! itself never reads passwd back. So we treat:
//!
//!   * `config.xml` as the source of truth for everything user-related
//!     (name, descr, uid, priv, authorized_keys field, bcrypt-hash).
//!   * `pw` / `/home/<name>/.ssh/authorized_keys` as derived artifacts
//!     that need to be kept in lockstep on the OS side. We invoke them
//!     directly via SSH exec.
//!
//! ## XML round-trip strategy
//!
//! Round-tripping config.xml through a parser + serializer risks losing
//! whitespace / element ordering / attribute formatting that pfSense's
//! parser tolerates but a future Rust serializer might emit differently.
//! To minimise that surface:
//!
//!   1. We parse the entire document into a `Doc` (event tree from
//!      quick-xml) that records bytes verbatim where possible.
//!   2. We mutate ONLY the `<system>/<user>` subtree (and `<system>/<group>`
//!      for membership edits). All other nodes survive byte-for-byte.
//!   3. We serialize back via quick-xml's Writer, only touching the user/
//!      group sections.
//!
//! ## Password hashing
//!
//! pfSense's web-UI auth checks `bcrypt-hash` first, falls back to
//! `sha512-hash`, then legacy `md5-hash`. We always write `bcrypt-hash`
//! with cost=10 (pfSense default), removing the other two fields. The
//! `bcrypt` crate's `hash` function produces the exact `$2y$10$…` format
//! pfSense expects.
//!
//! ## What is NOT in scope here
//!
//!   * pfSense's config history (`/cf/conf/backup/config-…xml`). Web UI
//!     adds a snapshot per `write_config()`; we don't, so the History
//!     page won't show our edits. Acceptable trade-off; the user can
//!     still revert via a manual restore.
//!   * The revision counter (`<revision>` block at the top of
//!     `config.xml`). pfSense doesn't fail if it's stale; the Web UI
//!     just shows a slightly older mtime in the version banner.

use crate::ssh::{exec_command, ClientHandle};
use crate::users::PfUser;
use anyhow::{anyhow, bail, Context, Result};
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Reader;
use quick_xml::Writer;
use std::io::Cursor;

const CONFIG_PATH: &str = "/cf/conf/config.xml";

/// One pfSense user, in the shape config.xml emits. Distinct from `PfUser`
/// (which is what the GUI / list endpoint consumes) so we can keep the
/// XML-level invariants close to the parser.
#[derive(Debug, Clone)]
struct UserRow {
    name: String,
    descr: String,
    uid: u32,
    scope: String,
    expires: Option<String>,
    disabled: bool,
    priv_list: Vec<String>,
    authorized_keys_b64: String,
    bcrypt_hash: Option<String>,
    sha512_hash: Option<String>,
    md5_hash: Option<String>,
}

// ---------- Public API ----------

pub async fn list_users(h: &ClientHandle) -> Result<Vec<PfUser>> {
    let xml = read_config_xml(h).await?;
    let doc = Doc::parse(&xml)?;
    let rows = doc.list_users()?;
    let groups_by_uid = doc.groups_by_uid()?;
    let users = rows
        .into_iter()
        .map(|r| {
            let groups = groups_by_uid.get(&r.uid).cloned().unwrap_or_default();
            row_to_pfuser(r, groups)
        })
        .collect();
    Ok(users)
}

pub struct AddUserReq<'a> {
    pub name: &'a str,
    pub descr: &'a str,
    pub password: &'a str,
    pub priv_list: Vec<String>,
    pub groups: Vec<String>,
    pub authorized_keys: &'a str,
}

pub async fn add_user(h: &ClientHandle, req: AddUserReq<'_>) -> Result<()> {
    if req.name.is_empty() {
        bail!("name required");
    }
    if req.password.is_empty() {
        bail!("password required");
    }
    validate_username(req.name)?;

    let xml = read_config_xml(h).await?;
    let mut doc = Doc::parse(&xml)?;
    if doc.find_user(req.name).is_some() {
        bail!("user already exists: {}", req.name);
    }
    let next_uid = doc.next_uid()?;
    let bcrypt = bcrypt::hash(req.password, 10).context("hashing password")?;
    let row = UserRow {
        name: req.name.to_string(),
        descr: req.descr.to_string(),
        uid: next_uid,
        scope: "user".to_string(),
        expires: None,
        disabled: false,
        priv_list: req.priv_list.clone(),
        authorized_keys_b64: base64_encode(req.authorized_keys.as_bytes()),
        bcrypt_hash: Some(bcrypt),
        sha512_hash: None,
        md5_hash: None,
    };
    doc.append_user(&row)?;
    doc.set_next_uid(next_uid + 1)?;
    doc.add_to_group("all", next_uid)?;
    if !req.groups.is_empty() {
        doc.set_user_groups(next_uid, &req.groups)?;
    }
    let new_xml = doc.serialize()?;
    write_config_xml(h, &new_xml).await?;

    // OS-side side effects. Order matters: create the unix account first
    // (so /home/<name> exists for the .ssh write), then drop the
    // authorized_keys file.
    let shell = derive_shell(&row.priv_list);
    pw_useradd(h, &row.name, next_uid, shell, &row.descr).await?;
    if !req.authorized_keys.is_empty() {
        write_authorized_keys(h, &row.name, req.authorized_keys).await?;
    }
    Ok(())
}

pub struct UpdateUserReq<'a> {
    pub name: &'a str,
    pub descr: Option<&'a str>,
    pub priv_list: Option<Vec<String>>,
    pub authorized_keys: Option<&'a str>,
    pub disabled: Option<bool>,
    /// `None` leaves group memberships untouched (the audit's "default safe"
    /// case). `Some(vec)` sets them exactly.
    pub groups: Option<Vec<String>>,
}

pub async fn update_user(h: &ClientHandle, req: UpdateUserReq<'_>) -> Result<()> {
    if req.name.is_empty() {
        bail!("name required");
    }
    let xml = read_config_xml(h).await?;
    let mut doc = Doc::parse(&xml)?;
    let mut row = doc
        .find_user(req.name)
        .ok_or_else(|| anyhow!("user not found: {}", req.name))?;
    if let Some(d) = req.descr {
        row.descr = d.to_string();
    }
    if let Some(ref p) = req.priv_list {
        row.priv_list = p.clone();
    }
    if let Some(k) = req.authorized_keys {
        row.authorized_keys_b64 = base64_encode(k.as_bytes());
    }
    if let Some(d) = req.disabled {
        row.disabled = d;
    }
    doc.replace_user(&row)?;
    if let Some(ref g) = req.groups {
        doc.set_user_groups(row.uid, g)?;
    }
    let new_xml = doc.serialize()?;
    write_config_xml(h, &new_xml).await?;

    // OS-side updates.
    let shell = derive_shell(&row.priv_list);
    pw_usermod_shell(h, &row.name, shell).await?;
    if let Some(k) = req.authorized_keys {
        write_authorized_keys(h, &row.name, k).await?;
    }
    if let Some(d) = req.disabled {
        if d {
            let _ = exec_check(
                h,
                &format!("/usr/sbin/pw lock -n {}", shell_escape(&row.name)),
            )
            .await;
        } else {
            let _ = exec_check(
                h,
                &format!("/usr/sbin/pw unlock -n {}", shell_escape(&row.name)),
            )
            .await;
        }
    }
    Ok(())
}

pub async fn delete_user(h: &ClientHandle, name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name required");
    }
    let xml = read_config_xml(h).await?;
    let mut doc = Doc::parse(&xml)?;
    let row = doc
        .find_user(name)
        .ok_or_else(|| anyhow!("user not found: {name}"))?;
    if row.uid == 0 {
        bail!("refusing to delete uid 0 (admin)");
    }
    doc.remove_user(name)?;
    doc.remove_from_all_groups(row.uid)?;
    let new_xml = doc.serialize()?;
    write_config_xml(h, &new_xml).await?;
    // `pw userdel -r` removes the /home dir as well, which takes the
    // authorized_keys file with it.
    let _ = exec_check(
        h,
        &format!("/usr/sbin/pw userdel -n {} -r", shell_escape(name)),
    )
    .await;
    Ok(())
}

#[allow(dead_code)]
pub async fn set_password(h: &ClientHandle, name: &str, password: &str) -> Result<()> {
    if password.is_empty() {
        bail!("password required");
    }
    let xml = read_config_xml(h).await?;
    let mut doc = Doc::parse(&xml)?;
    let mut row = doc
        .find_user(name)
        .ok_or_else(|| anyhow!("user not found: {name}"))?;
    row.bcrypt_hash = Some(bcrypt::hash(password, 10).context("hashing password")?);
    row.sha512_hash = None;
    row.md5_hash = None;
    doc.replace_user(&row)?;
    let new_xml = doc.serialize()?;
    write_config_xml(h, &new_xml).await?;
    Ok(())
}

// ---------- SSH I/O ----------

async fn read_config_xml(h: &ClientHandle) -> Result<String> {
    let out = exec_command(h, &format!("/bin/cat {CONFIG_PATH}"))
        .await
        .context("reading config.xml")?;
    if !out.stderr.trim().is_empty() {
        bail!("cat {CONFIG_PATH} reported errors: {}", out.stderr.trim());
    }
    Ok(out.stdout)
}

async fn write_config_xml(h: &ClientHandle, contents: &str) -> Result<()> {
    // Write to a sibling tmp file and mv onto target so a half-written
    // config can never be loaded by pfSense's parser. Shell-quote nothing —
    // contents are sent via stdin not argv, and the path is a constant.
    let tmp = format!("{CONFIG_PATH}.pfusers.tmp");
    let cmd = format!("/bin/cat > {tmp} && /bin/mv {tmp} {CONFIG_PATH}");
    let out = crate::ssh::exec_with_stdin(h, &cmd, contents.as_bytes())
        .await
        .context("writing config.xml")?;
    if !out.stderr.trim().is_empty() {
        bail!("writing config.xml: {}", out.stderr.trim());
    }
    Ok(())
}

async fn exec_check(h: &ClientHandle, cmd: &str) -> Result<()> {
    let out = exec_command(h, cmd).await?;
    if !out.stderr.trim().is_empty() {
        bail!("`{cmd}` reported: {}", out.stderr.trim());
    }
    Ok(())
}

async fn pw_useradd(
    h: &ClientHandle,
    name: &str,
    uid: u32,
    shell: &str,
    descr: &str,
) -> Result<()> {
    // -m creates /home/<name>; -s sets shell; -c sets GECOS (full name).
    let cmd = format!(
        "/usr/sbin/pw useradd -n {} -u {} -m -s {} -c {} -G ''",
        shell_escape(name),
        uid,
        shell_escape(shell),
        shell_escape(descr),
    );
    exec_check(h, &cmd).await
}

async fn pw_usermod_shell(h: &ClientHandle, name: &str, shell: &str) -> Result<()> {
    let cmd = format!(
        "/usr/sbin/pw usermod -n {} -s {}",
        shell_escape(name),
        shell_escape(shell),
    );
    exec_check(h, &cmd).await
}

async fn write_authorized_keys(h: &ClientHandle, name: &str, keys: &str) -> Result<()> {
    let home = format!("/home/{name}");
    let ssh_dir = format!("{home}/.ssh");
    let ak = format!("{ssh_dir}/authorized_keys");
    // mkdir -p + chmod 700 + write file via stdin + chmod 600 + chown.
    let cmd = format!(
        "/bin/mkdir -p {ssh_dir} && /bin/chmod 700 {ssh_dir} && /bin/cat > {ak} && /bin/chmod 600 {ak} && /usr/sbin/chown -R {n}:{n} {ssh_dir}",
        n = shell_escape(name),
    );
    let out = crate::ssh::exec_with_stdin(h, &cmd, keys.as_bytes()).await?;
    if !out.stderr.trim().is_empty() {
        bail!("writing authorized_keys: {}", out.stderr.trim());
    }
    Ok(())
}

// ---------- Helpers ----------

fn derive_shell(priv_list: &[String]) -> &'static str {
    // Mirrors local_user_set's ladder from pfSense /etc/inc/auth.inc:706.
    if priv_list
        .iter()
        .any(|p| p == "user-shell-access" || p == "page-all")
    {
        "/bin/tcsh"
    } else if priv_list.iter().any(|p| p == "user-copy-files-chroot") {
        "/usr/local/sbin/scponlyc"
    } else if priv_list.iter().any(|p| p == "user-copy-files") {
        "/usr/local/bin/scponly"
    } else if priv_list.iter().any(|p| p == "user-ssh-tunnel") {
        "/usr/local/sbin/ssh_tunnel_shell"
    } else {
        "/sbin/nologin"
    }
}

fn validate_username(name: &str) -> Result<()> {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        bail!("username must contain only ASCII alphanumerics, '_', '-', '.': {name:?}");
    }
    if name.starts_with('-') {
        bail!("username can't start with '-'");
    }
    Ok(())
}

/// Single-quote-escape a string for embedding in `/bin/sh` command lines.
/// Wraps in single quotes; any literal single quote is rendered as
/// `'\''` (close-escape-open). Safe against shell metacharacters and the
/// values we ever pass here (usernames, file paths, shell paths).
fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | bytes[i + 2] as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        i += 3;
    }
    match bytes.len() - i {
        1 => {
            let n = (bytes[i] as u32) << 16;
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    // Lightweight inverse of base64_encode. Accepts standard alphabet with
    // '=' padding; ignores whitespace. Used for authorized_keys field
    // round-trip from XML.
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s: Vec<u8> = s
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for chunk in s.chunks(4) {
        let v: Vec<u8> = chunk.iter().filter_map(|c| val(*c)).collect();
        if v.len() < 2 {
            break;
        }
        out.push((v[0] << 2) | (v[1] >> 4));
        if v.len() >= 3 {
            out.push((v[1] << 4) | (v[2] >> 2));
        }
        if v.len() == 4 {
            out.push((v[2] << 6) | v[3]);
        }
    }
    out
}

fn row_to_pfuser(row: UserRow, groups: Vec<String>) -> PfUser {
    let bytes = base64_decode(&row.authorized_keys_b64);
    let authorized_keys = String::from_utf8_lossy(&bytes).into_owned();
    PfUser {
        name: row.name,
        descr: row.descr,
        uid: row.uid as i64,
        scope: row.scope,
        expires: row.expires,
        disabled: row.disabled,
        groups,
        priv_list: row.priv_list,
        authorized_keys,
        has_bcrypt: row.bcrypt_hash.is_some(),
        has_sha512: row.sha512_hash.is_some(),
        has_legacy_md5: row.md5_hash.is_some(),
    }
}

// ---------- XML document model ----------
//
// We hold the full document as a flat Vec<Event<'static>> from quick-xml.
// Mutations splice in / out of this vec at known indexes.

struct Doc {
    events: Vec<Event<'static>>,
}

// NB on the impl below: several of the depth-tracking match arms use
// `if depth == 0 { if let Some(x) = … }` rather than the equivalent
// `if depth == 0 && let …` chain — that chain is unstable on current
// stable Rust. The clippy::collapsible-if lint flags this; we silence
// it at the impl level rather than rewriting to a less-readable form.
#[allow(clippy::collapsible_if, clippy::collapsible_match)]
impl Doc {
    fn parse(xml: &str) -> Result<Self> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);
        let mut events = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => break,
                Ok(e) => events.push(e.into_owned()),
                Err(e) => bail!("parsing config.xml: {e}"),
            }
        }
        Ok(Self { events })
    }

    fn serialize(&self) -> Result<String> {
        let mut buf = Vec::new();
        let mut writer = Writer::new(Cursor::new(&mut buf));
        for ev in &self.events {
            writer
                .write_event(ev.clone())
                .map_err(|e| anyhow!("serializing config.xml: {e}"))?;
        }
        String::from_utf8(buf).map_err(|e| anyhow!("config.xml not UTF-8: {e}"))
    }

    /// Find the byte range `[start, end)` covering an element's `Start` …
    /// matching `End` (inclusive). Searches at any depth; returns the FIRST
    /// match for `name` within the element starting at `outer_start`.
    fn element_range(&self, outer_start: usize, name: &str) -> Option<(usize, usize)> {
        let needle = name.as_bytes();
        let mut depth = 0i32;
        let mut start_idx: Option<usize> = None;
        for (i, ev) in self.events.iter().enumerate().skip(outer_start) {
            match ev {
                Event::Start(s) => {
                    if depth == 0 && s.name().as_ref() == needle {
                        start_idx = Some(i);
                    }
                    depth += 1;
                }
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(start) = start_idx {
                            return Some((start, i));
                        }
                    }
                    if depth < 0 {
                        return None;
                    }
                }
                Event::Empty(s) => {
                    if depth == 0 && s.name().as_ref() == needle {
                        return Some((i, i));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Scan inside an element's `Start..End` range for ALL direct-child
    /// elements with the given name. Returns inclusive ranges.
    fn child_ranges(&self, start: usize, end: usize, name: &str) -> Vec<(usize, usize)> {
        let needle = name.as_bytes();
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut child_start: Option<usize> = None;
        for i in (start + 1)..end {
            match &self.events[i] {
                Event::Start(s) => {
                    if depth == 0 && s.name().as_ref() == needle {
                        child_start = Some(i);
                    }
                    depth += 1;
                }
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(cs) = child_start.take() {
                            out.push((cs, i));
                        }
                    }
                }
                Event::Empty(s) => {
                    if depth == 0 && s.name().as_ref() == needle {
                        out.push((i, i));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Read the text content of a leaf element identified by its `Start`
    /// index. Concatenates all `Text` events until the matching `End`.
    fn text_at(&self, start: usize) -> String {
        let mut out = String::new();
        let mut depth = 0i32;
        for ev in self.events.iter().skip(start) {
            match ev {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return out;
                    }
                }
                Event::Text(t) => {
                    out.push_str(&t.unescape().unwrap_or_default());
                }
                Event::CData(c) => {
                    out.push_str(std::str::from_utf8(c).unwrap_or(""));
                }
                _ => {}
            }
        }
        out
    }

    fn child_text(&self, start: usize, end: usize, name: &str) -> Option<String> {
        self.child_ranges(start, end, name)
            .first()
            .map(|(s, _)| self.text_at(*s))
    }

    fn child_texts(&self, start: usize, end: usize, name: &str) -> Vec<String> {
        self.child_ranges(start, end, name)
            .into_iter()
            .map(|(s, _)| self.text_at(s))
            .collect()
    }

    fn find_system_range(&self) -> Result<(usize, usize)> {
        // <pfsense> is the document root. system lives directly under it.
        let (pf_s, pf_e) = self
            .element_range(0, "pfsense")
            .ok_or_else(|| anyhow!("no <pfsense> root element"))?;
        let inner = self.child_ranges(pf_s, pf_e, "system");
        inner
            .first()
            .copied()
            .ok_or_else(|| anyhow!("no <system> in <pfsense>"))
    }

    fn list_users(&self) -> Result<Vec<UserRow>> {
        let (sys_s, sys_e) = self.find_system_range()?;
        let user_ranges = self.child_ranges(sys_s, sys_e, "user");
        user_ranges
            .into_iter()
            .map(|(s, e)| self.parse_user_range(s, e))
            .collect()
    }

    fn parse_user_range(&self, s: usize, e: usize) -> Result<UserRow> {
        let name = self.child_text(s, e, "name").unwrap_or_default();
        let descr = self.child_text(s, e, "descr").unwrap_or_default();
        let uid_str = self.child_text(s, e, "uid").unwrap_or_default();
        let uid: u32 = uid_str
            .parse()
            .with_context(|| format!("parsing uid {uid_str:?} for user {name:?}"))?;
        let scope = self
            .child_text(s, e, "scope")
            .unwrap_or_else(|| "user".into());
        let expires = self.child_text(s, e, "expires").filter(|s| !s.is_empty());
        // <disabled/> in pfSense is presence-or-absence; we treat any
        // matching child as truthy.
        let disabled = !self.child_ranges(s, e, "disabled").is_empty();
        let priv_list = self.child_texts(s, e, "priv");
        let authorized_keys_b64 = self.child_text(s, e, "authorizedkeys").unwrap_or_default();
        let bcrypt_hash = self.child_text(s, e, "bcrypt-hash");
        let sha512_hash = self.child_text(s, e, "sha512-hash");
        let md5_hash = self.child_text(s, e, "md5-hash");
        Ok(UserRow {
            name,
            descr,
            uid,
            scope,
            expires,
            disabled,
            priv_list,
            authorized_keys_b64,
            bcrypt_hash,
            sha512_hash,
            md5_hash,
        })
    }

    fn find_user(&self, name: &str) -> Option<UserRow> {
        self.list_users().ok()?.into_iter().find(|r| r.name == name)
    }

    fn next_uid(&self) -> Result<u32> {
        let (sys_s, sys_e) = self.find_system_range()?;
        let t = self
            .child_text(sys_s, sys_e, "nextuid")
            .ok_or_else(|| anyhow!("no <nextuid> in <system>"))?;
        t.parse().with_context(|| format!("parsing nextuid {t:?}"))
    }

    fn set_next_uid(&mut self, new: u32) -> Result<()> {
        let (sys_s, sys_e) = self.find_system_range()?;
        let (s, e) = self
            .child_ranges(sys_s, sys_e, "nextuid")
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no <nextuid> element"))?;
        // Replace events[s+1..e] with a single Text node.
        let new_text = Event::Text(BytesText::new(&new.to_string()).into_owned());
        self.events.splice(s + 1..e, [new_text]);
        Ok(())
    }

    fn append_user(&mut self, row: &UserRow) -> Result<()> {
        let (sys_s, sys_e) = self.find_system_range()?;
        let _ = sys_s;
        // Build the new block with a leading indent so the document stays
        // readable, then splice it in just before the closing </system>.
        // Splice (one mutation) rather than a loop of inserts (which
        // would reverse the order — each insert pushes the previous
        // one forward).
        let mut block: Vec<Event<'static>> = Vec::with_capacity(1 + 32);
        block.push(Event::Text(BytesText::new("\n\t").into_owned()));
        block.extend(build_user_block(row));
        self.events.splice(sys_e..sys_e, block);
        Ok(())
    }

    fn replace_user(&mut self, row: &UserRow) -> Result<()> {
        let (sys_s, sys_e) = self.find_system_range()?;
        let user_ranges = self.child_ranges(sys_s, sys_e, "user");
        for (s, e) in user_ranges {
            let n = self.child_text(s, e, "name").unwrap_or_default();
            if n == row.name {
                let new_block = build_user_block(row);
                self.events.splice(s..=e, new_block);
                return Ok(());
            }
        }
        bail!("user not found in XML: {}", row.name);
    }

    fn remove_user(&mut self, name: &str) -> Result<()> {
        let (sys_s, sys_e) = self.find_system_range()?;
        let user_ranges = self.child_ranges(sys_s, sys_e, "user");
        for (s, e) in user_ranges {
            let n = self.child_text(s, e, "name").unwrap_or_default();
            if n == name {
                // Also eat any immediate trailing whitespace text node so we
                // don't leave a stranded blank line.
                let end_with_ws = if matches!(self.events.get(e + 1), Some(Event::Text(_))) {
                    e + 1
                } else {
                    e
                };
                self.events.drain(s..=end_with_ws);
                return Ok(());
            }
        }
        bail!("user not found in XML: {name}");
    }

    /// Map uid → list of group names. Groups in pfSense list their members
    /// by uid string inside `<member>` child elements.
    fn groups_by_uid(&self) -> Result<std::collections::HashMap<u32, Vec<String>>> {
        let mut out: std::collections::HashMap<u32, Vec<String>> = Default::default();
        let (sys_s, sys_e) = self.find_system_range()?;
        for (gs, ge) in self.child_ranges(sys_s, sys_e, "group") {
            let gname = self.child_text(gs, ge, "name").unwrap_or_default();
            for m in self.child_texts(gs, ge, "member") {
                if let Ok(uid) = m.parse::<u32>() {
                    out.entry(uid).or_default().push(gname.clone());
                }
            }
        }
        Ok(out)
    }

    fn add_to_group(&mut self, group: &str, uid: u32) -> Result<()> {
        let (sys_s, sys_e) = self.find_system_range()?;
        let group_ranges = self.child_ranges(sys_s, sys_e, "group");
        for (gs, ge) in group_ranges {
            if self
                .child_text(gs, ge, "name")
                .as_deref()
                .unwrap_or_default()
                == group
            {
                let new_member = vec![
                    Event::Text(BytesText::new("\n\t\t").into_owned()),
                    Event::Start(BytesStart::new("member").into_owned()),
                    Event::Text(BytesText::new(&uid.to_string()).into_owned()),
                    Event::End(BytesEnd::new("member").into_owned()),
                ];
                let insert_at = ge;
                for ev in new_member.into_iter().rev() {
                    self.events.insert(insert_at, ev);
                }
                return Ok(());
            }
        }
        bail!("group not found: {group}");
    }

    fn remove_from_all_groups(&mut self, uid: u32) -> Result<()> {
        let target = uid.to_string();
        let (sys_s, sys_e) = self.find_system_range()?;
        let group_ranges = self.child_ranges(sys_s, sys_e, "group");
        // Collect (group_start, group_end, member_index_range) first so we
        // don't mutate while iterating.
        let mut to_remove: Vec<(usize, usize)> = Vec::new();
        for (gs, ge) in group_ranges {
            for (ms, me) in self.child_ranges(gs, ge, "member") {
                if self.text_at(ms) == target {
                    to_remove.push((ms, me));
                }
            }
        }
        // Remove from highest index first so earlier indices stay valid.
        to_remove.sort_by_key(|r| std::cmp::Reverse(r.0));
        for (s, e) in to_remove {
            let end_with_ws = if matches!(self.events.get(e + 1), Some(Event::Text(_))) {
                e + 1
            } else {
                e
            };
            self.events.drain(s..=end_with_ws);
        }
        Ok(())
    }

    fn set_user_groups(&mut self, uid: u32, groups: &[String]) -> Result<()> {
        // Authoritative: remove from every group, then add to `all` + each
        // listed group.
        self.remove_from_all_groups(uid)?;
        self.add_to_group("all", uid)?;
        for g in groups {
            if g != "all" {
                self.add_to_group(g, uid)?;
            }
        }
        Ok(())
    }
}

/// Build a `<user>…</user>` event sequence for a row.
fn build_user_block(row: &UserRow) -> Vec<Event<'static>> {
    let mut out: Vec<Event<'static>> = Vec::new();
    out.push(Event::Start(BytesStart::new("user").into_owned()));
    push_text_elem(&mut out, "scope", &row.scope);
    push_text_elem(&mut out, "name", &row.name);
    push_text_elem(&mut out, "descr", &row.descr);
    push_text_elem(&mut out, "uid", &row.uid.to_string());
    push_text_elem(&mut out, "expires", row.expires.as_deref().unwrap_or(""));
    push_text_elem(&mut out, "dashboardcolumns", "2");
    push_text_elem(&mut out, "authorizedkeys", &row.authorized_keys_b64);
    push_text_elem(&mut out, "ipsecpsk", "");
    push_text_elem(&mut out, "webguicss", "pfSense.css");
    if let Some(ref h) = row.bcrypt_hash {
        push_text_elem(&mut out, "bcrypt-hash", h);
    }
    if let Some(ref h) = row.sha512_hash {
        push_text_elem(&mut out, "sha512-hash", h);
    }
    if let Some(ref h) = row.md5_hash {
        push_text_elem(&mut out, "md5-hash", h);
    }
    if row.disabled {
        out.push(Event::Empty(BytesStart::new("disabled").into_owned()));
    }
    for p in &row.priv_list {
        push_text_elem(&mut out, "priv", p);
    }
    out.push(Event::End(BytesEnd::new("user").into_owned()));
    out
}

fn push_text_elem(out: &mut Vec<Event<'static>>, tag: &str, value: &str) {
    out.push(Event::Text(BytesText::new("\n\t\t").into_owned()));
    out.push(Event::Start(BytesStart::new(tag).into_owned()));
    if !value.is_empty() {
        out.push(Event::Text(BytesText::new(value).into_owned()));
    }
    out.push(Event::End(BytesEnd::new(tag).into_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XML: &str = r#"<?xml version="1.0"?>
<pfsense>
  <system>
    <hostname>pfSense</hostname>
    <domain>local</domain>
    <nextuid>2001</nextuid>
    <user>
      <scope>system</scope>
      <name>admin</name>
      <descr>System Administrator</descr>
      <uid>0</uid>
      <expires></expires>
      <authorizedkeys></authorizedkeys>
      <ipsecpsk></ipsecpsk>
      <bcrypt-hash>$2y$10$abcdefghijklmnopqrstuvwxyz123456789012345678901234</bcrypt-hash>
      <priv>page-all</priv>
    </user>
    <user>
      <scope>user</scope>
      <name>olga</name>
      <descr>Olga Timoshevskaia</descr>
      <uid>2000</uid>
      <expires></expires>
      <authorizedkeys>c3NoLWVkMjU1MTkgQUFBQQ==</authorizedkeys>
      <ipsecpsk></ipsecpsk>
      <bcrypt-hash>$2y$10$zyxwvutsrqponmlkjihgfedcba987654321098765432109876</bcrypt-hash>
      <priv>user-shell-access</priv>
    </user>
    <group>
      <name>all</name>
      <description>All Users</description>
      <scope>system</scope>
      <gid>1998</gid>
      <member>0</member>
      <member>2000</member>
    </group>
    <group>
      <name>admins</name>
      <description>System Administrators</description>
      <scope>system</scope>
      <gid>1999</gid>
      <member>0</member>
      <priv>page-all</priv>
    </group>
  </system>
</pfsense>"#;

    #[test]
    fn parses_two_users_with_expected_fields() {
        let doc = Doc::parse(SAMPLE_XML).unwrap();
        let rows = doc.list_users().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "admin");
        assert_eq!(rows[0].uid, 0);
        assert!(rows[0].bcrypt_hash.is_some());
        assert_eq!(rows[0].priv_list, vec!["page-all".to_string()]);
        assert_eq!(rows[1].name, "olga");
        assert_eq!(rows[1].uid, 2000);
        assert_eq!(rows[1].priv_list, vec!["user-shell-access".to_string()]);
        assert_eq!(rows[1].authorized_keys_b64, "c3NoLWVkMjU1MTkgQUFBQQ==");
    }

    #[test]
    fn groups_by_uid_reads_membership_correctly() {
        let doc = Doc::parse(SAMPLE_XML).unwrap();
        let g = doc.groups_by_uid().unwrap();
        let admin_groups = g.get(&0).cloned().unwrap_or_default();
        assert!(admin_groups.contains(&"all".to_string()));
        assert!(admin_groups.contains(&"admins".to_string()));
        let olga_groups = g.get(&2000).cloned().unwrap_or_default();
        assert_eq!(olga_groups, vec!["all".to_string()]);
    }

    #[test]
    fn round_trip_serializes_back_to_parseable_xml() {
        let doc = Doc::parse(SAMPLE_XML).unwrap();
        let out = doc.serialize().unwrap();
        // Re-parsing must succeed and give the same user list.
        let again = Doc::parse(&out).unwrap();
        let rows = again.list_users().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].name, "olga");
    }

    #[test]
    fn append_user_adds_to_system_block() {
        let mut doc = Doc::parse(SAMPLE_XML).unwrap();
        doc.append_user(&UserRow {
            name: "newguy".into(),
            descr: "New Guy".into(),
            uid: 2001,
            scope: "user".into(),
            expires: None,
            disabled: false,
            priv_list: vec!["user-shell-access".into()],
            authorized_keys_b64: "".into(),
            bcrypt_hash: Some("$2y$10$test".into()),
            sha512_hash: None,
            md5_hash: None,
        })
        .unwrap();
        let out = doc.serialize().unwrap();
        let again = Doc::parse(&out).unwrap();
        let rows = again.list_users().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].name, "newguy");
        assert_eq!(rows[2].uid, 2001);
    }

    #[test]
    fn remove_user_removes_the_row() {
        let mut doc = Doc::parse(SAMPLE_XML).unwrap();
        doc.remove_user("olga").unwrap();
        let rows = doc.list_users().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "admin");
    }

    #[test]
    fn replace_user_updates_descr_and_keys() {
        let mut doc = Doc::parse(SAMPLE_XML).unwrap();
        let mut row = doc.find_user("olga").unwrap();
        row.descr = "Olga (renamed)".into();
        row.authorized_keys_b64 = "bmV3a2V5".into();
        doc.replace_user(&row).unwrap();
        let out = doc.serialize().unwrap();
        let again = Doc::parse(&out).unwrap();
        let r = again.find_user("olga").unwrap();
        assert_eq!(r.descr, "Olga (renamed)");
        assert_eq!(r.authorized_keys_b64, "bmV3a2V5");
    }

    #[test]
    fn set_user_groups_adds_and_strips() {
        let mut doc = Doc::parse(SAMPLE_XML).unwrap();
        doc.set_user_groups(2000, &["admins".to_string()]).unwrap();
        let g = doc.groups_by_uid().unwrap();
        let olga_groups = g.get(&2000).cloned().unwrap_or_default();
        assert!(olga_groups.contains(&"all".to_string()));
        assert!(olga_groups.contains(&"admins".to_string()));
    }

    #[test]
    fn remove_from_all_groups_strips_admin_uid_zero() {
        let mut doc = Doc::parse(SAMPLE_XML).unwrap();
        doc.remove_from_all_groups(0).unwrap();
        let g = doc.groups_by_uid().unwrap();
        assert!(!g.contains_key(&0) || g[&0].is_empty());
    }

    #[test]
    fn next_uid_round_trip() {
        let mut doc = Doc::parse(SAMPLE_XML).unwrap();
        assert_eq!(doc.next_uid().unwrap(), 2001);
        doc.set_next_uid(2002).unwrap();
        assert_eq!(doc.next_uid().unwrap(), 2002);
    }

    #[test]
    fn derive_shell_priv_ladder() {
        assert_eq!(derive_shell(&[]), "/sbin/nologin");
        assert_eq!(
            derive_shell(&["user-ssh-tunnel".into()]),
            "/usr/local/sbin/ssh_tunnel_shell"
        );
        assert_eq!(derive_shell(&["user-shell-access".into()]), "/bin/tcsh");
        assert_eq!(derive_shell(&["page-all".into()]), "/bin/tcsh");
    }

    #[test]
    fn shell_escape_handles_single_quote() {
        assert_eq!(shell_escape("simple"), "'simple'");
        assert_eq!(shell_escape("with space"), "'with space'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
        // Adversarial: shell metacharacters all pass through.
        assert_eq!(shell_escape("`$()&;|"), "'`$()&;|'");
    }

    #[test]
    fn validate_username_rejects_special_chars() {
        assert!(validate_username("olga").is_ok());
        assert!(validate_username("user-name.1").is_ok());
        assert!(validate_username("bad name").is_err()); // space
        assert!(validate_username("bad$name").is_err()); // shell meta
        assert!(validate_username("-leadingdash").is_err());
    }

    #[test]
    fn base64_encode_round_trip() {
        for input in [b"" as &[u8], b"a", b"ab", b"abc", b"abcd", b"foobar"] {
            let enc = base64_encode(input);
            let dec = base64_decode(&enc);
            assert_eq!(dec, input.to_vec(), "round-trip failed for {input:?}");
        }
        // Known reference.
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_decode("Zm9v"), b"foo".to_vec());
    }
}
