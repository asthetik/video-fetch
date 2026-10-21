use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::error::AppResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cookie {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expiration: u64,
    pub name: String,
    pub value: String,
}

pub fn parse_netscape_cookies(text: &str) -> AppResult<Vec<Cookie>> {
    let mut cookies = Vec::new();

    for line in text.lines() {
        let line = line.trim_end();
        // Browser extensions mark HttpOnly cookies with a `#HttpOnly_` prefix
        // so other Netscape readers skip them; yt-dlp strips it back off.
        // Bilibili's SESSDATA is HttpOnly, so reading the line as a comment
        // dropped exactly the cookie this import exists to capture.
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 7 {
            continue;
        }

        let include_subdomains = parts[1].eq_ignore_ascii_case("TRUE");
        let secure = parts[3].eq_ignore_ascii_case("TRUE");
        let expiration = parts[4].parse::<u64>().unwrap_or(0);

        cookies.push(Cookie {
            domain: parts[0].to_string(),
            include_subdomains,
            path: parts[2].to_string(),
            secure,
            expiration,
            name: parts[5].to_string(),
            value: parts[6].to_string(),
        });
    }

    Ok(cookies)
}

pub fn has_bilibili_sessdata(cookies: &[Cookie]) -> bool {
    cookies
        .iter()
        .any(|cookie| cookie.name == "SESSDATA" && !cookie.value.is_empty())
}

/// True for Bilibili-owned domains (`bilibili.com` or any subdomain), guarding
/// against suffix lookalikes like `bilibili.com.evil.com`.
pub fn is_bilibili_domain(domain: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    d == "bilibili.com" || d.ends_with(".bilibili.com")
}

/// Ensure SESSDATA cookies have a Bilibili domain so Netscape export / yt-dlp work,
/// and drop cookies from other domains (captcha/tracking cookies the login WebView
/// accumulates are not needed for downloads and must not be persisted). Only a
/// domain-less SESSDATA is rewritten onto `.bilibili.com`; a SESSDATA carrying an
/// explicit foreign domain (including suffix lookalikes) is dropped rather than
/// trusted and re-hosted under the Bilibili domain.
pub fn normalize_bilibili_cookies(cookies: &[Cookie]) -> Vec<Cookie> {
    cookies
        .iter()
        .map(|cookie| {
            let mut c = cookie.clone();
            if c.name == "SESSDATA" && c.domain.is_empty() {
                c.domain = ".bilibili.com".into();
                c.include_subdomains = true;
            }
            if c.path.is_empty() {
                c.path = "/".into();
            }
            c
        })
        .filter(|c| is_bilibili_domain(&c.domain))
        .collect()
}

/// Join stored cookies into a `name=value; name=value` request header, keeping
/// only Bilibili domains and skipping cookies that have already expired.
pub fn cookie_header_for_bilibili(cookies: &[Cookie]) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    cookies
        .iter()
        .filter(|c| is_bilibili_domain(&c.domain) && !c.name.is_empty())
        .filter(|c| c.expiration == 0 || c.expiration >= now)
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Create the cookie jar owner-only from the moment it exists.
///
/// `File::create` would leave it at `0666 & ~umask` until a later chmod, and
/// a local user who opens the file inside that window keeps the handle — the
/// chmod cannot take back a read that already started. The kernel applies
/// `mode` when it creates the file, so there is no window to lose. A path
/// that already exists keeps its own mode, which is what the caller's fixup
/// covers.
fn create_private_jar(path: &Path) -> std::io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    // `create_new` refuses to open anything already at the staging name: a
    // planted 0644 file or symlink must fail the write, never be followed and
    // published. Callers retry under a fresh name on `AlreadyExists`.
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Per-call sequence for staging names. Writers are not serialized (the
/// commands went async, so a resume and the login-persist path can overlap),
/// and two writers sharing one staging inode would let a later `truncate`
/// punch NUL holes into bytes the earlier writer already synced — or turn one
/// writer's error cleanup into the deletion of the other's file. A unique
/// name per call removes the sharing entirely; the counter only has to be
/// unique within the process (the app is single-instance), and it advances
/// past names already on disk instead of deleting what this call did not
/// create.
static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Prefix of this jar's staging names. `.videofetch-` is the same hidden,
/// app-owned prefix `fsutil::copy_temp_for` uses, so a leftover staging file
/// never reads as the product to a prefix scan — and a scan BY this prefix is
/// how the removal paths (logout, logged-out materialize) clean up a staging
/// file stranded by a kill between staging and rename.
pub(crate) fn staging_prefix(path: &Path) -> OsString {
    let mut prefix = OsString::from(".videofetch-");
    prefix.push(path.file_name().unwrap_or_default());
    prefix.push(".");
    prefix
}

/// Where the jar is staged before the rename that publishes it: a sibling of
/// the target (the rename stays on one volume and stays atomic), hidden and
/// prefixed like `fsutil::copy_temp_for`, with a per-call suffix.
fn jar_temp_for(path: &Path) -> PathBuf {
    let mut name = staging_prefix(path);
    name.push(STAGING_SEQ.fetch_add(1, Ordering::Relaxed).to_string());
    name.push(".tmp");
    path.with_file_name(name)
}

/// Create the staging file for one write, advancing past names that are
/// already taken (a leftover from a killed run, or a planted file) rather
/// than reusing or deleting them. Failure to find a free name after a bounded
/// number of attempts is returned as-is; the caller must not clean up a path
/// this call never created.
fn open_staging(path: &Path) -> std::io::Result<(PathBuf, fs::File)> {
    for _ in 0..16 {
        let tmp = jar_temp_for(path);
        match create_private_jar(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free staging name after 16 attempts",
    ))
}

/// Remove every staged-but-never-published sibling of `path`. Best-effort:
/// the removal paths must not fail because the cache dir has surprises in it,
/// and the prefix (hidden, app-owned, exact) is what keeps a user file from
/// ever matching.
pub(crate) fn remove_stranded_staging_files(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let prefix = staging_prefix(path);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .as_os_str()
            .as_encoded_bytes()
            .starts_with(prefix.as_encoded_bytes())
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

pub fn write_netscape_file(path: &Path, cookies: &[Cookie]) -> AppResult<()> {
    // Staged beside the jar and renamed into place: truncate-then-write opened
    // the jar first, so a death mid-write left a half cookies.txt where the
    // whole one belongs — and yt-dlp reads whatever is there. The rename
    // installs the whole file or nothing.
    let mut tmp = None;
    let write = (|| -> AppResult<()> {
        let (staging, mut file) = open_staging(path)?;
        tmp = Some(staging);
        writeln!(file, "# Netscape HTTP Cookie File")?;
        writeln!(file, "# https://curl.haxx.se/docs/http-cookies.html")?;
        writeln!(
            file,
            "# This file was generated by Video Fetch. Edit at your own risk."
        )?;

        for cookie in cookies {
            writeln!(
                file,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                cookie.domain,
                if cookie.include_subdomains {
                    "TRUE"
                } else {
                    "FALSE"
                },
                cookie.path,
                if cookie.secure { "TRUE" } else { "FALSE" },
                cookie.expiration,
                cookie.name,
                cookie.value,
            )?;
        }

        // Land the bytes before the rename publishes them: on a
        // delayed-allocation filesystem the rename can otherwise reach the
        // disk first and leave a zero-length jar.
        file.sync_all()?;
        fs::rename(tmp.as_deref().expect("staging path set above"), path)?;
        sync_parent_dir(path);
        Ok(())
    })();
    if write.is_err() {
        // Only ever this call's own staging file: `open_staging` either
        // created it or returned an error with `tmp` still `None` (a taken or
        // planted name advances the sequence rather than being deleted).
        // Nothing else tidies this directory, so a scratch file left by a
        // failed write would otherwise sit there for the life of the install.
        if let Some(tmp) = tmp {
            let _ = fs::remove_file(&tmp);
        }
    }
    write
}

/// Flush the directory that received a just-published jar. Some filesystems
/// persist a rename's new name later than the file's bytes; the jar is the
/// only copy yt-dlp reads, so a crash in that window would leave it stale or
/// missing. Best effort: filesystems that cannot flush a directory reject it
/// with ENOTSUP/EINVAL, and failing the write there gains nothing.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        match fs::File::open(parent).and_then(|dir| dir.sync_all()) {
            Ok(()) => {
                #[cfg(test)]
                sync_probe::record(parent);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
                ) => {}
            Err(e) => tracing::warn!(
                "could not flush {} after publishing cookies: {e}",
                parent.display()
            ),
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(all(test, unix))]
mod sync_probe {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static SYNCED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(super) fn record(path: &Path) {
        SYNCED.lock().unwrap().push(path.to_path_buf());
    }

    pub(super) fn synced(path: &Path) -> bool {
        SYNCED.lock().unwrap().iter().any(|p| p == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# Netscape HTTP Cookie File
.bilibili.com	TRUE	/	FALSE	0	SESSDATA	abc123
.bilibili.com	TRUE	/	FALSE	0	bili_jct	tok
"#;

    /// Browser extensions mark HttpOnly cookies with a `#HttpOnly_` prefix so
    /// other Netscape readers skip them; yt-dlp strips it. Bilibili's SESSDATA
    /// is HttpOnly, so reading the line as a comment dropped exactly the
    /// cookie the import exists to capture.
    #[test]
    fn parses_httponly_prefixed_lines() {
        let text = "# Netscape HTTP Cookie File\n\
                    #HttpOnly_.bilibili.com\tTRUE\t/\tTRUE\t0\tSESSDATA\tabc123\n";
        let cookies = parse_netscape_cookies(text).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "SESSDATA");
        assert_eq!(cookies[0].domain, ".bilibili.com");
        assert!(cookies[0].secure);
        assert!(has_bilibili_sessdata(&cookies));
    }

    #[test]
    fn still_skips_comments_and_blank_lines() {
        let text = "# a comment\n\
                    \n\
                    #HttpOnly_.bilibili.com\tTRUE\t/\tTRUE\t0\tSESSDATA\tv\n\
                    #HttpOnly_\n\
                    # trailing comment\n";
        let cookies = parse_netscape_cookies(text).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].value, "v");
    }

    #[test]
    fn parses_sessdata() {
        let cookies = parse_netscape_cookies(SAMPLE).unwrap();
        assert!(has_bilibili_sessdata(&cookies));
        assert_eq!(
            cookies.iter().find(|c| c.name == "SESSDATA").unwrap().value,
            "abc123"
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(
            parse_netscape_cookies("not a cookies file")
                .unwrap()
                .is_empty()
        );
    }

    fn cookie(domain: &str, name: &str, value: &str) -> Cookie {
        Cookie {
            domain: domain.into(),
            include_subdomains: domain.starts_with('.'),
            path: "/".into(),
            secure: true,
            expiration: 0,
            name: name.into(),
            value: value.into(),
        }
    }

    /// The jar carries SESSDATA. `File::create` left it at 0666 & ~umask
    /// until auth.rs chmod-ed it afterwards, and a local user who opened the
    /// file inside that window kept the handle — the chmod cannot take back a
    /// read that already started. Asking the kernel for the mode at creation
    /// leaves no window to lose.
    #[cfg(unix)]
    #[test]
    fn write_netscape_file_creates_the_jar_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode_of = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // Premise: under a umask of 077 a plain create is 0600 too, and the
        // assertion below could hold without the mode ever being requested.
        let control = dir.path().join("control");
        fs::write(&control, b"x").unwrap();
        assert_ne!(
            mode_of(&control),
            0o600,
            "this umask makes the test vacuous; it cannot tell whether the \
             mode was requested"
        );

        let path = dir.path().join("cookies.txt");
        write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "secret")]).unwrap();

        assert_eq!(mode_of(&path), 0o600);
    }

    /// The write swaps a finished temp file in rather than rewriting the jar,
    /// and that swap is the whole point: an interrupted truncate-then-write
    /// leaves a half cookies.txt where the whole one belongs, and yt-dlp reads
    /// whatever is there. Atomicity itself needs fault injection to observe,
    /// so this pins the structure that provides it — a rename installs a new
    /// inode, an in-place rewrite keeps the old one.
    #[cfg(unix)]
    #[test]
    fn write_netscape_file_replaces_the_jar_instead_of_rewriting_it() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "one")]).unwrap();
        let first = fs::metadata(&path).unwrap().ino();

        write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "two")]).unwrap();
        let second = fs::metadata(&path).unwrap().ino();

        assert_ne!(
            first, second,
            "an in-place rewrite keeps the inode; only a rename swaps in a new \
             one — this is the seam the write must go through"
        );
    }

    /// A write that cannot even start staging must leave the jar that is
    /// already there untouched: yt-dlp keeps reading the previous whole file
    /// instead of inheriting a truncated one. `#[cfg(unix)]`: the failure is
    /// staged by making the cache dir read-only, which has no portable
    /// Windows equivalent; on Windows the failure-path cleanup stays covered
    /// by `a_failed_publish_leaves_no_temporary_behind`.
    #[cfg(unix)]
    #[test]
    fn a_failed_write_leaves_the_previous_jar_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "old")]).unwrap();
        let before = fs::read(&path).unwrap();

        // A read-only directory: creating the staging file fails before the
        // jar is opened. (With a per-call unique staging name there is no
        // fixed path to pre-block, and blocking the cache dir is the exact
        // shape of the failure this test needs.)
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        assert!(
            write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "new")]).is_err(),
            "the blocked staging path must fail the write"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "a failed write must leave the previous whole jar in place"
        );
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn write_netscape_file_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "secret")]).unwrap();

        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "cookies.txt")
            .collect();

        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A rename that cannot land must not leave its scratch file behind:
    /// nothing else tidies the cache dir, so it would sit there for the life
    /// of the install.
    #[test]
    fn a_failed_publish_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the jar belongs, so the rename cannot land.
        let path = dir.path().join("cookies.txt");
        fs::create_dir(&path).unwrap();

        assert!(write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "s")]).is_err());
        assert!(
            staging_leftovers(dir.path(), &path).is_empty(),
            "the staging file must not outlive the attempt that used it"
        );
    }

    /// All staging files for `jar` currently on disk (there should be none
    /// outside a write in flight).
    fn staging_leftovers(dir: &Path, jar: &Path) -> Vec<String> {
        let prefix = staging_prefix(jar);
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| {
                n.as_os_str()
                    .as_encoded_bytes()
                    .starts_with(prefix.as_encoded_bytes())
            })
            .map(|n| n.to_string_lossy().into_owned())
            .collect()
    }

    /// The staging name is per-call unique (concurrent writers — a resume
    /// and the login-persist path can overlap since the commands went async —
    /// must never share one inode) and hidden behind an app prefix (a
    /// leftover must not read as the jar to a prefix scan), mirroring
    /// `fsutil::copy_temp_for`'s collision discipline.
    #[test]
    fn staging_names_are_unique_and_prefixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        let a = jar_temp_for(&path);
        let b = jar_temp_for(&path);
        assert_ne!(a, b, "two writers must not share a staging path");
        assert_ne!(a, path.with_file_name("cookies.txt.tmp"));
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(".videofetch-cookies.txt.") && name.ends_with(".tmp"),
            "staging name must carry the app prefix: {name}"
        );
    }

    /// `create_new` refuses to follow a file planted at the staging name:
    /// publishing an attacker-planted 0644 (or symlinked) temp must be
    /// impossible, not merely unlikely.
    #[test]
    fn a_planted_staging_file_is_refused_not_published() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        // Plant at the exact next staging name.
        let planted = jar_temp_for(&path);
        fs::write(&planted, b"planted").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&planted, fs::Permissions::from_mode(0o644)).unwrap();
        }

        // The write must not publish the planted bytes. Either it refuses the
        // staging name (create_new) or it opens a different one; both leave
        // the planted file un-published at `path`.
        let result = write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "new")]);
        if result.is_ok() {
            let bytes = fs::read(&path).unwrap();
            assert_ne!(
                bytes, b"planted",
                "the planted bytes must never be published"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(
                    mode, 0o600,
                    "the published jar must be freshly created 0600, not the planted file's mode"
                );
            }
        } else {
            assert!(!path.exists(), "a refused write must not publish anything");
        }
    }

    #[test]
    fn normalize_keeps_only_bilibili_domains() {
        let cookies = vec![
            cookie(".bilibili.com", "SESSDATA", "abc"),
            cookie(".bilibili.com", "bili_jct", "tok"),
            cookie(".geetest.com", "gt", "x"),
            cookie(".doubleclick.net", "id", "y"),
        ];
        let normalized = normalize_bilibili_cookies(&cookies);
        let names: Vec<&str> = normalized.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["SESSDATA", "bili_jct"]);
    }

    #[test]
    fn normalize_rewrites_domainless_sessdata() {
        let cookies = vec![cookie("", "SESSDATA", "abc")];
        let normalized = normalize_bilibili_cookies(&cookies);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].domain, ".bilibili.com");
        assert!(normalized[0].include_subdomains);
    }

    #[test]
    fn normalize_drops_sessdata_from_foreign_domain() {
        let cookies = vec![cookie(".example.com", "SESSDATA", "abc")];
        let normalized = normalize_bilibili_cookies(&cookies);
        assert!(normalized.is_empty());
    }

    #[test]
    fn normalize_drops_lookalike_domains() {
        let cookies = vec![
            cookie("bilibili.com.evil.com", "SESSDATA", "abc"),
            cookie("bilibili.com.evil.com", "buvid3", "xyz"),
        ];
        let normalized = normalize_bilibili_cookies(&cookies);
        assert!(normalized.is_empty());
    }

    #[test]
    fn cookie_header_keeps_only_bilibili_domains() {
        let cookies = vec![
            cookie(".bilibili.com", "SESSDATA", "abc"),
            cookie(".bilibili.com", "bili_jct", "tok"),
            cookie(".doubleclick.net", "id", "x"),
        ];
        assert_eq!(
            cookie_header_for_bilibili(&cookies),
            "SESSDATA=abc; bili_jct=tok"
        );
    }

    #[test]
    fn cookie_header_skips_expired_cookies() {
        let past = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() - 60)
            .unwrap_or(0);
        let mut expired = cookie(".bilibili.com", "SESSDATA", "old");
        expired.expiration = past;
        let cookies = vec![expired, cookie(".bilibili.com", "buvid3", "x")];
        assert_eq!(cookie_header_for_bilibili(&cookies), "buvid3=x");
    }

    #[test]
    fn bilibili_domain_matching() {
        assert!(is_bilibili_domain("bilibili.com"));
        assert!(is_bilibili_domain(".bilibili.com"));
        assert!(is_bilibili_domain("www.bilibili.com"));
        assert!(is_bilibili_domain("passport.bilibili.com"));
        assert!(!is_bilibili_domain("bilibili.com.evil.com"));
        assert!(!is_bilibili_domain("notbilibili.com"));
        assert!(!is_bilibili_domain(""));
    }

    /// The rename that publishes the jar must be durable before the caller
    /// relies on it: a crash that loses the rename would leave yt-dlp with
    /// the stale jar (or none) even though the write reported success.
    ///
    /// Value: protects=the parent flush after the rename; fails_when=the
    /// rename is published without flushing its directory; why_new=the
    /// post-0.4.2 review found the missing parent fsync; seam=none (directory
    /// durability cannot be observed without a power loss, so a probe records
    /// the call).
    #[cfg(unix)]
    #[test]
    fn writing_the_jar_flushes_its_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        write_netscape_file(&path, &[cookie(".bilibili.com", "SESSDATA", "abc")]).unwrap();
        assert!(
            sync_probe::synced(path.parent().unwrap()),
            "the jar's parent must be flushed after the publishing rename"
        );
    }
}
