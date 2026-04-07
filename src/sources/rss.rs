use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use std::time::Duration;

/// A parsed item from an RSS or Atom feed.
#[derive(Debug, Clone)]
pub struct FeedItem {
    pub title: String,
    pub link: String,
    pub description: String,
    pub published: Option<DateTime<Utc>>,
    pub feed_name: String,
}

/// Default tech news and sensitive-content RSS/Atom feeds.
/// Covers tech news, world affairs, security, and government/policy.
pub const DEFAULT_FEEDS: &[(&str, &str)] = &[
    // ── Core tech news ─────────────────────────────────────────────────────
    ("Ars Technica", "https://feeds.arstechnica.com/arstechnica/technology-lab"),
    ("The Verge", "https://www.theverge.com/rss/index.xml"),
    ("TechCrunch", "https://techcrunch.com/feed"),
    ("Wired", "https://www.wired.com/feed/rss"),
    ("Slashdot", "https://rss.slashdot.org/Slashdot/slashdotMain"),
    ("Lobsters", "https://lobste.rs/rss"),
    ("HN Front Page", "https://hnrss.org/frontpage?count=50"),
    ("HN Ask", "https://hnrss.org/ask?count=30"),
    ("HN Show", "https://hnrss.org/show?count=30"),
    ("MIT Technology Review", "https://www.technologyreview.com/feed/"),
    ("IEEE Spectrum", "https://spectrum.ieee.org/feeds/feed.rss"),
    // ── World & government news ────────────────────────────────────────────
    ("BBC News Technology", "https://feeds.bbci.co.uk/news/technology/rss.xml"),
    ("BBC News World", "https://feeds.bbci.co.uk/news/world/rss.xml"),
    ("Reuters Technology", "https://feeds.reuters.com/reuters/technologyNews"),
    ("AP Technology", "https://feeds.apnews.com/rss/apf-technology"),
    ("The Guardian Tech", "https://www.theguardian.com/technology/rss"),
    ("The Guardian World", "https://www.theguardian.com/world/rss"),
    // ── Security / breach ─────────────────────────────────────────────────
    ("Krebs on Security", "https://krebsonsecurity.com/feed/"),
    ("Bleeping Computer", "https://www.bleepingcomputer.com/feed/"),
    ("SecurityWeek", "https://feeds.feedburner.com/Securityweek"),
    ("The Hacker News", "https://feeds.feedburner.com/TheHackersNews"),
    // ── AI / research ─────────────────────────────────────────────────────
    ("Hugging Face Blog", "https://huggingface.co/blog/feed.xml"),
    ("Google AI Blog", "https://blog.research.google/feeds/posts/default"),
    ("Anthropic News", "https://www.anthropic.com/rss.xml"),
    // ── Google News: broad search terms ───────────────────────────────────
    (
        "Google News: Leaks/Censored",
        "https://news.google.com/rss/search?q=(leaked+OR+censored+OR+banned+OR+breach)+(tech+OR+github+OR+AI+OR+software)&hl=en-US&gl=US&ceid=US:en",
    ),
    (
        "Google News: AI/Open Source",
        "https://news.google.com/rss/search?q=(artificial+intelligence+OR+open+source)+(release+OR+launches+OR+cuts+OR+ban)&hl=en-US&gl=US&ceid=US:en",
    ),
    (
        "Google News: Government/Tech Policy",
        "https://news.google.com/rss/search?q=(government+OR+congress+OR+parliament+OR+regulation+OR+antitrust)+(AI+OR+tech+OR+cyber+OR+digital)&hl=en-US&gl=US&ceid=US:en",
    ),
    (
        "Google News: Cyber/Security",
        "https://news.google.com/rss/search?q=(cyberattack+OR+data+breach+OR+ransomware+OR+zero-day+OR+hacked)&hl=en-US&gl=US&ceid=US:en",
    ),
    (
        "Google News: Geopolitics/Tech",
        "https://news.google.com/rss/search?q=(sanctions+OR+ban+OR+seized+OR+arrested+OR+indicted)+(technology+OR+AI+OR+software+OR+chip)&hl=en-US&gl=US&ceid=US:en",
    ),
];

pub struct RssSource {
    client: Client,
}

impl Default for RssSource {
    fn default() -> Self {
        Self::new()
    }
}

impl RssSource {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("repo-radar/1.0 (+https://github.com/lemwaiping123-eng/repo-radar)")
            .build()
            .unwrap_or_default();
        Self { client }
    }

    /// Fetch and parse all default feeds. Errors on individual feeds are logged and skipped.
    pub async fn fetch_all(&self) -> Vec<FeedItem> {
        let mut results = Vec::new();
        for (name, url) in DEFAULT_FEEDS {
            match self.fetch_feed(name, url).await {
                Ok(items) => results.extend(items),
                Err(e) => tracing::debug!(feed=%name, error=%e, "RSS feed fetch failed"),
            }
        }
        results
    }

    /// Fetch and parse a single feed by URL. Public for use in web layer.
    pub async fn fetch_one(&self, name: &str, url: &str) -> Result<Vec<FeedItem>> {
        self.fetch_feed(name, url).await
    }

    async fn fetch_feed(&self, name: &str, url: &str) -> Result<Vec<FeedItem>> {
        let body = self
            .client
            .get(url)
            .send()
            .await
            .context("RSS HTTP request failed")?
            .text()
            .await
            .context("RSS response read failed")?;

        parse_feed(&body, name).context("RSS parse failed")
    }
}

// ---------------------------------------------------------------------------
// Minimal hand-rolled RSS/Atom parser (no extra crate needed)
// ---------------------------------------------------------------------------

/// Extract the text content of the first matching XML tag.
fn extract_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)?;
    // skip to end of opening tag
    let after_open = xml[start..].find('>')? + start + 1;
    // handle CDATA: <tag><![CDATA[content]]></tag>
    let content = if xml[after_open..].starts_with("<![CDATA[") {
        let cdata_start = after_open + 9; // len("<![CDATA[")
        let cdata_end = xml[cdata_start..].find("]]>")? + cdata_start;
        &xml[cdata_start..cdata_end]
    } else {
        let end = xml[after_open..].find(&close)? + after_open;
        &xml[after_open..end]
    };
    Some(content.trim())
}

/// Decode the most common HTML/XML entities.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&hellip;", "…")
        .replace("&nbsp;", " ")
}

/// Parse a single RSS <item> block.
fn parse_rss_item(block: &str, feed_name: &str) -> Option<FeedItem> {
    let title = extract_tag(block, "title")
        .map(decode_entities)
        .filter(|t| !t.is_empty())?;

    let link = extract_tag(block, "link")
        .or_else(|| {
            // Atom uses <link href="..."/>
            let marker = "href=\"";
            let start = block.find(marker)? + marker.len();
            let end = block[start..].find('"')? + start;
            Some(&block[start..end])
        })
        .map(str::trim)
        .map(decode_entities)?;

    let description = extract_tag(block, "description")
        .or_else(|| extract_tag(block, "content"))
        .or_else(|| extract_tag(block, "summary"))
        .map(decode_entities)
        .unwrap_or_default();

    // Strip HTML tags from description for cleaner text
    let plain_desc = strip_html(&description);

    let published = extract_tag(block, "pubDate")
        .or_else(|| extract_tag(block, "published"))
        .or_else(|| extract_tag(block, "updated"))
        .and_then(|d| {
            DateTime::parse_from_rfc2822(d)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
                .or_else(|| {
                    DateTime::parse_from_rfc3339(d)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                })
        });

    Some(FeedItem {
        title,
        link,
        description: plain_desc.chars().take(300).collect(),
        published,
        feed_name: feed_name.to_string(),
    })
}

/// Very lightweight HTML tag stripper.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn parse_feed(xml: &str, feed_name: &str) -> Result<Vec<FeedItem>> {
    // Detect RSS vs Atom by looking for <item> vs <entry>
    let item_tag = if xml.contains("<item>") || xml.contains("<item ") {
        "item"
    } else {
        "entry"
    };
    let open = format!("<{}>", item_tag);
    let close = format!("</{}>", item_tag);

    let mut items = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find(&open) {
        let after = start + open.len();
        let end = match remaining[after..].find(&close) {
            Some(i) => i + after,
            None => break,
        };
        let block = &remaining[start..end + close.len()];
        if let Some(item) = parse_rss_item(block, feed_name) {
            items.push(item);
        }
        remaining = &remaining[end + close.len()..];
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rss_parser_basic() {
        let xml = r#"<?xml version="1.0"?>
<rss>
  <channel>
    <item>
      <title>Test Article &amp; More</title>
      <link>https://example.com/article</link>
      <description><![CDATA[This is a <b>test</b> description.]]></description>
      <pubDate>Mon, 01 Jan 2024 12:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#;
        let items = parse_feed(xml, "Test Feed").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Test Article & More");
        assert_eq!(items[0].link, "https://example.com/article");
        assert!(items[0].description.contains("test"));
        assert!(items[0].published.is_some());
    }

    #[test]
    fn test_atom_parser() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <title>Atom Article</title>
    <link href="https://example.com/atom"/>
    <summary>Atom summary here.</summary>
    <updated>2024-01-01T12:00:00Z</updated>
  </entry>
</feed>"#;
        let items = parse_feed(xml, "Atom Feed").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Atom Article");
    }

    #[test]
    fn test_entity_decoding() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt;"), "a & b <c>");
    }
}
