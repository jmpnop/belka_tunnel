//! Drive `pfSsh.php` over SSH. The REPL `eval()`s our buffer with no error
//! wrapping (exit code is 0 even on PHP parse/fatal — see research note),
//! so every script we send is wrapped in `try { … echo SENTINEL+payload }
//! catch { fwrite(STDERR, ERR_SENTINEL+message) }` and we parse the
//! sentinel out of stdout. Banner lines, MOTD chatter, "pfSense shell:"
//! prompts are ignored.

use crate::ssh::{exec_command, ClientHandle};
use crate::users::PfUser;
use anyhow::{bail, Context, Result};

/// Marks the start of a JSON payload in stdout. Random-looking string so
/// no plausible pfSense banner can collide with it.
const OK_SENTINEL: &str = "<<<PFUSERS-OK-7e3f1a>>>";
/// Marks an error message in either stdout or stderr.
const ERR_SENTINEL: &str = "<<<PFUSERS-ERR-7e3f1a>>>";

/// Build a complete pfSsh.php invocation that reads the PHP body from
/// stdin via heredoc and tags its output with our sentinels. The remote
/// command we send is a single line so `russh::Channel::exec` can run it
/// directly without a shell escape dance.
fn wrap_for_pfsh(body: &str) -> String {
    // pfSsh.php's playback_text() does raw eval() on the heredoc body —
    // we don't need to escape PHP syntax. We DO need to terminate with
    // the magic `exec\nexit\n` so the REPL processes our buffer and
    // quits instead of hanging waiting for more input.
    //
    // Note the deliberate use of plain `'<<EOF'` (quoted EOF marker) on
    // the heredoc — that disables shell-side variable expansion so a `$`
    // in the PHP body doesn't get eaten by the remote shell.
    let php = format!(
        r#"require_once("auth.inc"); global $config, $userindex, $groupindex;
try {{
{body}
}} catch (Throwable $e) {{
  fwrite(STDERR, "{ERR_SENTINEL}".$e->getMessage()."\n");
  exit(1);
}}"#
    );
    format!("/usr/local/sbin/pfSsh.php <<'EOF'\n{php}\nexec\nexit\nEOF\n")
}

/// Run a PHP body through pfSsh.php and return the OK_SENTINEL payload.
/// If the buffer wrote to ERR_SENTINEL, that error is bubbled out.
async fn run_pfsh(handle: &ClientHandle, body: &str) -> Result<String> {
    let cmd = wrap_for_pfsh(body);
    let out = exec_command(handle, &cmd).await?;
    // Error sentinel can appear on either stream; check both.
    for stream in [&out.stdout, &out.stderr] {
        if let Some(start) = stream.find(ERR_SENTINEL) {
            let after = &stream[start + ERR_SENTINEL.len()..];
            let msg = after.lines().next().unwrap_or("").trim();
            bail!("pfSsh.php error: {msg}");
        }
    }
    let Some(start) = out.stdout.find(OK_SENTINEL) else {
        // No OK sentinel means the script never reached its echo — either a
        // PHP parse error (which pfSsh.php prints to stderr at exit 0) or
        // we got cut off mid-flight.
        let hint = if !out.stderr.trim().is_empty() {
            format!(" (stderr: {})", out.stderr.trim())
        } else {
            String::new()
        };
        bail!("pfSsh.php produced no OK sentinel{hint}");
    };
    let payload_start = start + OK_SENTINEL.len();
    let tail = &out.stdout[payload_start..];
    // OK sentinel content runs to end-of-line, or to ERR_SENTINEL if both
    // somehow appear (defensive — the try/catch shouldn't allow it).
    let payload = tail.lines().next().unwrap_or("").to_string();
    Ok(payload)
}

// ---------- Operations ----------

/// PHP body to enumerate users with all the fields the GUI needs.
fn list_users_php() -> String {
    format!(
        r#"$out = [];
foreach ($config["system"]["user"] as $u) {{
  $out[] = [
    "name"  => $u["name"],
    "descr" => $u["descr"] ?? "",
    "uid"   => (int)$u["uid"],
    "scope" => $u["scope"] ?? "user",
    "expires" => $u["expires"] ?? null,
    "disabled" => isset($u["disabled"]),
    "groups" => function_exists("local_user_get_groups") ? local_user_get_groups($u, true) : [],
    "priv_list" => $u["priv"] ?? [],
    "authorized_keys" => isset($u["authorizedkeys"]) ? base64_decode($u["authorizedkeys"]) : "",
    "has_bcrypt" => !empty($u["bcrypt-hash"]),
    "has_sha512" => !empty($u["sha512-hash"]),
    "has_legacy_md5" => !empty($u["md5-hash"]),
  ];
}}
echo "{OK_SENTINEL}".json_encode($out)."\n";"#
    )
}

pub async fn list_users(handle: &ClientHandle) -> Result<Vec<PfUser>> {
    let payload = run_pfsh(handle, &list_users_php()).await?;
    serde_json::from_str::<Vec<PfUser>>(&payload)
        .with_context(|| format!("parsing list_users JSON: {}", truncate(&payload, 200)))
}

/// Create a new user. Replicates the web-UI assembly order:
/// password hash → append to system/user → add to 'all' group → set
/// memberships → materialise via local_user_set → write_config.
pub async fn add_user(
    handle: &ClientHandle,
    name: &str,
    descr: &str,
    password: &str,
    priv_list: &[String],
    extra_groups: &[String],
    authorized_keys: &str,
) -> Result<()> {
    let name_lit = php_string(name);
    let descr_lit = php_string(descr);
    let password_lit = php_string(password);
    let priv_arr = php_string_array(priv_list);
    let groups_arr = php_string_array(extra_groups);
    let keys_b64 = base64_encode(authorized_keys.as_bytes());
    let keys_lit = php_string(&keys_b64);

    let body = format!(
        r#"$nextuid = (int)config_get_path('system/nextuid');
$user = [
  "scope" => "user",
  "name" => {name_lit},
  "descr" => {descr_lit},
  "uid" => (string)$nextuid,
  "expires" => "",
  "dashboardcolumns" => "2",
  "authorizedkeys" => {keys_lit},
  "ipsecpsk" => "",
  "webguicss" => "pfSense.css",
  "priv" => {priv_arr},
];
$wrap = ["item" => &$user];
local_user_set_password($wrap, {password_lit});
config_set_path('system/nextuid', $nextuid + 1);
$users = config_get_path('system/user', []);
$users[] = $user;
config_set_path('system/user', $users);
$groups = config_get_path('system/group', []);
foreach ($groups as &$g) {{ if ($g['name'] == 'all') {{ $g['member'][] = $user['uid']; break; }} }}
unset($g);
config_set_path('system/group', $groups);
local_user_set_groups($user, {groups_arr});
local_user_set($user);
$userindex = index_users();
write_config("pfUsers: created user " . {name_lit});
echo "{OK_SENTINEL}{{}}\n";"#
    );
    let _ = run_pfsh(handle, &body).await?;
    Ok(())
}

pub async fn delete_user(handle: &ClientHandle, name: &str) -> Result<()> {
    let name_lit = php_string(name);
    let body = format!(
        r#"$idx = null;
foreach (config_get_path('system/user', []) as $i => $u) {{
  if ($u['name'] === {name_lit}) {{ $idx = $i; break; }}
}}
if ($idx === null) throw new Exception("user not found: " . {name_lit});
$u = config_get_path("system/user/{{$idx}}");
if ((int)$u['uid'] === 0) throw new Exception("refusing to delete uid 0");
local_user_del($u);
config_del_path("system/user/{{$idx}}");
$users = array_values(config_get_path('system/user', []));
config_set_path('system/user', $users);
write_config("pfUsers: deleted user " . {name_lit});
echo "{OK_SENTINEL}{{}}\n";"#
    );
    let _ = run_pfsh(handle, &body).await?;
    Ok(())
}

pub async fn update_user(
    handle: &ClientHandle,
    name: &str,
    descr: &str,
    priv_list: &[String],
    groups: &[String],
    authorized_keys: &str,
    disabled: bool,
) -> Result<()> {
    let name_lit = php_string(name);
    let descr_lit = php_string(descr);
    let priv_arr = php_string_array(priv_list);
    let groups_arr = php_string_array(groups);
    let keys_b64 = base64_encode(authorized_keys.as_bytes());
    let keys_lit = php_string(&keys_b64);
    let disabled_kv = if disabled {
        // PHP `isset()` checks key presence, so we just set it to "".
        "$u['disabled'] = \"\";".to_string()
    } else {
        "unset($u['disabled']);".to_string()
    };
    let body = format!(
        r#"$idx = null;
foreach (config_get_path('system/user', []) as $i => $row) {{
  if ($row['name'] === {name_lit}) {{ $idx = $i; break; }}
}}
if ($idx === null) throw new Exception("user not found: " . {name_lit});
$u = config_get_path("system/user/{{$idx}}");
$u['descr'] = {descr_lit};
$u['priv'] = {priv_arr};
$u['authorizedkeys'] = {keys_lit};
{disabled_kv}
config_set_path("system/user/{{$idx}}", $u);
local_user_set_groups($u, {groups_arr});
local_user_set($u);
write_config("pfUsers: updated user " . {name_lit});
echo "{OK_SENTINEL}{{}}\n";"#
    );
    let _ = run_pfsh(handle, &body).await?;
    Ok(())
}

// Currently unused but kept for the password-reset path that the GUI hasn't
// wired up yet. Will be called from the user-detail "Reset Password" flow.
#[allow(dead_code)]
pub async fn set_password(handle: &ClientHandle, name: &str, password: &str) -> Result<()> {
    let name_lit = php_string(name);
    let password_lit = php_string(password);
    let body = format!(
        r#"$idx = null;
foreach (config_get_path('system/user', []) as $i => $row) {{
  if ($row['name'] === {name_lit}) {{ $idx = $i; break; }}
}}
if ($idx === null) throw new Exception("user not found: " . {name_lit});
$u = config_get_path("system/user/{{$idx}}");
$wrap = ["item" => &$u, "idx" => $idx];
local_user_set_password($wrap, {password_lit});
config_set_path("system/user/{{$idx}}", $u);
local_user_set($u);
write_config("pfUsers: password reset for " . {name_lit});
echo "{OK_SENTINEL}{{}}\n";"#
    );
    let _ = run_pfsh(handle, &body).await?;
    Ok(())
}

// ---------- PHP literal escaping ----------

/// Quote a Rust string as a PHP double-quoted literal. PHP double-quoted
/// strings interpret `\`, `"`, `$`, and most C escapes — we escape them
/// so the value lands inside PHP exactly as it does in Rust.
fn php_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn php_string_array(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&php_string(s));
    }
    out.push(']');
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    // Tiny inline encoder — avoids pulling another crate.
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
    let remaining = bytes.len() - i;
    if remaining == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if remaining == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_string_escapes_dangerous_metacharacters() {
        // No expansion: $foo must reach PHP as a literal, not as a variable.
        assert_eq!(php_string("$foo"), "\"\\$foo\"");
        // Backslash + quote are escaped.
        assert_eq!(php_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        // Newlines become \n so the literal stays on one line.
        assert_eq!(php_string("line1\nline2"), "\"line1\\nline2\"");
        // Empty case.
        assert_eq!(php_string(""), "\"\"");
    }

    #[test]
    fn php_string_array_round_trip_shape() {
        let arr = php_string_array(&["user-shell-access".to_string(), "page-all".to_string()]);
        assert_eq!(arr, "[\"user-shell-access\", \"page-all\"]");
        let empty = php_string_array(&[]);
        assert_eq!(empty, "[]");
    }

    #[test]
    fn base64_matches_python_reference() {
        // Cross-check against known base64 outputs.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // A typical authorized_keys line.
        let ssh = "ssh-ed25519 AAAA test\n";
        // Decoding back via PHP base64_decode would give us 'ssh-ed25519 AAAA test\n'.
        let enc = base64_encode(ssh.as_bytes());
        assert!(
            !enc.contains('\n'),
            "base64 output should never have newlines"
        );
        assert!(enc.ends_with('='), "should be padded");
    }

    #[test]
    fn wrap_for_pfsh_includes_try_catch_and_sentinels() {
        let cmd = wrap_for_pfsh("echo \"x\";");
        assert!(cmd.contains("require_once(\"auth.inc\")"));
        assert!(cmd.contains("try {"));
        assert!(cmd.contains("} catch (Throwable $e) {"));
        assert!(cmd.contains(ERR_SENTINEL), "missing ERR_SENTINEL hook");
        assert!(cmd.contains("exec\nexit\n"));
        // Heredoc EOF marker should be quoted — otherwise the remote shell
        // would expand $variables in the PHP body before pfSsh.php sees it.
        assert!(cmd.contains("<<'EOF'"));
    }

    #[test]
    fn list_users_template_is_well_formed_php() {
        let php = list_users_php();
        assert!(php.contains("foreach ($config[\"system\"][\"user\"]"));
        assert!(php.contains("base64_decode"));
        assert!(php.contains("json_encode($out)"));
        assert!(php.contains(OK_SENTINEL));
    }
}
