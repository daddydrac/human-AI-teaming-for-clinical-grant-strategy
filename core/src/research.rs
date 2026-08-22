use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{net::IpAddr, time::Duration};
use time::OffsetDateTime;

#[derive(Clone)]
pub struct ResearchClient {
    http: Client,
    ingestion_http: Client,
    brave_key: Option<String>,
    brave_endpoint: String,
    ingestion_url: String,
    max_concurrency: usize,
    max_body_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedSource {
    pub title: String,
    pub url: String,
    pub text: String,
    pub retrieved_at: String,
    pub sha256: String,
    pub status: u16,
}

impl ResearchClient {
    pub fn provider_name(&self) -> &'static str {
        if self.brave_key.is_some() {
            "brave_search_with_rendered_ingestion"
        } else {
            "unconfigured"
        }
    }

    pub fn search_available(&self) -> bool {
        self.brave_key.is_some()
    }

    pub async fn ingestion_health(&self) -> Result<serde_json::Value> {
        let mut url = Url::parse(&self.ingestion_url).context("invalid DOCUMENT_INGESTION_URL")?;
        url.set_path("/health");
        url.set_query(None);
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub fn from_env() -> Result<Self> {
        let timeout = std::env::var("RESEARCH_HTTP_TIMEOUT_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let max_concurrency = std::env::var("RESEARCH_MAX_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8usize)
            .clamp(1, 64);
        let user_agent = std::env::var("RESEARCH_USER_AGENT")
            .unwrap_or_else(|_| format!("GrantWriter/{}", env!("CARGO_PKG_VERSION")));
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout))
            .user_agent(&user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let ingestion_timeout = std::env::var("INGESTION_HTTP_TIMEOUT_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90u64)
            .clamp(10, 300);
        let ingestion_http = Client::builder()
            .timeout(Duration::from_secs(ingestion_timeout))
            .user_agent(user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            ingestion_http,
            brave_key: std::env::var("BRAVE_SEARCH_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            brave_endpoint: std::env::var("BRAVE_SEARCH_ENDPOINT")
                .unwrap_or_else(|_| "https://api.search.brave.com/res/v1/web/search".into()),
            ingestion_url: std::env::var("DOCUMENT_INGESTION_URL")
                .unwrap_or_else(|_| "http://ingestion:8091/extract".into()),
            max_concurrency,
            max_body_bytes: std::env::var("RESEARCH_MAX_BODY_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8 * 1024 * 1024usize)
                .clamp(64 * 1024, 64 * 1024 * 1024),
        })
    }

    pub async fn search(
        &self,
        query: &str,
        domains: &[String],
        count: usize,
    ) -> Result<Vec<SearchHit>> {
        let key = self
            .brave_key
            .as_ref()
            .context("BRAVE_SEARCH_API_KEY is required for online research")?;
        let effective_query = scoped_search_query(query, domains);
        let response = self
            .http
            .get(&self.brave_endpoint)
            .header("X-Subscription-Token", key)
            .query(&[
                ("q", effective_query),
                ("count", count.clamp(1, 20).to_string()),
                ("safesearch", "moderate".to_owned()),
            ])
            .send()
            .await?
            .error_for_status()?;
        let value: serde_json::Value = response.json().await?;
        let hits = value
            .pointer("/web/results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(hits
            .into_iter()
            .filter_map(|v| {
                let url = v.get("url")?.as_str()?.to_owned();
                let title = v
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or(&url)
                    .to_owned();
                let snippet = v
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_owned();
                Some(SearchHit {
                    title,
                    url,
                    snippet,
                    source: "brave".into(),
                })
            })
            .collect())
    }

    pub async fn fetch_many(&self, hits: Vec<SearchHit>) -> Vec<Result<FetchedSource>> {
        stream::iter(hits.into_iter().map(|hit| {
            let this = self.clone();
            async move { this.fetch(&hit.url, Some(&hit.title)).await }
        }))
        .buffer_unordered(self.max_concurrency)
        .collect()
        .await
    }

    async fn validate_public_destination(&self, url: &Url) -> Result<()> {
        let host = url.host_str().context("URL has no host")?;
        let lower = host.to_ascii_lowercase();
        if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
            bail!("local/private research destination rejected");
        }
        let port = url
            .port_or_known_default()
            .context("URL has no usable port")?;
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .context("DNS resolution failed")?;
        let mut found = false;
        for addr in addrs {
            found = true;
            if !is_public_ip(addr.ip()) {
                bail!("private/link-local/loopback research destination rejected");
            }
        }
        if !found {
            bail!("research destination resolved to no addresses");
        }
        Ok(())
    }

    pub async fn fetch(&self, url: &str, title_hint: Option<&str>) -> Result<FetchedSource> {
        let parsed = Url::parse(url).context("invalid source URL")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("unsupported source URL scheme");
        }
        self.validate_public_destination(&parsed).await?;
        let res = self.http.get(parsed).send().await?;
        let status = res.status();
        let ctype = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if !status.is_success() {
            bail!("source returned HTTP {}", status);
        }
        if !ctype.contains("text/")
            && !ctype.contains("application/xhtml")
            && !ctype.contains("application/json")
        {
            bail!("source content type is not text-readable: {ctype}");
        }
        if let Some(len) = res.content_length() {
            if len as usize > self.max_body_bytes {
                bail!(
                    "source body exceeds configured limit of {} bytes",
                    self.max_body_bytes
                );
            }
        }
        let mut stream = res.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len().saturating_add(chunk.len()) > self.max_body_bytes {
                bail!(
                    "source body exceeds configured limit of {} bytes",
                    self.max_body_bytes
                );
            }
            body.extend_from_slice(&chunk);
        }
        let text = html2text::from_read(body.as_slice(), 120)?;
        let normalized = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let mut h = Sha256::new();
        h.update(normalized.as_bytes());
        Ok(FetchedSource {
            title: title_hint.unwrap_or(url).to_owned(),
            url: url.to_owned(),
            text: normalized,
            retrieved_at: OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)?,
            sha256: hex::encode(h.finalize()),
            status: status.as_u16(),
        })
    }

    /// Render a funding-opportunity URL in Chromium and persist the resulting
    /// Markdown as the authoritative extracted source buffer. Exact compliance
    /// excerpts are located later in Rust and are never supplied by this service.
    pub async fn fetch_rendered(
        &self,
        url: &str,
        title_hint: Option<&str>,
    ) -> Result<FetchedSource> {
        let parsed = Url::parse(url).context("invalid source URL")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("unsupported source URL scheme");
        }
        self.validate_public_destination(&parsed).await?;
        let response = self
            .ingestion_http
            .post(&self.ingestion_url)
            .json(&serde_json::json!({"url":url}))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            bail!("browser ingestion returned HTTP {status}: {detail}");
        }
        #[derive(Deserialize)]
        struct Rendered {
            title: String,
            url: String,
            text: String,
            status: u16,
        }
        let rendered: Rendered = response
            .json()
            .await
            .context("browser ingestion returned invalid JSON")?;
        if rendered.text.trim().is_empty() {
            bail!("browser ingestion returned no readable Markdown");
        }
        if rendered.text.as_bytes().len() > self.max_body_bytes {
            bail!(
                "rendered Markdown exceeds configured limit of {} bytes",
                self.max_body_bytes
            );
        }
        let mut h = Sha256::new();
        h.update(rendered.text.as_bytes());
        Ok(FetchedSource {
            title: title_hint
                .filter(|x| !x.trim().is_empty())
                .unwrap_or(&rendered.title)
                .to_owned(),
            url: rendered.url,
            text: rendered.text,
            retrieved_at: OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)?,
            sha256: hex::encode(h.finalize()),
            status: rendered.status,
        })
    }
}

fn scoped_search_query(query: &str, domains: &[String]) -> String {
    let clean = domains
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .collect::<Vec<_>>();
    if clean.is_empty() {
        return query.trim().to_string();
    }
    let scope = clean
        .into_iter()
        .map(|d| format!("site:{d}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("({}) ({scope})", query.trim())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            // Reject non-routable, private, loopback, link-local, documentation,
            // carrier-grade NAT, benchmark and multicast/reserved ranges.
            !(o[0] == 0
                || o[0] == 10
                || o[0] == 127
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 169 && o[1] == 254)
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113)
                || o[0] >= 224)
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_unique_local()
                || v.is_unicast_link_local()
                || v.is_multicast()
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scoped_search_query;
    #[test]
    fn domain_scoping_uses_or_not_impossible_and() {
        let q = scoped_search_query(
            "ctDNA patent",
            &["patents.google.com".into(), "data.uspto.gov".into()],
        );
        assert!(q.contains("site:patents.google.com OR site:data.uspto.gov"));
        assert!(!q.contains("site:patents.google.com site:data.uspto.gov"));
    }
}
