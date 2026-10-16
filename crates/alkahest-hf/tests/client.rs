//! Hub client behaviour against recorded-shape fixtures served by wiremock. Real Hub calls only
//! run with `ALKAHEST_TEST_NET=1`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_hf::{
    HfClient, HfError, SearchQuery, Token, gguf_variants, mmproj_files, pick_mmproj, pick_variant,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SEARCH: &str = include_str!("fixtures/search.json");
const INFO: &str = include_str!("fixtures/model_info.json");
const PAGE1: &str = include_str!("fixtures/tree_page1.json");
const PAGE2: &str = include_str!("fixtures/tree_page2.json");
const README: &str = include_str!("fixtures/README.md");
const REPO: &str = "bartowski/Llama-3.2-3B-Instruct-GGUF";

fn json(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json")
}

fn client(server: &MockServer, token: Option<&str>) -> HfClient {
    HfClient::new(&server.uri(), token.and_then(Token::new)).unwrap()
}

#[tokio::test]
async fn search_sends_gguf_filter_and_download_sort() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .and(query_param("search", "llama 3.2"))
        .and(query_param("filter", "gguf"))
        .and(query_param("sort", "downloads"))
        .and(query_param("limit", "20"))
        .respond_with(json(SEARCH))
        .expect(1)
        .mount(&server)
        .await;
    let rows = client(&server, None)
        .search_models(&SearchQuery::gguf("llama 3.2"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, REPO);
    assert_eq!(rows[0].downloads, 250_000);
    assert!(!rows[0].gated.is_gated());
    assert!(rows[1].gated.is_gated());
}

#[tokio::test]
async fn model_info_requests_blobs_and_exposes_lfs_digests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}")))
        .and(query_param("blobs", "true"))
        .respond_with(json(INFO))
        .mount(&server)
        .await;
    let info = client(&server, None).model_info(REPO, None).await.unwrap();
    assert_eq!(
        info.sha.as_deref(),
        Some("5ab33fa94d1d04e903623ae72c95d1696f09f9e8")
    );
    assert_eq!(
        info.gguf.as_ref().unwrap().architecture.as_deref(),
        Some("llama")
    );
    let q4 = info
        .siblings
        .iter()
        .find(|s| s.rfilename.ends_with("Q4_K_M.gguf"))
        .unwrap();
    assert_eq!(q4.lfs.as_ref().unwrap().size, 2_019_377_696);
}

#[tokio::test]
async fn revision_goes_into_the_path() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/api/models/{REPO}/revision/refs%2Fpr%2F1")))
        .respond_with(json(INFO))
        .expect(1)
        .mount(&server)
        .await;
    client(&server, None)
        .model_info(REPO, Some("refs/pr/1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn files_are_chosen_by_quant_and_mmproj_is_detected() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/api/models/{REPO}")))
        .respond_with(json(INFO))
        .mount(&server)
        .await;
    let files = client(&server, None).repo_files(REPO, None).await.unwrap();
    let variants = gguf_variants(&files);
    assert_eq!(variants.len(), 2, "mmproj must not count as a model");
    let chosen = pick_variant(&variants, None).unwrap();
    assert_eq!(chosen.quant.as_deref(), Some("Q4_K_M"));
    assert_eq!(chosen.primary().sha256.as_deref().map(str::len), Some(64));
    assert_eq!(chosen.size(), Some(2_019_377_696));
    let mm = mmproj_files(&files);
    assert_eq!(pick_mmproj(&mm).unwrap().path, "mmproj-Llama-F16.gguf");
}

#[tokio::test]
async fn tree_follows_link_pagination_and_reads_lfs_oid() {
    let server = MockServer::start().await;
    let next = format!(
        "<{}/api/models/{REPO}/tree/main?recursive=true&cursor=2>; rel=\"next\"",
        server.uri()
    );
    Mock::given(path(format!("/api/models/{REPO}/tree/main")))
        .and(query_param("cursor", "2"))
        .respond_with(json(PAGE2))
        .mount(&server)
        .await;
    Mock::given(path(format!("/api/models/{REPO}/tree/main")))
        .respond_with(json(PAGE1).insert_header("Link", next.as_str()))
        .mount(&server)
        .await;
    let tree = client(&server, None).tree(REPO, None).await.unwrap();
    assert_eq!(tree.len(), 4);
    let lfs = tree[1].lfs.as_ref().unwrap();
    assert_eq!(lfs.sha256.len(), 64);
    assert_eq!(lfs.size, 2_019_377_696);
}

#[tokio::test]
async fn readme_is_parsed_into_front_matter_and_body() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/{REPO}/resolve/main/README.md")))
        .respond_with(ResponseTemplate::new(200).set_body_string(README))
        .mount(&server)
        .await;
    let card = client(&server, None).model_card(REPO, None).await.unwrap();
    assert_eq!(card.license().as_deref(), Some("llama3.2"));
    assert_eq!(card.tags(), ["gguf", "llama"]);
    assert!(card.body.starts_with("# Llama 3.2"));
}

#[tokio::test]
async fn no_token_means_anonymous_requests() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/api/models/{REPO}")))
        .respond_with(json(INFO))
        .mount(&server)
        .await;
    client(&server, None).model_info(REPO, None).await.unwrap();
    let received = server.received_requests().await.unwrap();
    assert!(received[0].headers.get("authorization").is_none());
}

#[tokio::test]
async fn token_is_sent_as_bearer_to_the_endpoint_only() {
    let server = MockServer::start().await;
    Mock::given(path(format!("/api/models/{REPO}")))
        .and(header("authorization", "Bearer hf_secret"))
        .respond_with(json(INFO))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server, Some("hf_secret"));
    c.model_info(REPO, None).await.unwrap();
    assert!(!format!("{c:?}").contains("hf_secret"));
    assert_eq!(
        c.bearer_for(&format!("{}/x/resolve/main/f.gguf", server.uri())),
        Some("hf_secret")
    );
    assert_eq!(c.bearer_for("https://cdn.example.org/blob"), None);
}

#[tokio::test]
async fn gated_and_unauthorized_map_to_typed_errors_with_a_token_hint() {
    let server = MockServer::start().await;
    Mock::given(path("/api/models/org/private"))
        .respond_with(ResponseTemplate::new(401).insert_header("x-error-code", "RepoNotFound"))
        .mount(&server)
        .await;
    Mock::given(path("/api/models/org/gated"))
        .respond_with(ResponseTemplate::new(403).insert_header("x-error-code", "GatedRepo"))
        .mount(&server)
        .await;
    let c = client(&server, None);
    let e = c.model_info("org/private", None).await.unwrap_err();
    assert!(matches!(e, HfError::Unauthorized { .. }), "{e:?}");
    assert!(e.to_string().contains("HF_TOKEN"));
    let e = c.model_info("org/gated", None).await.unwrap_err();
    assert!(matches!(e, HfError::Forbidden { .. }), "{e:?}");
    assert!(e.to_string().contains("HF_TOKEN"));
}

#[tokio::test]
async fn not_found_and_rate_limit_are_typed() {
    let server = MockServer::start().await;
    Mock::given(path("/api/models/org/none"))
        .respond_with(ResponseTemplate::new(404).insert_header("x-error-code", "RepoNotFound"))
        .mount(&server)
        .await;
    Mock::given(path("/api/models/org/busy"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
        .mount(&server)
        .await;
    let c = client(&server, None);
    match c.model_info("org/none", None).await.unwrap_err() {
        HfError::NotFound(m) => assert!(m.contains("RepoNotFound")),
        e => panic!("{e:?}"),
    }
    assert!(matches!(
        c.model_info("org/busy", None).await.unwrap_err(),
        HfError::RateLimited {
            retry_after_secs: Some(7)
        }
    ));
}

#[tokio::test]
async fn resolve_url_encodes_segments() {
    let server = MockServer::start().await;
    let c = client(&server, None);
    let u = c
        .resolve_url("org/model", None, "dir/My Model Q4.gguf")
        .unwrap();
    assert_eq!(
        u,
        format!(
            "{}/org/model/resolve/main/dir/My%20Model%20Q4.gguf",
            server.uri()
        )
    );
    let u = c
        .resolve_url("org/model", Some("refs/pr/2"), "a.gguf")
        .unwrap();
    assert!(u.contains("/resolve/refs%2Fpr%2F2/a.gguf"));
}

#[test]
fn invalid_endpoint_is_rejected() {
    assert!(matches!(
        HfClient::new("not a url", None),
        Err(HfError::InvalidUrl(_))
    ));
}

/// Real Hub, opt-in: `ALKAHEST_TEST_NET=1 cargo test -p alkahest-hf -- --ignored`.
#[tokio::test]
#[ignore = "needs network: set ALKAHEST_TEST_NET=1 and run with --ignored"]
async fn real_hub_search_and_info() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let c = HfClient::new("https://huggingface.co", None).unwrap();
    let rows = c
        .search_models(&SearchQuery::gguf("tinyllama"))
        .await
        .unwrap();
    assert!(!rows.is_empty());
    let info = c.model_info(&rows[0].id, None).await.unwrap();
    assert!(!info.siblings.is_empty());
}
