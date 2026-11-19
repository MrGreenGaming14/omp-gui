use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::get;
use axum::Router;
use loa_updater::feeds::{GitHubFeed, NpmFeed, Watcher};
use loa_updater::{Feed, PrereleasePolicy, Product, ProductKind};
use semver::Version;
use std::sync::{Arc, Mutex};

type RequestLog = Arc<Mutex<Vec<(String, Option<String>)>>>;

#[derive(Clone, Default)]
struct Requests(RequestLog);

async fn github(State(seen): State<Requests>, headers: HeaderMap) -> Response<Body> {
    let etag = headers.get("if-none-match").and_then(|v| v.to_str().ok()).map(str::to_owned);
    seen.0.lock().unwrap().push(("github".into(), etag.clone()));
    if etag.as_deref() == Some("\"gh1\"") {
        return Response::builder().status(StatusCode::NOT_MODIFIED).body(Body::empty()).unwrap();
    }
    let body = serde_json::json!([
        {"tag_name":"v1.2.0", "body":"fixes", "assets":[{"name":"app-1.2.0.tar.gz", "browser_download_url":"http://127.0.0.1/artifact", "size":4, "digest":format!("sha256:{}", "a".repeat(64))}]},
        {"tag_name":"v1.3.0-beta.1", "prerelease":true},
        {"tag_name":"v9.0.0", "draft":true}
    ]);
    Response::builder().header("etag", "\"gh1\"").header("x-ratelimit-remaining", "2")
        .header("x-ratelimit-reset", "4000000000").body(Body::from(body.to_string())).unwrap()
}

async fn npm(Path(_package): Path<String>, State(seen): State<Requests>, headers: HeaderMap) -> Response<Body> {
    let etag = headers.get("if-none-match").and_then(|v| v.to_str().ok()).map(str::to_owned);
    seen.0.lock().unwrap().push(("npm".into(), etag.clone()));
    if etag.as_deref() == Some("\"npm1\"") {
        return Response::builder().status(StatusCode::NOT_MODIFIED).body(Body::empty()).unwrap();
    }
    let body = serde_json::json!({"dist-tags":{"latest":"2.1.0","next":"3.0.0-beta.1"},"versions":{
        "2.1.0":{"dist":{"tarball":"http://127.0.0.1/pkg.tgz","integrity":format!("sha512-{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [3u8;64]))}},
        "3.0.0-beta.1":{"dist":{"tarball":"http://127.0.0.1/next.tgz"}},
        "2.2.0":{"dist":{"tarball":"http://127.0.0.1/future.tgz"}}
    }});
    Response::builder().header("etag", "\"npm1\"").body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn watches_github_and_npm_with_conditional_requests_and_pins() {
    let seen = Requests::default();
    let server = Router::new().route("/repos/owner/app/releases", get(github))
        .route("/{package}", get(npm)).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    let client = loa_updater::http::client().unwrap();
    let mut watcher = Watcher::new(GitHubFeed::new(client.clone(), &base), NpmFeed::new(client, &base), None, Default::default());
    let app = Product { id: "app".into(), kind: ProductKind::App, feed: Feed::GitHub { owner: "owner".into(), repo: "app".into() }, prereleases: PrereleasePolicy::Stable, asset: Some("app-{version}.tar.gz".into()) };
    let package = Product { id: "omp".into(), kind: ProductKind::Omp, feed: Feed::Npm { package: "omp".into() }, prereleases: PrereleasePolicy::Stable, asset: None };
    let current = Version::parse("1.0.0").unwrap();
    let release = watcher.check(&app, &current, None).await.unwrap().unwrap();
    assert_eq!(release.version, Version::parse("1.2.0").unwrap());
    assert!(release.artifact.unwrap().checksum.is_some());
    assert_eq!(watcher.github_rate_limit.unwrap().remaining, 2);
    assert_eq!(watcher.check(&app, &current, None).await.unwrap().unwrap().version, release.version);
    assert_eq!(watcher.check(&package, &current, None).await.unwrap().unwrap().version, Version::parse("2.1.0").unwrap());
    let pin = Version::parse("2.1.0").unwrap();
    assert_eq!(watcher.check(&package, &pin, Some(&pin)).await.unwrap(), None);
    assert_eq!(&*seen.0.lock().unwrap(), &[("github".into(), None), ("github".into(), Some("\"gh1\"".into())), ("npm".into(), None), ("npm".into(), Some("\"npm1\"".into()))]);
    handle.abort();
}
