//! Self-update: check GitHub for a newer release, download the asset for this
//! platform, verify it, smoke-test it, and swap the running binary atomically.
//!
//! Guarantees, in the order they are enforced by [`apply`]:
//! 1. the tag is newer than the running one, and a binary exists for this CPU;
//! 2. the download's SHA-256 matches the one listed in the release's
//!    `SHA256SUMS.txt` (our own SHA-256, so no crypto dependency);
//! 3. the extracted binary runs here and reports the version we asked for;
//! 4. only then is it `rename`d over the running `deeptui` — an interrupted or
//!    failing update can never leave a half-written binary in place.
//!
//! The startup check is one cached HTTPS GET at most once a day
//! ([`CHECK_TTL`]), skipped entirely with `update_check = false`,
//! `DEEPTUI_NO_UPDATE_CHECK=1` or `--no-update-check`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Release assets live in `lemonhub-io/deeptui`: the API reports the newest tag
/// and `/releases/latest` (redirect) is the rate-limit-proof fallback.
const RELEASES: &str = "https://github.com/lemonhub-io/deeptui/releases";
const LATEST_API: &str = "https://api.github.com/repos/lemonhub-io/deeptui/releases/latest";

/// A completed check is trusted for this long — one round-trip per day.
pub const CHECK_TTL: u64 = 24 * 60 * 60;

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release triple for this host, `None` when we publish no binary for it.
pub fn target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        // static musl: no glibc floor, and the same asset on every distro
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// `deeptui-v0.1.0-beta-x86_64-unknown-linux-musl.tar.gz` (`.zip` on Windows),
/// matching `.github/workflows/release.yml`.
pub fn asset_name(tag: &str, triple: &str) -> String {
    let ext = if triple.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("deeptui-{tag}-{triple}.{ext}")
}

/// Numeric components of a tag (`v1.2.3-rc.1` → `[1, 2, 3]`) plus any
/// pre-release suffix.
pub fn parse_version(tag: &str) -> (Vec<u64>, Option<&str>) {
    let s = tag.trim().trim_start_matches(['v', 'V']);
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (nums, rest) = s.split_at(end);
    let nums = nums.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let pre = rest.trim_start_matches(['-', '+']);
    (nums, (!pre.is_empty()).then_some(pre))
}

/// True when `candidate` is a strictly newer release than `current`.
/// Missing components are zero (`v1.2` == `v1.2.0`), and a plain release beats
/// a pre-release with the same numbers (`1.0.0` > `1.0.0-rc.1`).
pub fn is_newer(current: &str, candidate: &str) -> bool {
    let (a, apre) = parse_version(current);
    let (b, bpre) = parse_version(candidate);
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return y > x;
        }
    }
    apre.is_some() && bpre.is_none()
}

/// `0.1.0-beta` → `v0.1.0-beta` (release tags always carry the `v`).
pub fn normalize_tag(tag: &str) -> Result<String> {
    let t = tag.trim();
    if t.is_empty() || !t.chars().any(|c| c.is_ascii_digit()) {
        bail!("not a release tag: {tag:?}");
    }
    Ok(if t.starts_with('v') {
        t.to_string()
    } else {
        format!("v{t}")
    })
}

// ---------------------------------------------------------------- sha256 ----

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn compress(h: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*c);
    }
    for i in 16..64 {
        let (a, b) = (w[i - 15], w[i - 2]);
        let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
        let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let mut v = *h;
    for i in 0..64 {
        let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
        let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
        let t1 = v[7]
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
        let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        let t2 = s0.wrapping_add(maj);
        v = [
            t1.wrapping_add(t2),
            v[0],
            v[1],
            v[2],
            v[3].wrapping_add(t1),
            v[4],
            v[5],
            v[6],
        ];
    }
    for (x, y) in h.iter_mut().zip(v) {
        *x = x.wrapping_add(y);
    }
}

/// FIPS 180-4 SHA-256. Hand-rolled: 40 lines, no crypto crate in the binary.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let (blocks, rem) = data.as_chunks::<64>();
    for c in blocks {
        compress(&mut h, c);
    }
    // pad: 0x80, zeros, then the 64-bit big-endian bit length in the last 8
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let len = if rem.len() < 56 { 64 } else { 128 };
    let bits = (data.len() as u64).wrapping_mul(8);
    tail[len - 8..len].copy_from_slice(&bits.to_be_bytes());
    for c in tail[..len].as_chunks::<64>().0 {
        compress(&mut h, c);
    }
    let mut out = [0u8; 32];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

/// SHA-256 listed for `asset` in a `sha256sum`-format file. Both the
/// `sum  name` and `sum *name` forms are accepted, plus `.`-prefixed names.
pub fn parse_sums(text: &str, asset: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let sum = it.next()?;
        let name = it.next()?.trim_start_matches(['*', '.', '/']);
        let hex64 = sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit());
        (hex64 && name == asset).then(|| sum.to_ascii_lowercase())
    })
}

// --------------------------------------------------------------- unpacking --

pub fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::with_capacity(data.len().saturating_mul(4));
    flate2::read::GzDecoder::new(data)
        .read_to_end(&mut out)
        .context("corrupt .tar.gz (gzip stream)")?;
    Ok(out)
}

fn field(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).unwrap_or("")
}

/// The contents of the member named `name` in an uncompressed tar.
///
/// Deliberately ustar-only: the release archives are built by `tar -czf` from
/// short, flat paths, so pax/gnu long-name extensions never appear and a full
/// tar implementation would be dead weight.
pub fn tar_entry<'a>(tar: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let mut off = 0;
    while off + 512 <= tar.len() {
        let h = &tar[off..off + 512];
        if h.iter().all(|b| *b == 0) {
            return None; // end-of-archive marker
        }
        let Ok(size) = usize::from_str_radix(field(&h[124..136]).trim(), 8) else {
            return None; // malformed header: stop rather than misparse
        };
        let Some(body) = tar.get(off + 512..off + 512 + size) else {
            return None; // truncated
        };
        if field(&h[0..100]).rsplit('/').next() == Some(name) {
            return Some(body);
        }
        off += 512 + size.div_ceil(512) * 512;
    }
    None
}

/// The contents of the entry named `name` in a zip.
///
/// Local file headers only: the release `.zip` holds a handful of small files
/// written with their sizes inline (no streamed data descriptors), and a
/// central-directory reader would be a lot of code for no extra safety —
/// the payload is checksum-verified before it is used either way.
pub fn zip_entry(zip: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut i = 0;
    while let Some(rel) = zip.get(i..)?.windows(4).position(|w| w == b"PK\x03\x04") {
        let off = i + rel;
        i = off + 4;
        let h = zip.get(off..off + 30)?;
        let method = u16::from_le_bytes([h[8], h[9]]);
        let csize = u32::from_le_bytes([h[18], h[19], h[20], h[21]]) as usize;
        let nlen = u16::from_le_bytes([h[26], h[27]]) as usize;
        let elen = u16::from_le_bytes([h[28], h[29]]) as usize;
        let raw = zip.get(off + 30..off + 30 + nlen)?;
        let body = zip.get(off + 30 + nlen + elen..)?;
        // a non-UTF-8 name is not ours: keep scanning instead of giving up
        if std::str::from_utf8(raw)
            .ok()
            .and_then(|n| n.rsplit('/').next())
            != Some(name)
        {
            continue;
        }
        return match method {
            0 => body.get(..csize).map(<[u8]>::to_vec),
            // raw deflate (zip method 8), not zlib-wrapped
            8 => {
                use std::io::Read;
                let mut out = Vec::with_capacity(csize.max(64));
                flate2::read::DeflateDecoder::new(body)
                    .read_to_end(&mut out)
                    .ok()?;
                Some(out)
            }
            _ => None,
        };
    }
    None
}

/// The executable out of a release asset: `.tar.gz` everywhere, `.zip` on
/// Windows (the packaging step mirrors the platform).
pub fn extract(asset: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let (name, found) = if asset.ends_with(".zip") {
        ("deeptui.exe", zip_entry(bytes, "deeptui.exe"))
    } else {
        let tar = gunzip(bytes)?;
        ("deeptui", tar_entry(&tar, "deeptui").map(<[u8]>::to_vec))
    };
    found.with_context(|| format!("{asset} contains no {name}"))
}

// ------------------------------------------------------------ installing ----

/// Run `exe --version` and return the version it reports.
pub fn smoke(exe: &Path) -> Option<String> {
    let out = std::process::Command::new(exe)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Some(stdout.trim().strip_prefix("deeptui ")?.trim().to_string())
}

fn bin_name(exe: &Path) -> String {
    exe.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "deeptui".into())
}

/// Write `bytes` next to `exe` as an executable temp file (same directory, so
/// the final step is a rename within one filesystem). Returns the temp path.
pub fn stage(exe: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let dir = exe
        .parent()
        .context("no install directory for the binary")?;
    let tmp = dir.join(format!(".{}.new.{}", bin_name(exe), std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(tmp)
}

/// Move a staged file over `exe`. On Unix this is a plain rename, which keeps
/// the currently running process on its old inode. On Windows a running image
/// cannot be overwritten but can be renamed out of the way first.
pub fn commit(staged: &Path, exe: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let old = exe.with_file_name(format!("{}.old", bin_name(exe)));
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old).with_context(|| format!("cannot move {}", exe.display()))?;
        if let Err(e) = std::fs::rename(staged, exe) {
            let _ = std::fs::rename(&old, exe); // put the old binary back
            return Err(e).with_context(|| format!("cannot install {}", exe.display()));
        }
        let _ = std::fs::remove_file(&old);
        return Ok(());
    }
    #[cfg(not(windows))]
    std::fs::rename(staged, exe).with_context(|| format!("cannot install {}", exe.display()))
}

/// Drop leftovers from earlier updates (`.deeptui.new.*`, `deeptui.old`) in the
/// install directory. Entries younger than [`STALE_GRACE`] are left alone so a
/// concurrent update in another shell is never deleted mid-flight.
pub fn cleanup_stale() {
    const STALE_GRACE: Duration = Duration::from_secs(600);
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    let name = bin_name(&exe);
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let mine = n == format!("{name}.old")
            || (n.starts_with(&format!(".{name}.new.")) && n.len() > name.len() + 6);
        if !mine {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().unwrap_or_default() > STALE_GRACE)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ------------------------------------------------------------------ HTTP ----

pub fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("deeptui/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .build()?)
}

/// Tag of the latest release. The API answers directly; when it rate-limits
/// anonymous callers (60/h/IP) the `/releases/latest` redirect is the fallback
/// — no extra request either way, reqwest keeps the final URL.
pub async fn latest_tag(http: &reqwest::Client) -> Result<String> {
    if let Ok(r) = http.get(LATEST_API).send().await {
        if r.status().is_success() {
            if let Ok(v) = r.json::<serde_json::Value>().await {
                if let Some(t) = v.get("tag_name").and_then(|t| t.as_str()) {
                    return normalize_tag(t);
                }
            }
        }
    }
    let r = http
        .get(format!("{RELEASES}/latest"))
        .send()
        .await
        .context("cannot reach github.com")?;
    let tag = r.url().path().rsplit('/').next().unwrap_or("").to_string();
    if tag.is_empty() || tag == "latest" {
        bail!("cannot determine the latest release tag");
    }
    normalize_tag(&tag)
}

pub async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let r = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    if !r.status().is_success() {
        bail!("GET {url} → HTTP {}", r.status());
    }
    Ok(r.bytes().await.context("truncated download")?.to_vec())
}

// ------------------------------------------------------------- the update ---

pub enum Outcome {
    UpToDate(&'static str),
    Updated {
        from: &'static str,
        to: String,
        path: PathBuf,
    },
}

/// Download `want` (or the latest release), verify it, and install it over the
/// running binary. Nothing outside the temp file and the final rename touches
/// the install directory, and every check runs before the rename.
pub async fn apply(http: &reqwest::Client, want: Option<&str>) -> Result<Outcome> {
    let cur = current_version();
    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    let tag = match want {
        Some(t) => normalize_tag(t)?,
        None => latest_tag(http).await?,
    };
    if !is_newer(cur, &tag) {
        return Ok(Outcome::UpToDate(cur));
    }
    let triple = target_triple().context(
        "no prebuilt binary for this platform — reinstall with install.sh --from-source",
    )?;
    let asset = asset_name(&tag, triple);
    let base = format!("{RELEASES}/download/{tag}");
    println!("▸ deeptui {cur} → {tag} ({triple})");

    let bytes = fetch(http, &format!("{base}/{asset}")).await?;
    let sums = fetch(http, &format!("{base}/SHA256SUMS.txt"))
        .await
        .context("cannot verify the download: SHA256SUMS.txt is unavailable")?;
    let sums = String::from_utf8_lossy(&sums).into_owned();
    let want_sum = parse_sums(&sums, &asset)
        .with_context(|| format!("SHA256SUMS.txt lists no checksum for {asset}"))?;
    let got = sha256_hex(&bytes);
    if got != want_sum {
        bail!("checksum mismatch for {asset}\n  expected {want_sum}\n  got      {got}");
    }
    println!("▸ checksum verified ({})", &got[..8]);

    let payload = extract(&asset, &bytes)?;
    let staged = stage(&exe, &payload)?;
    match smoke(&staged) {
        Some(v) if parse_version(&v).0 == parse_version(&tag).0 => {}
        Some(v) => {
            let _ = std::fs::remove_file(&staged);
            bail!("{asset} reports deeptui {v} but the tag is {tag} — refusing to install");
        }
        None => {
            let _ = std::fs::remove_file(&staged);
            bail!("{asset} does not run on this machine (x86-64-v3 needs AVX2; older CPUs: install.sh --from-source)");
        }
    }
    commit(&staged, &exe)?;
    Ok(Outcome::Updated {
        from: cur,
        to: tag,
        path: exe,
    })
}

/// `deeptui --self-update`: update and report, script-friendly one-liners.
pub async fn run(http: &reqwest::Client, want: Option<&str>) -> Result<()> {
    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    let dir = exe
        .parent()
        .context("no install directory for the binary")?;
    // Probing the binary itself would fail with ETXTBSY while it is running
    // (and it is the rename into the directory that has to be permitted).
    if !can_write_dir(dir) {
        bail!(
            "cannot write to {} — reinstall with install.sh, or run this as the owner of {}",
            dir.display(),
            exe.display()
        );
    }
    match apply(http, want).await? {
        Outcome::UpToDate(v) => match want {
            Some(t) => println!("✓ deeptui {v} is already at or past {t}"),
            None => println!("✓ deeptui {v} is the latest release"),
        },
        Outcome::Updated { from, to, path } => {
            println!("✓ updated deeptui {from} → {to} ({})", path.display());
            println!("  restart deeptui to use the new build");
            cleanup_stale();
        }
    }
    Ok(())
}

/// The update needs to create a temp file next to the binary and rename it
/// over the binary: that is a directory permission, not a file one.
fn can_write_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".deeptui.probe.{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ------------------------------------------------------------- auto-check ---

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `<data_dir>/deeptui/update-check` — `DEEPTUI_UPDATE_CACHE` overrides it
/// (the tests point it at a temp dir so they never touch the real one).
fn cache_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEEPTUI_UPDATE_CACHE") {
        return Some(PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    dirs::data_dir().map(|d| d.join("deeptui").join("update-check"))
}

/// `<unix_ts>\t<tag>` newer than `ttl`: the answer to "is there an update?"
/// without a network round-trip.
pub fn read_cache(path: &Path, now: u64, ttl: u64) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut f = text.trim().split('\t');
    let ts: u64 = f.next()?.trim().parse().ok()?;
    let tag = f.next()?.trim();
    // a timestamp in the future means the clock moved, not fresh data
    let age = now.checked_sub(ts)?;
    (age <= ttl && !tag.is_empty()).then(|| tag.to_string())
}

pub fn write_cache(path: &Path, now: u64, tag: &str) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(path, format!("{now}\t{tag}\n"));
}

/// Newer tag from the cache only — no network. Used by the TUI banner, which
/// must not delay startup.
pub fn cached_update() -> Option<String> {
    let tag = read_cache(&cache_path()?, now(), CHECK_TTL)?;
    is_newer(current_version(), &tag).then_some(tag)
}

/// One check per [`CHECK_TTL`]: the cached tag when it is fresh, otherwise a
/// short-timeout lookup that refreshes the cache. `None` means "no update".
pub async fn check(enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    let path = cache_path();
    let now = now();
    let tag = match path.as_deref().and_then(|p| read_cache(p, now, CHECK_TTL)) {
        Some(t) => t,
        None => {
            let http = client(Duration::from_secs(6)).ok()?;
            let t = latest_tag(&http).await.ok()?;
            if let Some(p) = path.as_deref() {
                write_cache(p, now, &t);
            }
            t
        }
    };
    is_newer(current_version(), &tag).then_some(tag)
}

/// `deeptui --check-update`: report and exit.
pub async fn report(http: &reqwest::Client) -> Result<()> {
    let cur = current_version();
    let tag = latest_tag(http).await?;
    if is_newer(cur, &tag) {
        println!("deeptui {cur} → {tag} available");
        println!("update with: deeptui --self-update");
    } else {
        println!("deeptui {cur} is the latest release");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deeptui_testkit::TmpDir;

    const VS: &[&str] = &[
        "v0.1.0-beta",
        "v0.1.0",
        "v0.4.0",
        "v0.4.1",
        "v0.9.9",
        "v0.10.0",
        "v1.0.0-rc.1",
        "v1.0.0",
    ];

    #[test]
    fn version_order_is_numeric_not_lexicographic() {
        for (i, a) in VS.iter().enumerate() {
            for (j, b) in VS.iter().enumerate() {
                assert_eq!(is_newer(a, b), j > i, "{a} → {b}");
                assert!(!is_newer(a, a), "{a} is not newer than itself");
            }
        }
    }

    #[test]
    fn version_parsing_accepts_real_world_tags() {
        assert_eq!(parse_version("v1.2.3"), (vec![1, 2, 3], None));
        assert_eq!(parse_version("1.2"), (vec![1, 2], None));
        assert_eq!(parse_version("v2.0.0-rc.1"), (vec![2, 0, 0], Some("rc.1")));
        assert_eq!(parse_version("deeptui-v3"), (vec![0], Some("deeptui-v3")));
        // trailing components default to zero
        assert!(!is_newer("v1.2", "v1.2.0"));
        assert!(is_newer("v1.2", "v1.2.1"));
        // junk never looks newer
        assert!(!is_newer("v1.0.0", "nightly"));
        assert_eq!(normalize_tag("0.1.0-beta").unwrap(), "v0.1.0-beta");
        assert_eq!(normalize_tag(" v0.1.0-beta ").unwrap(), "v0.1.0-beta");
        assert!(normalize_tag("").is_err());
        assert!(normalize_tag("latest").is_err());
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        let t = "x86_64-unknown-linux-musl";
        assert_eq!(
            asset_name("v0.1.0-beta", t),
            "deeptui-v0.1.0-beta-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset_name("v0.1.0-beta", "x86_64-pc-windows-msvc"),
            "deeptui-v0.1.0-beta-x86_64-pc-windows-msvc.zip"
        );
        // this host must map to one of the published triples
        assert!(target_triple().is_some(), "no asset for this platform");
        assert!(!asset_name("v1.0.0", target_triple().unwrap()).is_empty());
    }

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 55/56 bytes straddle the single-block padding boundary
        assert_eq!(
            sha256_hex(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn checksum_lookup_handles_both_sha256sum_layouts() {
        let f = "8a1336011e4a887282c9cff6cc549ffbba18bbb3669c6a2ffe8b60521eb259e6  deeptui-a.tar.gz\n\
                 410dc5e76c648653f39328b3e45fa365986b6b9ffbb8ff728e71ecb04f3f4046 *deeptui-b.tar.gz\n\
                 bad  deeptui-c.tar.gz\n";
        assert_eq!(
            parse_sums(f, "deeptui-a.tar.gz").unwrap(),
            "8a1336011e4a887282c9cff6cc549ffbba18bbb3669c6a2ffe8b60521eb259e6"
        );
        assert_eq!(
            parse_sums(f, "deeptui-b.tar.gz").unwrap(),
            "410dc5e76c648653f39328b3e45fa365986b6b9ffbb8ff728e71ecb04f3f4046"
        );
        // short/bogus digests are ignored, and so are unknown names
        assert!(parse_sums(f, "deeptui-c.tar.gz").is_none());
        assert!(parse_sums(f, "deeptui-d.tar.gz").is_none());
        // Windows line endings and ./ prefixes
        assert!(parse_sums("ab\r\n", "x").is_none());
        assert_eq!(
            parse_sums(&format!("{}  ./deeptui", "a".repeat(64)), "deeptui").unwrap(),
            "a".repeat(64)
        );
        assert!(parse_sums("", "deeptui").is_none());
    }

    // ---- archive builders: the release assets, made by hand ----

    fn tar_header(name: &str, size: usize) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0'; // regular file
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h
    }

    fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries {
            out.extend_from_slice(&tar_header(name, data.len()));
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend_from_slice(&[0u8; 1024]); // two zero blocks end the archive
        out
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn zip(method: u16, name: &str, data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let body = if method == 8 {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        let mut out = vec![0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0]; // sig, version, flags
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc (unchecked)
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn tarball_extraction_picks_the_binary() {
        let bin = b"\x7fELF fake deeptui";
        let archive = gzip(&tar(&[
            (
                "deeptui-v9.9.9-x86_64-unknown-linux-musl/README.md",
                b"readme",
            ),
            (
                "deeptui-v9.9.9-x86_64-unknown-linux-musl/LICENSE",
                b"license",
            ),
            ("deeptui-v9.9.9-x86_64-unknown-linux-musl/deeptui", bin),
        ]));
        let asset = "deeptui-v9.9.9-x86_64-unknown-linux-musl.tar.gz";
        assert_eq!(extract(asset, &archive).unwrap(), bin);
        // a member whose name merely starts the same is not a match
        assert!(extract(asset, &gzip(&tar(&[("deeptui.dSYM", b"x")]))).is_err());
        assert!(extract(asset, b"not gzip").is_err());
    }

    #[test]
    fn tar_scanning_survives_padding_and_odd_members() {
        // 600 bytes of payload spans two blocks; the lookup continues past it
        let big = vec![b'z'; 600];
        let a = tar(&[("dir/big.txt", &big), ("dir/deeptui", b"bin")]);
        assert_eq!(tar_entry(&a, "deeptui").unwrap(), b"bin");
        assert!(tar_entry(&a, "missing").is_none());
        // truncated archive: no panic, just no answer
        assert!(tar_entry(&a[..700], "deeptui").is_none());
        assert!(tar_entry(&a[..20], "deeptui").is_none());
    }

    #[test]
    fn zip_extraction_handles_stored_and_deflated_entries() {
        let payload = b"\x7fELF windows deeptui";
        for method in [0u16, 8] {
            let z = zip(
                method,
                "deeptui-v9.9.9-x86_64-pc-windows-msvc/deeptui.exe",
                payload,
            );
            assert_eq!(
                extract("deeptui-v9.9.9-x86_64-pc-windows-msvc.zip", &z).unwrap(),
                payload,
                "method {method}"
            );
        }
        // a zip without the binary is an error, not a panic
        let z = zip(8, "deeptui-v9.9.9-win/README.md", b"readme");
        assert!(extract("deeptui-v9.9.9-x86_64-pc-windows-msvc.zip", &z).is_err());
        assert!(zip_entry(&z, "deeptui.exe").is_none());
        assert!(zip_entry(b"PK\x03\x04short", "deeptui.exe").is_none());
    }

    #[test]
    fn stage_commit_and_cleanup_leave_a_runnable_binary() {
        let t = TmpDir::new("update");
        let exe = t.join("deeptui");
        std::fs::write(&exe, b"old").unwrap();
        let staged = stage(&exe, b"new content").unwrap();
        assert_eq!(staged.parent(), Some(t.path()), "staged next to the target");
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"old",
            "staging must not touch the target"
        );
        commit(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new content");
        assert!(!staged.exists(), "the temp file is consumed by the rename");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(&exe).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o755);
        }
        // leftovers of a previous run are swept, recent ones are left alone
        std::fs::write(t.join(".deeptui.new.999"), b"x").unwrap();
        std::fs::write(t.join("deeptui.old"), b"x").unwrap();
        cleanup_stale();
        assert!(
            t.join(".deeptui.new.999").exists(),
            "recent files are spared"
        );
    }

    #[cfg(unix)]
    #[test]
    fn smoke_reads_the_version_a_binary_reports() {
        let t = TmpDir::new("smoke");
        let ok = t.join("ok");
        std::fs::write(&ok, "#!/bin/sh\necho deeptui 9.9.9\n").unwrap();
        let mut p = std::fs::metadata(&ok).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        p.set_mode(0o755);
        std::fs::set_permissions(&ok, p).unwrap();
        assert_eq!(smoke(&ok).as_deref(), Some("9.9.9"));

        let rubbish = t.join("rubbish");
        std::fs::write(&rubbish, "#!/bin/sh\necho hello\n").unwrap();
        std::fs::set_permissions(&rubbish, std::fs::metadata(&ok).unwrap().permissions()).unwrap();
        assert!(smoke(&rubbish).is_none());
        let failing = t.join("failing");
        std::fs::write(&failing, "#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(&failing, std::fs::metadata(&ok).unwrap().permissions()).unwrap();
        assert!(smoke(&failing).is_none());
        assert!(smoke(&t.join("absent")).is_none());
    }

    #[test]
    fn install_directory_write_probe() {
        let t = TmpDir::new("probe");
        assert!(can_write_dir(t.path()));
        // read-only directory: nothing may be staged or renamed into it
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ro = t.join("ro");
            std::fs::create_dir(&ro).unwrap();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
            assert!(!can_write_dir(&ro));
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(!can_write_dir(&t.join("absent")));
        // the probe never lingers
        let n = std::fs::read_dir(t.path()).unwrap().count();
        assert_eq!(n, 1, "only the read-only dir is there");
    }

    #[test]
    fn cache_answers_within_the_ttl_only() {
        let t = TmpDir::new("cache");
        let p = t.join("nested/update-check");
        let now = 1_700_000_000;
        assert!(read_cache(&p, now, CHECK_TTL).is_none(), "missing file");
        write_cache(&p, now, "v9.9.9");
        assert_eq!(read_cache(&p, now, CHECK_TTL).as_deref(), Some("v9.9.9"));
        assert_eq!(
            read_cache(&p, now + CHECK_TTL, CHECK_TTL).as_deref(),
            Some("v9.9.9"),
            "the window is inclusive"
        );
        assert!(
            read_cache(&p, now + CHECK_TTL + 1, CHECK_TTL).is_none(),
            "stale"
        );
        // garbage, an empty tag and a clock that jumped back are all "unknown"
        std::fs::write(&p, "junk\n").unwrap();
        assert!(read_cache(&p, now, CHECK_TTL).is_none());
        std::fs::write(&p, format!("{now}\t\n")).unwrap();
        assert!(read_cache(&p, now, CHECK_TTL).is_none());
        std::fs::write(&p, format!("{}\tv9.9.9\n", now + 10_000)).unwrap();
        assert!(read_cache(&p, now, CHECK_TTL).is_none());
    }

    #[test]
    fn the_cache_lives_where_the_environment_says() {
        let t = TmpDir::new("envcache");
        let p = t.join("u");
        std::env::set_var("DEEPTUI_UPDATE_CACHE", &p);
        assert_eq!(cache_path().as_deref(), Some(p.as_path()));
        // a cached newer tag is what the banner and the check report
        write_cache(&p, now(), "v99.0.0");
        assert_eq!(cached_update().as_deref(), Some("v99.0.0"));
        write_cache(&p, now(), &format!("v{}", current_version()));
        assert!(
            cached_update().is_none(),
            "the running version is not an update"
        );
        std::env::remove_var("DEEPTUI_UPDATE_CACHE");
        assert!(cache_path()
            .map(|p| p.ends_with("deeptui/update-check"))
            .unwrap_or(true));
    }
}
