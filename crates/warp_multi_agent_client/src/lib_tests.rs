use std::sync::Arc;

use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE;
use futures::StreamExt as _;
use futures::executor::block_on;
use prost::Message as _;
use warp_server_auth::auth_state::AuthState;
use warp_server_auth::credentials::Credentials;
use warp_server_client::base_client::{
    AmbientHeaderPolicy, AuthenticatedGraphqlConfig, BaseClient, GraphqlRoutingConfig,
};

use super::{
    Error, ambient_policy, decode_response_event, hosted_endpoint_url,
    is_passive_suggestion_request,
};

#[test]
fn detects_passive_suggestion_requests() {
    let regular = warp_multi_agent_api::Request::default();
    let passive = warp_multi_agent_api::Request {
        input: Some(warp_multi_agent_api::request::Input {
            r#type: Some(
                warp_multi_agent_api::request::input::Type::GeneratePassiveSuggestions(
                    Default::default(),
                ),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };

    assert!(!is_passive_suggestion_request(&regular));
    assert!(is_passive_suggestion_request(&passive));
}

#[test]
fn routes_regular_and_passive_requests_to_distinct_endpoints() {
    let prefix = if cfg!(feature = "agent_mode_evals") {
        "agent-mode-evals"
    } else {
        "ai"
    };

    assert!(hosted_endpoint_url(false).ends_with(&format!("/{prefix}/multi-agent")));
    assert!(hosted_endpoint_url(true).ends_with(&format!("/{prefix}/passive-suggestions")));
}

#[test]
fn selects_endpoint_specific_ambient_header_policies() {
    assert_eq!(ambient_policy(false), AmbientHeaderPolicy::workload_only());
    assert_eq!(ambient_policy(true), AmbientHeaderPolicy::omit_all());
}

#[test]
fn decodes_quoted_base64_protobuf_response_event() {
    let expected = warp_multi_agent_api::ResponseEvent::default();
    let encoded = BASE64_URL_SAFE.encode(expected.encode_to_vec());

    let decoded = decode_response_event(&format!("\"{encoded}\"")).unwrap();

    assert_eq!(decoded, expected);
}

#[test]
fn distinguishes_base64_and_protobuf_decode_errors() {
    assert!(matches!(
        decode_response_event("%"),
        Err(Error::Base64Decode(_))
    ));

    let invalid_protobuf = BASE64_URL_SAFE.encode([0xff]);
    assert!(matches!(
        decode_response_event(&invalid_protobuf),
        Err(Error::ProtobufDecode(_))
    ));
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn native_output_stream_is_send() {
    fn assert_send<T: Send>() {}

    assert_send::<super::OutputStream>();
}

struct LocalIapTokenProvider;

impl http_client::iap::IapTokenProvider for LocalIapTokenProvider {
    fn cached_token(&self) -> Option<String> {
        Some("must-not-be-sent".to_owned())
    }
}

fn local_client(root: &str) -> BaseClient {
    let state = Arc::new(AuthState::new_logged_out_for_test());
    state.set_credentials(Some(Credentials::LocalAgent));
    let (sender, _) = async_channel::unbounded();
    let mut http_client = http_client::Client::new_for_test();
    http_client.set_iap_token_provider(Arc::new(LocalIapTokenProvider));
    let client = BaseClient::new(
        Arc::new(http_client),
        state,
        sender,
        Some("must-not-be-sent".to_owned()),
        GraphqlRoutingConfig::default(),
        AuthenticatedGraphqlConfig::default(),
        Some(Arc::new(LocalIapTokenProvider)),
    )
    .with_local_agent_url(Some(url::Url::parse(root).unwrap()));
    client.set_ambient_agent_task_id(Some("must-not-be-sent".to_owned()));
    client
}

#[test]
fn local_inference_sends_protobuf_without_warp_identity_headers() {
    let mut server = mockito::Server::new();
    let expected = warp_multi_agent_api::ResponseEvent {
        r#type: Some(warp_multi_agent_api::response_event::Type::Init(
            Default::default(),
        )),
    };
    let encoded = BASE64_URL_SAFE.encode(expected.encode_to_vec());
    let request = warp_multi_agent_api::Request::default();
    let response = server
        .mock("POST", "/ai/multi-agent")
        .match_header("content-type", "application/x-protobuf")
        .match_header("authorization", mockito::Matcher::Missing)
        .match_header("x-warp-client-version", mockito::Matcher::Missing)
        .match_header("x-warp-client-id", mockito::Matcher::Missing)
        .match_header("x-warp-team-uid", mockito::Matcher::Missing)
        .match_header("x-warp-cloud-agent-id", mockito::Matcher::Missing)
        .match_header("x-oz-api-source", mockito::Matcher::Missing)
        .match_header("x-warp-ambient-workload-token", mockito::Matcher::Missing)
        .match_header(
            http_client::iap::IAP_PROXY_AUTH_HEADER,
            mockito::Matcher::Missing,
        )
        .match_body(request.encode_to_vec())
        .with_header("content-type", "text/event-stream")
        .with_body(format!("data: {encoded}\n\n"))
        .create();
    let client = local_client(&server.url());

    let actual = block_on(async {
        let mut stream =
            super::generate_multi_agent_output(&client, &request, Some("team".to_owned()))
                .await
                .unwrap();
        stream.next().await.unwrap().unwrap()
    });

    assert_eq!(actual, expected);
    response.assert();
    assert!(block_on(client.get_or_refresh_access_token()).is_err());
}

#[test]
fn local_passive_suggestions_fail_before_network_or_authentication() {
    let client = local_client("http://127.0.0.1:1");
    let request = warp_multi_agent_api::Request {
        input: Some(warp_multi_agent_api::request::Input {
            r#type: Some(
                warp_multi_agent_api::request::input::Type::GeneratePassiveSuggestions(
                    Default::default(),
                ),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };

    assert!(matches!(
        block_on(super::generate_multi_agent_output(&client, &request, None)),
        Err(Error::UnsupportedLocalRequest)
    ));
}
