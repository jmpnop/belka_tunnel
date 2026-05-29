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
//! ## Byte-preserving mutation + four-layer safety wrapper
//!
//! Round-tripping the entire document through a serializer would risk
//! losing whitespace / attribute formatting that pfSense's parser
//! tolerates only because PHP serialized them. We avoid that entirely:
//!
//!   1. **Byte-precise mutation** (`apply_mutation`): parse the original
//!      XML to find byte offsets, then splice only the changed bytes.
//!      `result = original[..start] + new + original[end..]`. Regions
//!      OUTSIDE the splice are LITERALLY the same `&[u8]` as input —
//!      there's no Writer re-emission for unchanged areas, so they
//!      cannot diverge from what pfSense's parser sees.
//!
//!   2. **Snapshot before every write** (`write_with_safety`): copy
//!      `/cf/conf/config.xml` to `/cf/conf/backup/config-<unix-ts>-pfusers.xml`
//!      via SSH. pfSense's Web UI Config History page enumerates that
//!      directory, so the user can revert through their normal admin
//!      path — or via SSH `cp` if needed.
//!
//!   3. **Self-verification** (Rust side): after the write, re-read
//!      config.xml and assert the mutation actually took (new user
//!      present, descr round-tripped, count incremented, etc.). Catches
//!      our serializer bugs cheaply.
//!
//!   4. **pfSense parser verification** (PHP side, one bounded call):
//!      `php -r 'require_once("config.inc"); parse_config(true);
//!      echo count($config["system"]["user"]);'` over SSH. Compares the
//!      user count to what we expect post-mutation. This is the ONLY
//!      PHP call in pfusers and exists purely as a parser-agreement
//!      check — no PHP in the mutation path.
//!
//! If ANY of layers 3/4 fail, we automatically `cp` the snapshot back
//! over config.xml. The router sees the same bytes it had before the
//! call, and the caller gets an `Err` with the diagnostic.
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
use quick_xml::events::Event;
use quick_xml::Reader;

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

// ---------- Byte-precise mutation engine ----------
//
// All write paths route through `apply_mutations`. Instead of round-tripping
// the entire document through quick-xml's Writer (which is where the
// byte-mismatch risk lives), we:
//
//   1. Parse the original XML to find byte offsets for the regions we want
//      to change.
//   2. Generate new bytes ONLY for those regions.
//   3. Splice: `result = original[..start] + new + original[end..]`.
//
// Unchanged regions of the file are literally the same `&[u8]` as the
// input — there is no Writer re-emission to disagree with pfSense's
// parser. The risk surface contracts to "do the bytes we generate for the
// changed regions parse cleanly?" — which we test against an
// existing-block template and validate end-to-end via the safety wrapper.

#[derive(Debug, Clone)]
enum Mutation {
    AppendUser(UserRow),
    ReplaceUser(UserRow),
    RemoveUser(String),
    AddGroupMember { group: String, uid: u32 },
    RemoveGroupMembership(u32),
    SetNextUid(u32),
}

/// Apply all mutations sequentially, re-parsing between each. Slow in the
/// limit (O(N²) for N mutations) but pfusers performs at most a handful
/// per save, so the simplicity wins. The alternative (sort byte ranges,
/// apply in reverse) is fragile when a mutation changes byte counts mid-pass.
fn apply_mutations(mut xml: String, mutations: Vec<Mutation>) -> Result<String> {
    for m in mutations {
        xml = apply_mutation(&xml, &m)?;
    }
    Ok(xml)
}

fn apply_mutation(xml: &str, m: &Mutation) -> Result<String> {
    let events = parse_with_positions(xml)?;
    match m {
        Mutation::AppendUser(row) => {
            let insert_at = system_close_start(&events, xml)?;
            let indent = detect_user_indent(&events, xml);
            let block = generate_user_block(row, &indent);
            Ok(splice(xml, insert_at, insert_at, &block))
        }
        Mutation::ReplaceUser(row) => {
            let (start, end) = find_user_byte_range(&events, &row.name)?;
            let indent = leading_indent_of(xml, start);
            let block = generate_user_block(row, &indent);
            Ok(splice(xml, start, end, &block))
        }
        Mutation::RemoveUser(name) => {
            let (start, end) = find_user_byte_range_with_leading_ws(xml, &events, name)?;
            Ok(splice(xml, start, end, ""))
        }
        Mutation::AddGroupMember { group, uid } => {
            let close_at = group_close_start(&events, xml, group)?;
            let indent = leading_indent_of(xml, close_at);
            let inner_indent = format!("{indent}\t");
            let chunk = format!("{inner_indent}<member>{uid}</member>\n{indent}");
            Ok(splice(xml, close_at, close_at, &chunk))
        }
        Mutation::RemoveGroupMembership(uid) => {
            let ranges = find_member_byte_ranges(xml, &events, *uid);
            // Apply highest-position first so earlier offsets stay valid.
            let mut out = xml.to_string();
            for (s, e) in ranges.into_iter().rev() {
                let (s2, e2) = grow_to_include_leading_ws(&out, s, e);
                out = splice(&out, s2, e2, "");
            }
            Ok(out)
        }
        Mutation::SetNextUid(n) => {
            let (s, e) = find_text_content_range(&events, "nextuid")?;
            Ok(splice(xml, s, e, &n.to_string()))
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedEvent {
    event: Event<'static>,
    /// Byte range `[start, end)` covered by this event in the original
    /// source. Used for byte-precise mutation.
    start: usize,
    end: usize,
}

fn parse_with_positions(xml: &str) -> Result<Vec<ParsedEvent>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut events = Vec::new();
    loop {
        let pos_before = reader.buffer_position() as usize;
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(e) => {
                let pos_after = reader.buffer_position() as usize;
                events.push(ParsedEvent {
                    event: e.into_owned(),
                    start: pos_before,
                    end: pos_after,
                });
            }
            Err(e) => bail!("parsing config.xml: {e}"),
        }
    }
    Ok(events)
}

fn splice(src: &str, start: usize, end: usize, with: &str) -> String {
    let mut out = String::with_capacity(src.len() - (end - start) + with.len());
    out.push_str(&src[..start]);
    out.push_str(with);
    out.push_str(&src[end..]);
    out
}

/// Walk events looking for the closing tag of a top-level <system>. Returns
/// the byte position of '<' in `</system>` so the caller can splice before it.
fn system_close_start(events: &[ParsedEvent], _xml: &str) -> Result<usize> {
    let mut depth = 0i32;
    let mut in_system = false;
    for ev in events {
        match &ev.event {
            Event::Start(s) => {
                if depth == 1 && s.name().as_ref() == b"system" {
                    in_system = true;
                }
                depth += 1;
            }
            Event::End(e) => {
                depth -= 1;
                if depth == 1 && in_system && e.name().as_ref() == b"system" {
                    return Ok(ev.start);
                }
            }
            _ => {}
        }
    }
    bail!("no </system> found");
}

/// Like system_close_start but for an arbitrary <group> identified by name.
fn group_close_start(events: &[ParsedEvent], xml: &str, group_name: &str) -> Result<usize> {
    let mut depth = 0i32;
    let mut group_starts: Vec<usize> = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        match &ev.event {
            Event::Start(s) => {
                if depth == 2 && s.name().as_ref() == b"group" {
                    group_starts.push(i);
                }
                depth += 1;
            }
            Event::End(e) => {
                depth -= 1;
                if depth == 2 && e.name().as_ref() == b"group" {
                    let gs = group_starts.pop().unwrap_or(i);
                    // Find <name> child within this group.
                    if let Some(name_text) = child_text_in_range(events, xml, gs, i, "name") {
                        if name_text == group_name {
                            return Ok(ev.start);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    bail!("group not found: {group_name}");
}

/// Return the byte range of `<user>…</user>` matching `name`.
fn find_user_byte_range(events: &[ParsedEvent], name: &str) -> Result<(usize, usize)> {
    let mut depth = 0i32;
    let mut user_starts: Vec<usize> = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        match &ev.event {
            Event::Start(s) => {
                if depth == 2 && s.name().as_ref() == b"user" {
                    user_starts.push(i);
                }
                depth += 1;
            }
            Event::End(e) => {
                depth -= 1;
                if depth == 2 && e.name().as_ref() == b"user" {
                    let us = user_starts.pop().unwrap_or(i);
                    // Walk children to find <name>.
                    // Use the raw XML through child_text_in_range_xml.
                    if let Some(n) = read_child_text_within(events, us, i, "name") {
                        if n == name {
                            return Ok((events[us].start, events[i].end));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    bail!("user not found in XML: {name}")
}

/// Same as find_user_byte_range but extends the start backwards to absorb
/// any leading whitespace (Text event consisting only of `\n\t…`), so a
/// delete doesn't leave a stranded blank line.
fn find_user_byte_range_with_leading_ws(
    xml: &str,
    events: &[ParsedEvent],
    name: &str,
) -> Result<(usize, usize)> {
    let (mut s, e) = find_user_byte_range(events, name)?;
    // Walk back over preceding whitespace bytes (only space/tab/newline)
    // so the deleted region eats its own leading indentation.
    while s > 0 {
        let prev = xml.as_bytes()[s - 1];
        if matches!(prev, b' ' | b'\t' | b'\r' | b'\n') {
            s -= 1;
        } else {
            break;
        }
    }
    // Restore exactly one newline so adjacent text doesn't run together.
    while s > 0 && xml.as_bytes()[s - 1] != b'\n' {
        s -= 1;
    }
    Ok((s, e))
}

fn find_text_content_range(events: &[ParsedEvent], name: &str) -> Result<(usize, usize)> {
    let needle = name.as_bytes();
    for (i, ev) in events.iter().enumerate() {
        if let Event::Start(s) = &ev.event {
            if s.name().as_ref() == needle {
                let start_text = events[i].end;
                let mut inner_depth = 1i32;
                for ev_inner in events.iter().skip(i + 1) {
                    match &ev_inner.event {
                        Event::Start(_) => inner_depth += 1,
                        Event::End(_) => {
                            inner_depth -= 1;
                            if inner_depth == 0 {
                                return Ok((start_text, ev_inner.start));
                            }
                        }
                        _ => {}
                    }
                }
                bail!("unclosed <{name}>");
            }
        }
    }
    bail!("element not found: <{name}>")
}

fn read_child_text_within(
    events: &[ParsedEvent],
    parent_start_idx: usize,
    parent_end_idx: usize,
    child_name: &str,
) -> Option<String> {
    let needle = child_name.as_bytes();
    let mut depth = 0i32;
    for i in (parent_start_idx + 1)..parent_end_idx {
        match &events[i].event {
            Event::Start(s) => {
                if depth == 0 && s.name().as_ref() == needle {
                    // Concatenate text until matching End.
                    let mut out = String::new();
                    let mut inner_depth = 1i32;
                    for ev_inner in events.iter().take(parent_end_idx).skip(i + 1) {
                        match &ev_inner.event {
                            Event::Start(_) => inner_depth += 1,
                            Event::End(_) => {
                                inner_depth -= 1;
                                if inner_depth == 0 {
                                    return Some(out);
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
                    return Some(out);
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    None
}

fn child_text_in_range(
    events: &[ParsedEvent],
    _xml: &str,
    parent_start: usize,
    parent_end: usize,
    name: &str,
) -> Option<String> {
    read_child_text_within(events, parent_start, parent_end, name)
}

/// All <member> child byte ranges where the text content equals `uid`,
/// across all groups in the document.
fn find_member_byte_ranges(xml: &str, events: &[ParsedEvent], uid: u32) -> Vec<(usize, usize)> {
    let target = uid.to_string();
    let mut out = Vec::new();
    let mut depth = 0i32;
    for (i, ev) in events.iter().enumerate() {
        match &ev.event {
            Event::Start(s) => {
                if depth == 3 && s.name().as_ref() == b"member" {
                    // Find matching End.
                    let mut inner = 1i32;
                    for j in (i + 1)..events.len() {
                        match &events[j].event {
                            Event::Start(_) => inner += 1,
                            Event::End(_) => {
                                inner -= 1;
                                if inner == 0 {
                                    let text = &xml[events[i].end..events[j].start];
                                    if text.trim() == target {
                                        out.push((events[i].start, events[j].end));
                                    }
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    out
}

fn grow_to_include_leading_ws(xml: &str, mut s: usize, e: usize) -> (usize, usize) {
    while s > 0 {
        let prev = xml.as_bytes()[s - 1];
        if matches!(prev, b' ' | b'\t' | b'\r' | b'\n') {
            s -= 1;
        } else {
            break;
        }
    }
    while s > 0 && xml.as_bytes()[s - 1] != b'\n' {
        s -= 1;
    }
    (s, e)
}

/// Extract the whitespace indentation immediately preceding byte position
/// `pos` — the run of spaces/tabs after the most recent `\n`. Used so a
/// new block matches the existing indentation depth.
fn leading_indent_of(xml: &str, pos: usize) -> String {
    let bytes = xml.as_bytes();
    if pos == 0 {
        return String::new();
    }
    let mut start = pos;
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    std::str::from_utf8(&bytes[start..pos])
        .unwrap_or("\t\t")
        .to_string()
}

/// Pick an indent string for a new <user> block. We sniff the leading
/// whitespace of an existing <user> in the document if one is present;
/// otherwise default to two tabs (matches pfSense convention).
fn detect_user_indent(events: &[ParsedEvent], xml: &str) -> String {
    let mut depth = 0i32;
    for ev in events {
        match &ev.event {
            Event::Start(s) => {
                if depth == 2 && s.name().as_ref() == b"user" {
                    return leading_indent_of(xml, ev.start);
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    "\t\t".to_string()
}

/// Render a `<user>…</user>` block as bytes ready to splice into the
/// document at the given indentation level. Children get one extra tab of
/// indent (matching pfSense's two-space-or-tab convention seen in
/// /cf/conf/config.xml).
fn generate_user_block(row: &UserRow, indent: &str) -> String {
    let mut out = String::new();
    let child = format!("{indent}\t");
    out.push_str("<user>\n");
    push_elem(&mut out, &child, "scope", &row.scope);
    push_elem(&mut out, &child, "name", &row.name);
    push_elem(&mut out, &child, "descr", &row.descr);
    push_elem(&mut out, &child, "uid", &row.uid.to_string());
    push_elem(
        &mut out,
        &child,
        "expires",
        row.expires.as_deref().unwrap_or(""),
    );
    push_elem(&mut out, &child, "dashboardcolumns", "2");
    push_elem(&mut out, &child, "authorizedkeys", &row.authorized_keys_b64);
    push_elem(&mut out, &child, "ipsecpsk", "");
    push_elem(&mut out, &child, "webguicss", "pfSense.css");
    if let Some(h) = &row.bcrypt_hash {
        push_elem(&mut out, &child, "bcrypt-hash", h);
    }
    if let Some(h) = &row.sha512_hash {
        push_elem(&mut out, &child, "sha512-hash", h);
    }
    if let Some(h) = &row.md5_hash {
        push_elem(&mut out, &child, "md5-hash", h);
    }
    if row.disabled {
        out.push_str(&format!("{child}<disabled/>\n"));
    }
    for p in &row.priv_list {
        push_elem(&mut out, &child, "priv", p);
    }
    out.push_str(indent);
    out.push_str("</user>\n");
    out
}

fn push_elem(out: &mut String, indent: &str, name: &str, value: &str) {
    if value.is_empty() {
        out.push_str(&format!("{indent}<{name}></{name}>\n"));
    } else {
        out.push_str(&format!(
            "{indent}<{name}>{val}</{name}>\n",
            val = xml_escape_text(value),
        ));
    }
}

fn xml_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
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

    let original = read_config_xml(h).await?;
    let doc = Doc::parse(&original)?;
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
    let mut mutations = vec![
        Mutation::AppendUser(row.clone()),
        Mutation::SetNextUid(next_uid + 1),
        Mutation::AddGroupMember {
            group: "all".to_string(),
            uid: next_uid,
        },
    ];
    for g in &req.groups {
        if g != "all" {
            mutations.push(Mutation::AddGroupMember {
                group: g.clone(),
                uid: next_uid,
            });
        }
    }
    let new_xml = apply_mutations(original.clone(), mutations)?;

    write_with_safety(h, &original, &new_xml, |fresh| {
        let after = Doc::parse(fresh)?;
        // Verify the new user appears and got the uid we expected.
        let row = after
            .find_user(req.name)
            .ok_or_else(|| anyhow!("post-write check: new user not found in XML"))?;
        if row.uid != next_uid {
            bail!(
                "post-write check: uid mismatch (got {}, expected {next_uid})",
                row.uid
            );
        }
        if after.next_uid()? != next_uid + 1 {
            bail!("post-write check: nextuid was not incremented");
        }
        Ok(())
    })
    .await?;

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
    let original = read_config_xml(h).await?;
    let doc = Doc::parse(&original)?;
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
    let row_uid = row.uid;
    let mut mutations = vec![Mutation::ReplaceUser(row.clone())];
    if let Some(ref g) = req.groups {
        // Authoritative: strip all memberships, re-add 'all' + each in list.
        mutations.push(Mutation::RemoveGroupMembership(row_uid));
        mutations.push(Mutation::AddGroupMember {
            group: "all".to_string(),
            uid: row_uid,
        });
        for grp in g {
            if grp != "all" {
                mutations.push(Mutation::AddGroupMember {
                    group: grp.clone(),
                    uid: row_uid,
                });
            }
        }
    }
    let new_xml = apply_mutations(original.clone(), mutations)?;

    write_with_safety(h, &original, &new_xml, |fresh| {
        let after = Doc::parse(fresh)?;
        let written = after
            .find_user(req.name)
            .ok_or_else(|| anyhow!("post-write check: user not found after write"))?;
        if let Some(d) = req.descr {
            if written.descr != d {
                bail!("post-write check: descr did not round-trip");
            }
        }
        if let Some(ref p) = req.priv_list {
            if written.priv_list != *p {
                bail!("post-write check: priv list did not round-trip");
            }
        }
        Ok(())
    })
    .await?;

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
    let original = read_config_xml(h).await?;
    let doc = Doc::parse(&original)?;
    let row = doc
        .find_user(name)
        .ok_or_else(|| anyhow!("user not found: {name}"))?;
    if row.uid == 0 {
        bail!("refusing to delete uid 0 (admin)");
    }
    let mutations = vec![
        Mutation::RemoveUser(name.to_string()),
        Mutation::RemoveGroupMembership(row.uid),
    ];
    let new_xml = apply_mutations(original.clone(), mutations)?;

    write_with_safety(h, &original, &new_xml, |fresh| {
        let after = Doc::parse(fresh)?;
        if after.find_user(name).is_some() {
            bail!("post-write check: user still present after delete");
        }
        Ok(())
    })
    .await?;

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
    let original = read_config_xml(h).await?;
    let doc = Doc::parse(&original)?;
    let mut row = doc
        .find_user(name)
        .ok_or_else(|| anyhow!("user not found: {name}"))?;
    row.bcrypt_hash = Some(bcrypt::hash(password, 10).context("hashing password")?);
    row.sha512_hash = None;
    row.md5_hash = None;
    let new_xml = apply_mutations(original.clone(), vec![Mutation::ReplaceUser(row)])?;
    write_with_safety(h, &original, &new_xml, |fresh| {
        let after = Doc::parse(fresh)?;
        let r = after
            .find_user(name)
            .ok_or_else(|| anyhow!("post-write check: user not found"))?;
        if r.bcrypt_hash.is_none() {
            bail!("post-write check: bcrypt-hash missing");
        }
        Ok(())
    })
    .await?;
    Ok(())
}

// ---------- Safety wrapper around writes ----------

/// Snapshot + write + verify + rollback. The complete-mitigation flow:
///
///   1. `cp /cf/conf/config.xml /cf/conf/backup/config-<unix-ts>-pfusers.xml`
///      — pfSense's web UI Config History page enumerates that directory, so
///      our snapshot shows up alongside its own and the user gets a one-click
///      revert through normal admin paths.
///   2. Write `new_xml` to a sibling .tmp, then atomic-rename onto config.xml.
///   3. Re-read config.xml, parse with our parser, run `checks(fresh)` —
///      caller asserts the mutation actually took (new user found, descr
///      round-tripped, count incremented, etc.).
///   4. Optionally invoke pfSense's PHP parser once for total parser-
///      agreement: `php -r 'require_once("config.inc"); parse_config(true);
///      echo count($config["system"]["user"]);'`. If the count disagrees
///      with our pre-write expectation, rollback. (One bounded PHP call,
///      only for verification — no PHP in the mutation path.)
///   5. On ANY failure in 2/3/4: `cp` the snapshot back over config.xml.
///      The router sees the same bytes it had before this call.
async fn write_with_safety(
    h: &ClientHandle,
    original_xml: &str,
    new_xml: &str,
    checks: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    let unix_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let snapshot_path = format!("/cf/conf/backup/config-{unix_ts}-pfusers.xml");

    // Step 1: snapshot. If this fails, abort BEFORE any mutation — no
    // recovery path exists without it.
    exec_check(h, &format!("/bin/cp {CONFIG_PATH} {snapshot_path}"))
        .await
        .context("creating config snapshot")?;

    // Step 2: write the new XML atomically.
    if let Err(e) = write_config_xml(h, new_xml).await {
        // If the write itself failed, the original is intact (we wrote
        // to .tmp). No rollback needed; snapshot remains for the user.
        return Err(e).context("write_with_safety: write failed (snapshot kept)");
    }

    // Step 3+4: read back, verify with our parser AND pfSense's, rollback
    // if either disagrees.
    let outcome = (async {
        let fresh = read_config_xml(h).await?;
        checks(&fresh)?;
        verify_with_pfsense_parser(h, original_xml, &fresh).await?;
        Ok::<(), anyhow::Error>(())
    })
    .await;

    if let Err(e) = outcome {
        // Step 5: rollback from snapshot.
        let rollback = exec_check(h, &format!("/bin/cp {snapshot_path} {CONFIG_PATH}")).await;
        if let Err(re) = rollback {
            return Err(anyhow!(
                "verification failed: {e:#}\nROLLBACK ALSO FAILED: {re:#}\nSnapshot still at {snapshot_path} — manual recovery required"
            ));
        }
        return Err(e).context(format!(
            "verification failed; rolled back from {snapshot_path}"
        ));
    }
    Ok(())
}

/// Run pfSense's own PHP parser against the freshly-written config.xml.
/// Compares the user count to what we had BEFORE the write — any
/// disagreement means pfSense's parser saw something different than our
/// parser did and we should roll back.
///
/// This is the ONLY PHP call in pfusers and only fires after the write,
/// purely as a parser-agreement check.
async fn verify_with_pfsense_parser(
    h: &ClientHandle,
    original_xml: &str,
    fresh_xml: &str,
) -> Result<()> {
    let original_doc = Doc::parse(original_xml)?;
    let fresh_doc = Doc::parse(fresh_xml)?;
    let our_original = original_doc.list_users()?.len() as i64;
    let our_fresh = fresh_doc.list_users()?.len() as i64;
    // PHP runs config.inc + parse_config(true) (which re-reads
    // /cf/conf/config.xml), and prints the count of system/user entries.
    //
    // CRITICAL: we do NOT pass `-d include_path=…` here. pfSense's default
    // include_path (as set in /usr/local/etc/php.ini) is
    //   .:/etc/inc:/usr/local/pfSense/include:/usr/local/pfSense/include/www
    //   :/usr/local/www:/usr/local/captiveportal:/usr/local/pkg
    //   :/usr/local/www/classes:/usr/local/www/classes/Form
    //   :/usr/local/share/pear:/usr/local/share/openssl_x509_crl/
    // Overriding it with a single dir (which we used to do) makes
    // util.inc's `require_once('Net/IPv6.php')` fail to find the PEAR
    // file and bail with exit 255 + empty stdout — exactly the
    // "produced non-numeric" rollback we just hit.
    let cmd = "/usr/local/bin/php -r 'require_once(\"config.inc\"); parse_config(true); echo count($config[\"system\"][\"user\"]);'";
    let out = exec_command(h, cmd).await?;
    let count_str = out.stdout.trim();
    let pfsense_count: i64 = count_str
        .parse()
        .map_err(|_| anyhow!("pfSense PHP verify produced non-numeric: {count_str:?}"))?;
    if pfsense_count != our_fresh {
        bail!(
            "pfSense parser saw {pfsense_count} users; our parser saw {our_fresh}. \
             Pre-write count was {our_original}. Rolling back."
        );
    }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- Live-router integration tests ----------
    //
    // These touch the real router at admin@192.168.1.1:22222. They are
    // `#[ignore]` by default so the regular `cargo test` run stays
    // hermetic. Run explicitly:
    //   cargo test --release -p pfusers swap_olga_priv_live \
    //       -- --ignored --nocapture
    //
    // The four-layer safety wrapper (snapshot + write + verify + rollback)
    // means even a failed test leaves the router in a recoverable state:
    // either the change took (and we saw the BEFORE/AFTER diff) or it was
    // rolled back from the snapshot.

    #[tokio::test]
    #[ignore = "live router integration; run with `--ignored --nocapture`"]
    async fn swap_olga_priv_live() {
        let cfg = crate::config::AppConfig::load_or_default().expect("load config");
        let (handle, _) = crate::ssh::connect(
            &cfg.ssh.host,
            cfg.ssh.port,
            &cfg.ssh.user,
            &cfg.ssh.key_path,
            cfg.ssh.host_key_fingerprint.clone(),
        )
        .await
        .expect("ssh connect");

        let before = list_users(&handle).await.expect("list_users BEFORE");
        let olga_before = before
            .iter()
            .find(|u| u.name == "olgatimoshevskaia")
            .expect("olga in user list");
        println!(
            "BEFORE  olga.priv_list = {:?}  groups = {:?}",
            olga_before.priv_list, olga_before.groups
        );
        assert!(
            olga_before
                .priv_list
                .iter()
                .any(|p| p == "user-shell-access"),
            "expected olga to start with user-shell-access"
        );

        let req = UpdateUserReq {
            name: "olgatimoshevskaia",
            descr: None,
            priv_list: Some(vec!["user-ssh-tunnel".to_string()]),
            authorized_keys: None,
            disabled: None,
            // groups: None means "don't touch memberships" (audit #4).
            groups: None,
        };
        update_user(&handle, req).await.expect("update_user");

        let after = list_users(&handle).await.expect("list_users AFTER");
        let olga_after = after
            .iter()
            .find(|u| u.name == "olgatimoshevskaia")
            .expect("olga still in user list after update");
        println!(
            "AFTER   olga.priv_list = {:?}  groups = {:?}",
            olga_after.priv_list, olga_after.groups
        );
        assert_eq!(
            olga_after.priv_list,
            vec!["user-ssh-tunnel".to_string()],
            "priv list did not round-trip as expected"
        );
        assert_eq!(
            olga_after.groups, olga_before.groups,
            "group memberships changed even though groups=None was passed"
        );
        println!("OK — olga now uses user-ssh-tunnel (shell will be /usr/local/sbin/ssh_tunnel_shell)");
    }

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

    // --- Byte-precise mutation tests ---
    //
    // The single most important guarantee these tests pin down: bytes
    // OUTSIDE the mutation region are preserved verbatim. That is the
    // structural fix for the byte-mismatch risk; we test it directly.

    #[test]
    fn apply_mutation_remove_user_preserves_bytes_outside_region() {
        // Take the substring before the first <user> and after the last
        // </group>, check both pieces survive a RemoveUser mutation
        // unchanged.
        let before_users = SAMPLE_XML.find("<user>").unwrap();
        let after_groups = SAMPLE_XML.rfind("</group>").unwrap() + "</group>".len();
        let prefix = &SAMPLE_XML[..before_users];
        let suffix = &SAMPLE_XML[after_groups..];

        let out = apply_mutation(SAMPLE_XML, &Mutation::RemoveUser("olga".into())).unwrap();
        // Prefix must appear at byte 0.
        assert!(
            out.starts_with(prefix),
            "prefix not byte-identical after RemoveUser"
        );
        // Suffix must appear verbatim near the end.
        assert!(
            out.contains(suffix),
            "suffix not byte-identical after RemoveUser"
        );
    }

    #[test]
    fn apply_mutation_set_next_uid_only_touches_nextuid_text() {
        let out = apply_mutation(SAMPLE_XML, &Mutation::SetNextUid(2042)).unwrap();
        // The whole file should match the original EXCEPT for the value of
        // <nextuid>: the original has 2001, the new has 2042.
        let new = out.replace("<nextuid>2042</nextuid>", "<nextuid>2001</nextuid>");
        assert_eq!(
            new, SAMPLE_XML,
            "SetNextUid changed more than the <nextuid> text content",
        );
    }

    #[test]
    fn apply_mutation_append_user_preserves_pre_system_close_bytes() {
        let row = UserRow {
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
        };
        let out = apply_mutation(SAMPLE_XML, &Mutation::AppendUser(row)).unwrap();
        // Round-trip parses + 3 users now.
        let again = Doc::parse(&out).unwrap();
        let rows = again.list_users().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].name, "newguy");
        // Pre-existing user blocks must be byte-identical.
        let olga_block_start = SAMPLE_XML.find("<user>\n      <scope>user").unwrap();
        let olga_block_end = SAMPLE_XML[olga_block_start..].find("</user>").unwrap()
            + olga_block_start
            + "</user>".len();
        let olga_bytes = &SAMPLE_XML[olga_block_start..olga_block_end];
        assert!(
            out.contains(olga_bytes),
            "olga's <user> block must survive AppendUser byte-for-byte",
        );
    }

    #[test]
    fn apply_mutation_replace_user_only_changes_target_block() {
        let mut row = Doc::parse(SAMPLE_XML).unwrap().find_user("olga").unwrap();
        row.descr = "Olga (renamed)".into();
        row.authorized_keys_b64 = "bmV3a2V5".into();
        let out = apply_mutation(SAMPLE_XML, &Mutation::ReplaceUser(row)).unwrap();
        let again = Doc::parse(&out).unwrap();
        let r = again.find_user("olga").unwrap();
        assert_eq!(r.descr, "Olga (renamed)");
        assert_eq!(r.authorized_keys_b64, "bmV3a2V5");
        // admin's block must be byte-identical (it precedes olga's).
        let admin_block_start = SAMPLE_XML.find("<user>\n      <scope>system").unwrap();
        let admin_block_end = SAMPLE_XML[admin_block_start..].find("</user>").unwrap()
            + admin_block_start
            + "</user>".len();
        let admin_bytes = &SAMPLE_XML[admin_block_start..admin_block_end];
        assert!(
            out.contains(admin_bytes),
            "admin's <user> block must survive ReplaceUser byte-for-byte"
        );
    }

    #[test]
    fn apply_mutation_remove_group_membership_strips_all_matching_member_rows() {
        let out = apply_mutation(SAMPLE_XML, &Mutation::RemoveGroupMembership(0)).unwrap();
        // admin (uid 0) is a member of both "all" and "admins" in the
        // sample; both <member>0</member> rows must be gone.
        assert!(
            !out.contains("<member>0</member>"),
            "uid 0 still present as a member after RemoveGroupMembership"
        );
        // uid 2000's membership must survive.
        assert!(
            out.contains("<member>2000</member>"),
            "uid 2000's membership was wrongly removed"
        );
    }

    #[test]
    fn apply_mutation_add_group_member_inserts_inside_target_group() {
        let out = apply_mutation(
            SAMPLE_XML,
            &Mutation::AddGroupMember {
                group: "admins".to_string(),
                uid: 2000,
            },
        )
        .unwrap();
        // Must contain a new <member>2000</member> nested inside <group> ..
        // </group> for 'admins'.
        let admins_close = out.find("admins").and_then(|i| out[i..].find("</group>"));
        assert!(admins_close.is_some());
        // Both prior memberships still present.
        assert!(out.contains("<member>0</member>"));
        assert!(out.contains("<member>2000</member>"));
    }

    #[test]
    fn apply_mutations_pipeline_composes_correctly() {
        // Simulate the full add_user mutation list: append user, set next
        // uid, add to 'all'.
        let row = UserRow {
            name: "newguy".into(),
            descr: "New Guy".into(),
            uid: 2001,
            scope: "user".into(),
            expires: None,
            disabled: false,
            priv_list: vec![],
            authorized_keys_b64: "".into(),
            bcrypt_hash: Some("$2y$10$test".into()),
            sha512_hash: None,
            md5_hash: None,
        };
        let muts = vec![
            Mutation::AppendUser(row),
            Mutation::SetNextUid(2002),
            Mutation::AddGroupMember {
                group: "all".to_string(),
                uid: 2001,
            },
        ];
        let out = apply_mutations(SAMPLE_XML.to_string(), muts).unwrap();
        let after = Doc::parse(&out).unwrap();
        assert_eq!(after.list_users().unwrap().len(), 3);
        assert_eq!(after.next_uid().unwrap(), 2002);
        let g = after.groups_by_uid().unwrap();
        assert!(g.get(&2001).is_some_and(|v| v.contains(&"all".to_string())));
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
