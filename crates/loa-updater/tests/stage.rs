use axum::body::Body;
use axum::extract::State;
use axum::http::Response;
use axum::routing::get;
use axum::Router;
use flate2::write::GzEncoder;
use flate2::Compression;
use loa_updater::stage::{StageError, Stager};
use loa_updater::{ArchiveFormat, Artifact, Checksum, Release};
use semver::Version;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::Arc;

fn archive(path: &str, content: &[u8]) -> Vec<u8> {
    let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut compressed);
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, path, content).unwrap();
        tar.finish().unwrap();
    }
    compressed.flush().unwrap();
    compressed.finish().unwrap()
}

async fn bytes(State(body): State<Arc<Vec<u8>>>) -> Response<Body> {
    Response::builder().body(Body::from((*body).clone())).unwrap()
}

async fn fixture(body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new().route("/artifact", get(bytes)).with_state(Arc::new(body));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/artifact", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, task)
}

fn release(url: String, data: &[u8], format: ArchiveFormat) -> Release {
    Release {
        product: "omp".into(), version: Version::parse("20.0.0").unwrap(),
        notes: String::new(), published_at: None,
        artifact: Some(Artifact { name: "omp.tgz".into(), url,
            size: Some(data.len() as u64), checksum: Some(Checksum::Sha256(hex::encode(Sha256::digest(data)))),
            format, signature_url: None }),
    }
}

#[tokio::test]
async fn stages_npm_package_beside_existing_version_without_touching_active() {
    let data = archive("package/bin/omp", b"new version");
    let (url, server) = fixture(data.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let current = root.path().join("versions/omp/19.0.0/payload/bin");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::write(current.join("omp"), b"old version").unwrap();
    let stager = Stager::new(loa_updater::http::client().unwrap(), root.path());
    let installed = stager.stage(&release(url, &data, ArchiveFormat::NpmTarball)).await.unwrap();
    assert_eq!(std::fs::read(installed.payload().join("bin/omp")).unwrap(), b"new version");
    assert_eq!(std::fs::read(current.join("omp")).unwrap(), b"old version");
    assert_eq!(stager.stage(&release("http://127.0.0.1:1/not-fetched".into(), &data, ArchiveFormat::NpmTarball)).await.unwrap().dir, installed.dir);
    server.abort();
}

#[tokio::test]
async fn checksum_mismatch_cannot_stage_an_update() {
    let data = archive("bin/omp", b"bad bytes");
    let (url, server) = fixture(data.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let stager = Stager::new(loa_updater::http::client().unwrap(), root.path());
    let mut release = release(url, &data, ArchiveFormat::TarGz);
    release.artifact.as_mut().unwrap().checksum = Some(Checksum::Sha256("00".repeat(32)));
    let error = stager.stage(&release).await.unwrap_err();
    assert!(matches!(error, StageError::ChecksumMismatch { .. }), "{error}");
    assert!(!root.path().join("versions/omp/20.0.0").exists());
    server.abort();
}

#[tokio::test]
async fn oversized_feed_artifact_is_rejected_without_a_download() {
    let root = tempfile::tempdir().unwrap();
    let stager = Stager::new(loa_updater::http::client().unwrap(), root.path());
    let mut release = release("http://127.0.0.1:1/not-fetched".into(), b"", ArchiveFormat::Raw);
    release.artifact.as_mut().unwrap().size = Some(u64::MAX);
    assert!(matches!(stager.stage(&release).await.unwrap_err(), StageError::TooLarge { .. }));
    assert!(!root.path().join("versions/omp/20.0.0").exists());
}

#[tokio::test]
async fn existing_version_directory_without_manifest_is_never_deleted() {
    let root = tempfile::tempdir().unwrap();
    let installed = root.path().join("versions/omp/20.0.0/payload/bin");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(installed.join("omp"), b"running version").unwrap();
    let stager = Stager::new(loa_updater::http::client().unwrap(), root.path());
    let candidate = release("http://127.0.0.1:1/not-fetched".into(), b"new", ArchiveFormat::Raw);
    assert!(matches!(stager.stage(&candidate).await.unwrap_err(), StageError::UnsafeEntry { .. }));
    assert_eq!(std::fs::read(installed.join("omp")).unwrap(), b"running version");
}
