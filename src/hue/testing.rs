//! A fake Hue bridge for tests, answering like the real one over plain HTTP.

use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

/// The only application key the fake bridge accepts.
pub const VALID_KEY: &str = "valid-test-key";

const CONFIG_BODY: &str = r#"{"name":"Hue Bridge","datastoreversion":"198","swversion":"1978545030","apiversion":"1.78.0","mac":"ec:b5:fa:13:af:9c","bridgeid":"ECB5FAFFFE13AF9C","factorynew":false,"replacesbridgeid":null,"modelid":"BSB002","starterkitid":""}"#;

pub async fn fake_bridge() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/0/config"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CONFIG_BODY, "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/clip/v2/resource/bridge"))
        .and(header("hue-application-key", VALID_KEY))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"errors":[],"data":[]}"#, "application/json"),
        )
        .mount(&server)
        .await;
    // Like the real bridge: any other key gets a 403 with an HTML page.
    Mock::given(method("GET"))
        .and(path("/clip/v2/resource/bridge"))
        .respond_with(ResponseTemplate::new(403).set_body_string("<html>Oops</html>"))
        .mount(&server)
        .await;
    server
}

/// A local address with nothing listening behind it.
pub fn closed_port_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}
