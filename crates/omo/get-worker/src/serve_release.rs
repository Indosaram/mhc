//! Port of `src/serve-release.ts`: mirrored release assets with a GitHub fallback.

use crate::bindings::{CacheKey, CachePopulation, ReleaseStore, StoredObject};
use crate::datapoint::{
    is_qa_install, record_download, request_country, DownloadEvent, RequestKind, ServedFrom,
};
use crate::env::RequestContext;
use crate::http::{Body, Headers, Method, Request, Response};
use crate::release_names::{
    asset_kind, completion_marker_key, github_asset_url, is_release_version, release_object_key,
    AssetKind,
};

pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

fn not_found(message: &str) -> Response {
    Response::new(404, Some(Body::Text(format!("{message}\n"))))
        .with_header("Content-Type", "text/plain; charset=utf-8")
}

fn github_redirect(version: &str, asset: &str, reason: &str) -> Response {
    Response::new(302, None)
        .with_header("Location", github_asset_url(version, asset))
        .with_header("Cache-Control", "no-store")
        .with_header("X-Omo-Source", "github")
        .with_header("X-Omo-Fallback-Reason", reason)
}

fn object_response(object: &StoredObject, asset: &str, source: &str) -> Response {
    let mut headers = Headers::new();
    object.write_http_metadata(&mut headers);
    headers.set("ETag", object.http_etag.clone());
    headers.set("Content-Length", object.size.to_string());
    headers.set("Content-Type", "application/octet-stream");
    headers.set(
        "Content-Disposition",
        format!("attachment; filename=\"{asset}\""),
    );
    headers.set("Cache-Control", IMMUTABLE);
    headers.set("X-Omo-Source", source);
    Response::new(200, Some(object.body.clone())).with_headers(&headers)
}

/// The outcome of the completion-marker-gated storage read.
enum R2Outcome {
    Served(Response),
    Fallback(&'static str),
}

fn read_from_r2(ctx: &RequestContext<'_>, version: &str, asset: &str) -> R2Outcome {
    let store: &dyn ReleaseStore = ctx.env.releases.as_ref();
    match store.head(&completion_marker_key(version)) {
        Ok(None) => R2Outcome::Fallback("version-not-mirrored"),
        Ok(Some(_)) => match store.get(&release_object_key(version, asset)) {
            Ok(None) => R2Outcome::Fallback("object-missing"),
            Ok(Some(object)) => R2Outcome::Served(object_response(&object, asset, "r2")),
            Err(_) => R2Outcome::Fallback("r2-error"),
        },
        Err(_) => R2Outcome::Fallback("r2-error"),
    }
}

/// `serveReleaseAsset`: validates the version and asset before touching storage, then
/// serves from the edge cache, storage, or a GitHub redirect.
pub fn serve_release_asset(
    request: &Request,
    ctx: &RequestContext<'_>,
    version: &str,
    asset: &str,
) -> Response {
    let kind = match asset_kind(asset) {
        Some(kind) if is_release_version(version) => kind,
        _ => return not_found("unknown release asset"),
    };
    let is_get = request.method == Method::Get;
    let country = request_country(request);
    let qa = is_qa_install(request);
    let record = |source: ServedFrom| {
        if !is_get {
            return;
        }
        record_download(
            ctx.env.downloads.as_ref(),
            &DownloadEvent {
                kind: kind_of(kind),
                source,
                version: version.to_string(),
                asset: asset.to_string(),
                country: country.clone(),
                qa,
            },
        );
    };

    let cache_key = CacheKey::get(format!("https://get.omo.dev/v/{version}/{asset}"));
    let ranged = request.has_header("Range");
    let lookup = if ranged {
        cache_key.clone().with_headers(request.headers.clone())
    } else {
        cache_key.clone()
    };
    if let Some(cached) = ctx.cache.match_request(&lookup) {
        record(ServedFrom::Cache);
        let mut headers = cached.headers.clone();
        headers.set("X-Omo-Source", "cache");
        let body = if is_get { cached.body.clone() } else { None };
        return Response::new(cached.status, body).with_headers(&headers);
    }

    match read_from_r2(ctx, version, asset) {
        R2Outcome::Fallback(reason) => {
            record(ServedFrom::Github);
            github_redirect(version, asset, reason)
        }
        R2Outcome::Served(response) => {
            record(ServedFrom::R2);
            if !is_get {
                return response.without_body();
            }
            if !ranged {
                ctx.wait_until.wait_until(CachePopulation {
                    key: cache_key,
                    response: response.clone(),
                });
            }
            response
        }
    }
}

fn kind_of(kind: AssetKind) -> RequestKind {
    match kind {
        AssetKind::Binary => RequestKind::Binary,
        AssetKind::Checksums => RequestKind::Checksums,
        AssetKind::Engine => RequestKind::Engine,
    }
}
