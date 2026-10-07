//! Port of `src/channels.ts`: channel resolution from the mirrored pointer, with the
//! npm dist-tag as the fallback before the first mirror.

use crate::bindings::{CacheKey, CachePopulation, FetchError, FetchRequest, HttpFetcher};
use crate::datapoint::{
    is_qa_install, record_download, request_country, DownloadEvent, RequestKind, ServedFrom,
};
use crate::env::RequestContext;
use crate::http::{Body, Method, Request, Response};
use crate::release_names::{
    channel_pointer_key, is_release_version, release_version_of_npm_version, Channel,
};

pub const POINTER_TTL_SECONDS: u64 = 60;
pub const NPM_DIST_TAGS: &str = "https://registry.npmjs.org/-/package/omo-ai/dist-tags";

/// A channel resolved to a concrete release version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub version: String,
    pub source: ServedFrom,
}

fn from_r2(ctx: &RequestContext<'_>, channel: Channel) -> Option<String> {
    let object = ctx
        .env
        .releases
        .get(&channel_pointer_key(channel))
        .ok()
        .flatten()?;
    let version = object.text().trim().to_string();
    if is_release_version(&version) {
        Some(version)
    } else {
        None
    }
}

/// The R2 pointer only moves after a verified mirror; before the first mirror it is
/// absent, so the channel falls back to the npm dist-tag, which carries the same
/// latest/beta meaning.
fn from_npm(fetcher: &dyn HttpFetcher, channel: Channel) -> Result<Option<String>, FetchError> {
    let response = fetcher.fetch(
        FetchRequest::get(NPM_DIST_TAGS).with_header("Accept", "application/json"),
    )?;
    if !response.ok() {
        return Ok(None);
    }
    let tags = response.json().map_err(|error| FetchError {
        message: error.message,
    })?;
    let Some(npm_version) = tags.get(channel).and_then(|value| value.as_str()) else {
        return Ok(None);
    };
    let version = release_version_of_npm_version(npm_version);
    Ok(is_release_version(&version).then_some(version))
}

fn resolve_channel(
    ctx: &RequestContext<'_>,
    channel: Channel,
) -> Result<Option<Resolved>, FetchError> {
    if let Some(mirrored) = from_r2(ctx, channel) {
        return Ok(Some(Resolved {
            version: mirrored,
            source: ServedFrom::R2,
        }));
    }
    Ok(from_npm(ctx.fetch, channel)?.map(|version| Resolved {
        version,
        source: ServedFrom::Npm,
    }))
}

/// `serveChannel`: an edge-cache hit short-circuits; a miss resolves the pointer and
/// queues the cache population; a GET of an ok response is counted.
pub fn serve_channel(
    request: &Request,
    ctx: &RequestContext<'_>,
    channel: Channel,
) -> Result<Response, FetchError> {
    let cache_key = CacheKey::get(format!("https://get.omo.dev/channels/{channel}"));
    let response = match ctx.cache.match_request(&cache_key) {
        Some(cached) => cached,
        None => {
            let Some(resolved) = resolve_channel(ctx, channel)? else {
                return Ok(
                    Response::new(503, Some(Body::Text("channel unavailable\n".to_string())))
                        .with_header("Cache-Control", "no-store"),
                );
            };
            let fresh = Response::new(200, Some(Body::Text(format!("{}\n", resolved.version))))
                .with_header("Content-Type", "text/plain; charset=utf-8")
                .with_header(
                    "Cache-Control",
                    format!("public, max-age={POINTER_TTL_SECONDS}"),
                )
                .with_header("X-Omo-Source", resolved.source.as_str());
            ctx.wait_until(CachePopulation {
                key: cache_key,
                response: fresh.clone(),
            });
            fresh
        }
    };
    if request.method == Method::Get && response.ok() {
        record_download(
            ctx.env.downloads.as_ref(),
            &DownloadEvent {
                kind: RequestKind::Channel,
                source: ServedFrom::Worker,
                version: response.body_text().trim().to_string(),
                asset: channel.to_string(),
                country: request_country(request),
                qa: is_qa_install(request),
            },
        );
    }
    Ok(response)
}
