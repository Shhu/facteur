//! Sender logos: the brand behind an address. Looked up per domain, in order: BIMI (the logo the
//! brand publishes in its DNS, the one Gmail shows), icons declared by the home page,
//! `/apple-touch-icon.png`, `/favicon.ico`. Only the brand's own site is contacted, once per
//! domain, and the result (or its absence) is cached. Personal-address providers (gmail.com,
//! free.fr…) get no logo: it would be the provider's for everyone.

use std::io::Cursor;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;

use crate::autoconfig::registrable;

/// Shown at 36 px, sharp on high-density screens.
const SIZE: u32 = 64;
const MAX_IMAGE: usize = 2 * 1024 * 1024;
const MAX_PAGE: usize = 768 * 1024;
const MAX_LOGO: usize = 64 * 1024;

/// Approval key of a sender (remote images): its domain, except at personal-address providers
/// where that would approve everyone (there, the address).
pub fn sender_key(email: &str) -> Option<String> {
    let email = email.trim().to_ascii_lowercase();
    let host = email.rsplit_once('@')?.1.to_string();
    let domain = registrable(&host);
    if domain.is_empty() {
        return None;
    }
    Some(if crate::autoconfig::is_public_domain(&domain) { email } else { domain })
}

pub fn logo_domain(email: &str) -> Option<String> {
    let host = email.rsplit_once('@')?.1.trim().to_ascii_lowercase();
    let domain = registrable(&host);
    (!domain.is_empty() && domain.contains('.') && !crate::autoconfig::is_public_domain(&domain)).then_some(domain)
}

/// Whether facteur may contact the sender's domain for its logo. Fetching it tells that domain
/// (its web server, its DNS) that this mailbox is live and being read, the moment the list is
/// displayed: only for senders the user wrote to, or whose domain the provider authenticated
/// (DMARC pass, verdict from the account's usual server), and never for an untrustworthy message.
pub fn logo_allowed(trust: &crate::trust::Trust, signals: &crate::trust::Signals, context: &crate::trust::Context) -> bool {
    if trust.is_low() || context.blocked || context.in_junk {
        return false;
    }
    context.known_correspondent || (context.foreign_results.is_none() && signals.dmarc == Some(true))
}

/// Reachable on the Internet: no loopback, private, link-local or reserved ranges. A malicious
/// domain could otherwise make facteur query the user's router or a local service.
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64.0.0/10 (CGNAT)
                || (o[0] == 198 && (o[1] & 0xFE) == 18) // 198.18.0.0/15
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            // IPv6 addresses carrying an IPv4 one are judged by it (NAT64 64:ff9b::/96, 6to4
            // 2002::/16, the old "compatible" ::a.b.c.d).
            let embedded = |hi: u16, lo: u16| std::net::Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                return is_public(IpAddr::V4(embedded(s[6], s[7])));
            }
            if s[0] == 0x2002 {
                return is_public(IpAddr::V4(embedded(s[1], s[2])));
            }
            if s[..6] == [0, 0, 0, 0, 0, 0] {
                return is_public(IpAddr::V4(embedded(s[6], s[7])));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xFE00) == 0xFC00 // fc00::/7, unique local
                || (s[0] & 0xFFC0) == 0xFE80 // fe80::/10, link-local
                || (s[0] == 0x2001 && s[1] == 0x0DB8)) // documentation
        }
    }
}

pub(crate) struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let public: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.filter(|a| is_public(a.ip())).collect();
            if public.is_empty() {
                return Err(crate::tr!("{host} : aucune adresse publique", "{host}: no public address").into());
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// https, and no hard-coded non-public IP (it does not go through the DNS).
pub(crate) fn allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && match url.host() {
            Some(url::Host::Domain(_)) => true,
            Some(url::Host::Ipv4(ip)) => is_public(IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => is_public(IpAddr::V6(ip)),
            None => false,
        }
}

fn client() -> Option<reqwest::Client> {
    // reqwest is built without a default crypto provider: install the engine's.
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .dns_resolver(Arc::new(PublicOnly))
        // A proxy (system or HTTPS_PROXY) would resolve the name itself, past the public-only check.
        .no_proxy()
        // At most 4 redirects, always https, never to a private IP.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 4 || !allowed(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .user_agent("Mozilla/5.0 (compatible; facteur)")
        .build()
        .ok()
}

/// Cut as soon as it exceeds `max` (a server may not announce its size).
async fn get_capped(http: &reqwest::Client, url: &str, max: usize) -> Option<(String, Vec<u8>)> {
    let url = reqwest::Url::parse(url).ok().filter(allowed)?;
    let mut resp = http.get(url).send().await.ok()?.error_for_status().ok()?;
    if resp.content_length().is_some_and(|n| n as usize > max) {
        return None;
    }
    let final_url = resp.url().to_string();
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if body.len() + chunk.len() > max {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some((final_url, body))
}

/// A 64 px PNG as a data URL.
pub async fn fetch_logo(domain: &str) -> Option<String> {
    let http = client()?;
    if let Some(logo) = bimi(&http, domain).await {
        return Some(logo);
    }
    for site in [format!("https://{domain}/"), format!("https://www.{domain}/")] {
        if let Some((final_url, page)) = get_capped(&http, &site, MAX_PAGE).await
            && let Some(url) = best_icon(&final_url, &String::from_utf8_lossy(&page))
            && let Some(img) = image(&http, &url).await
        {
            return Some(img);
        }
    }
    for path in ["apple-touch-icon.png", "favicon.ico"] {
        if let Some(img) = image(&http, &format!("https://{domain}/{path}")).await {
            return Some(img);
        }
    }
    None
}

/// BIMI : `default._bimi.<domaine>` TXT `v=BIMI1; l=https://…/logo.svg`.
async fn bimi(http: &reqwest::Client, domain: &str) -> Option<String> {
    let resolver = TokioResolver::builder_tokio().ok()?.build().ok()?;
    let lookup = tokio::time::timeout(Duration::from_secs(3), resolver.txt_lookup(format!("default._bimi.{domain}")))
        .await
        .ok()?
        .ok()?;
    let record: String = lookup.answers().iter().find_map(|r| match &r.data {
        RData::TXT(txt) => Some(txt.txt_data.iter().map(|part| String::from_utf8_lossy(part).into_owned()).collect()),
        _ => None,
    })?;
    let url = record
        .split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("l"))
        .map(|(_, v)| v.trim().to_string())?;
    image(http, &url).await
}

/// Downloaded then redrawn as PNG by facteur: the WebView never gets the server's bytes, only an
/// image produced here by decoders written in Rust.
async fn image(http: &reqwest::Client, url: &str) -> Option<String> {
    let (_, bytes) = get_capped(http, url, MAX_IMAGE).await?;
    let png = tokio::task::spawn_blocking(move || to_png(&bytes)).await.ok()??;
    (png.len() <= MAX_LOGO).then(|| format!("data:image/png;base64,{}", B64.encode(&png)))
}

/// Shown up to 48 px, sharp on dense screens.
const PICTURE_SIZE: u32 = 256;
const MAX_UPLOAD: usize = 10 * 1024 * 1024;

/// Redrawn here as a PNG of `PICTURE_SIZE` px at most: the raw upload is never stored. `None`:
/// not an image, or too large.
pub fn account_picture(data_url: &str) -> Option<String> {
    let (head, data) = data_url.strip_prefix("data:")?.split_once(',')?;
    if !head.starts_with("image/") || !head.ends_with(";base64") || data.len() > MAX_UPLOAD / 3 * 4 + 4 {
        return None;
    }
    let bytes = B64.decode(data.trim()).ok()?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let picture = reader.decode().ok()?.thumbnail(PICTURE_SIZE, PICTURE_SIZE);
    let mut out = Vec::new();
    picture.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
    Some(format!("data:image/png;base64,{}", B64.encode(&out)))
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    head.contains("<svg")
}

fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    if is_svg(bytes) { svg_to_png(bytes) } else { raster_to_png(bytes) }
}

fn raster_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    // A small image declaring a huge size (decompression bomb) is refused.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let picture = reader.decode().ok()?.thumbnail(SIZE, SIZE);
    let mut out = Vec::new();
    picture.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
    Some(out)
}

fn svg_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    // No external resources: by default usvg would read local files referenced by
    // `<image href="…">`.
    let options = usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let size = tree.size();
    let scale = SIZE as f32 / size.width().max(size.height());
    let width = ((size.width() * scale).ceil() as u32).clamp(1, SIZE);
    let height = ((size.height() * scale).ceil() as u32).clamp(1, SIZE);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().ok()
}

/// `apple-touch-icon`, else the largest `icon`, else an SVG icon; URL made absolute.
pub fn best_icon(base: &str, html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut candidates: Vec<(u32, String)> = Vec::new();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("<link") {
        let start = from + pos;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        from = end;
        let Some(rel) = attr(tag, tag_lower, "rel") else { continue };
        let rel = rel.to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "icon" || r == "apple-touch-icon" || r == "apple-touch-icon-precomposed") {
            continue;
        }
        let Some(href) = attr(tag, tag_lower, "href") else { continue };
        let size = attr(tag, tag_lower, "sizes")
            .and_then(|s| s.split(['x', 'X']).next().and_then(|n| n.trim().parse::<u32>().ok()))
            .unwrap_or(if rel.contains("apple-touch-icon") { 180 } else if href.ends_with(".svg") { 120 } else { 16 });
        candidates.push((size, href.to_string()));
    }
    // 512 px and up is often a sharing image, not an icon: keep the largest below.
    candidates.sort_by_key(|(s, _)| if *s > 512 { 0 } else { *s });
    let href = candidates.pop()?.1;
    absolute(base, &html_unescape(&href))
}

fn attr<'a>(tag: &'a str, tag_lower: &str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=");
    let mut search = 0;
    loop {
        let pos = search + tag_lower[search..].find(&key)?;
        // `data-href=` must not pass for `href=`.
        let preceded = tag_lower[..pos].chars().last().is_none_or(|c| c.is_whitespace());
        let value_start = pos + key.len();
        if !preceded {
            search = value_start;
            continue;
        }
        let rest = &tag[value_start..];
        return match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next(),
            _ => rest.split(|c: char| c.is_whitespace() || c == '>').next(),
        };
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&#x2F;", "/").replace("&#47;", "/")
}

fn absolute(base: &str, href: &str) -> Option<String> {
    if href.starts_with("data:") {
        return None;
    }
    reqwest::Url::parse(base).ok()?.join(href).ok().map(String::from).filter(|u| u.starts_with("http"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_keys() {
        assert_eq!(sender_key("News@Mail.Airbnb.com").as_deref(), Some("airbnb.com"));
        assert_eq!(sender_key("marie@gmail.com").as_deref(), Some("marie@gmail.com"), "not all of gmail.com");
        assert_eq!(sender_key("pas-une-adresse"), None);
    }

    #[test]
    fn logos_only_for_authenticated_or_known_senders() {
        use crate::trust::{Context, Signals, evaluate};
        let check = |signals: Signals, context: Context| logo_allowed(&evaluate(&signals, &context), &signals, &context);
        let dmarc = Signals { dmarc: Some(true), dkim: Some(true), ..Default::default() };
        assert!(check(dmarc.clone(), Context::default()));
        assert!(!check(Signals::default(), Context::default()), "unauthenticated stranger");
        assert!(check(Signals::default(), Context { known_correspondent: true, ..Default::default() }));
        assert!(!check(dmarc.clone(), Context { foreign_results: Some("faux".into()), ..Default::default() }), "forged verdict");
        assert!(!check(dmarc.clone(), Context { in_junk: true, ..Default::default() }));
        assert!(!check(dmarc, Context { blocked: true, known_correspondent: true, ..Default::default() }));
    }

    #[test]
    fn private_suffixes_are_separate_senders() {
        assert_eq!(sender_key("it@contoso.onmicrosoft.com").as_deref(), Some("contoso.onmicrosoft.com"));
        assert_eq!(sender_key("x@evil.pages.dev").as_deref(), Some("evil.pages.dev"));
        assert_eq!(logo_domain("noreply@someone.github.io").as_deref(), Some("someone.github.io"));
    }

    #[test]
    fn only_brands_get_logos() {
        assert_eq!(logo_domain("automated@airbnb.com").as_deref(), Some("airbnb.com"));
        assert_eq!(logo_domain("notifications@mail.github.com").as_deref(), Some("github.com"));
        assert_eq!(logo_domain("contact@impots.gouv.fr").as_deref(), Some("impots.gouv.fr"));
        assert_eq!(logo_domain("jean@gmail.com"), None);
        assert_eq!(logo_domain("marie@free.fr"), None);
    }

    #[test]
    fn private_addresses_are_refused() {
        for ip in ["127.0.0.1", "10.0.0.8", "192.168.1.1", "172.16.4.2", "169.254.1.1", "100.64.0.1", "0.0.0.0", "::1", "fe80::1", "fd00::1", "::ffff:192.168.1.1"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} must be refused");
        }
        for ip in ["8.8.8.8", "140.82.112.3", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip} is public");
        }
        let ok = |u: &str| allowed(&reqwest::Url::parse(u).unwrap());
        assert!(ok("https://github.com/favicon.ico"));
        assert!(!ok("http://github.com/favicon.ico"), "https only");
        assert!(!ok("https://192.168.1.1/"), "hard-coded private IP");
        assert!(!ok("file:///C:/Windows/win.ini"));
    }

    #[test]
    fn images_are_redrawn_as_small_png() {
        // SVG with a script and a local image: rendered without either, as a 64 px PNG.
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="200" height="100">
            <script>alert(1)</script><image xlink:href="file:///etc/passwd" width="10" height="10"/>
            <rect width="200" height="100" fill="red"/></svg>"#;
        let png = to_png(svg).unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (64, 32));

        // A raster image declaring a huge size: refused without decoding it all.
        let mut bomb = Vec::new();
        image::RgbaImage::new(1, 1).write_to(&mut Cursor::new(&mut bomb), image::ImageFormat::Png).unwrap();
        bomb[16..20].copy_from_slice(&100_000u32.to_be_bytes()); // announced width: 100,000 px
        assert!(to_png(&bomb).is_none());
        assert!(to_png(b"<html>pas une image</html>").is_none());
    }

    #[test]
    fn uploaded_pictures_are_redrawn() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(600, 300, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let out = account_picture(&format!("data:image/png;base64,{}", B64.encode(&png))).unwrap();
        let bytes = B64.decode(out.strip_prefix("data:image/png;base64,").unwrap()).unwrap();
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!((back.width(), back.height()), (256, 128));
        assert!(account_picture("data:text/html;base64,PGI+").is_none());
        assert!(account_picture("data:image/png;base64,bm90IGFuIGltYWdl").is_none());
    }

    #[test]
    fn picks_the_best_declared_icon() {
        let page = r#"<head>
            <link rel="icon" href="/favicon-16.png" sizes="16x16">
            <link data-href="/piege.png" rel="stylesheet" href="/style.css">
            <link rel="icon" type="image/png" href="/favicon-32.png" sizes="32x32">
            <link rel="apple-touch-icon" href="https://cdn.example.com/touch.png?v=2&amp;x=1">
            <link rel="icon" href="/og.png" sizes="1200x630">
        </head>"#;
        assert_eq!(best_icon("https://www.example.com/fr/", page).as_deref(), Some("https://cdn.example.com/touch.png?v=2&x=1"));
        let relative = r#"<link rel='shortcut icon' href='img/f.ico'>"#;
        assert_eq!(best_icon("https://example.com/a/", relative).as_deref(), Some("https://example.com/a/img/f.ico"));
        assert_eq!(best_icon("https://example.com/", "<p>rien</p>"), None);
    }
}

#[cfg(test)]
mod addresses {
    #[test]
    fn ipv6_forms_of_private_ipv4_are_refused() {
        for ip in ["64:ff9b::c0a8:101", "2002:c0a8:101::1", "::192.168.1.1", "::ffff:10.0.0.1", "192.0.0.8"] {
            assert!(!super::is_public(ip.parse().unwrap()), "{ip}");
        }
        assert!(super::is_public("64:ff9b::808:808".parse().unwrap()));
    }
}

#[cfg(test)]
mod network {
    /// cargo test -p facteur-core logos_from_the_web -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs the network"]
    async fn logos_from_the_web() {
        for domain in ["airbnb.com", "github.com", "sncf-connect.com", "laposte.fr"] {
            let logo = super::fetch_logo(domain).await;
            let kind = logo.as_deref().map(|l| l.split(';').next().unwrap_or("").to_string());
            println!("{domain:<20} {:?} {} octets", kind, logo.as_ref().map_or(0, |l| l.len() * 3 / 4));
            assert!(logo.is_some(), "{domain} sans logo");
        }
    }
}
