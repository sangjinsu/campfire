use campfire_server::{
    Campfire, Config,
    dashboard::{Dashboard, DashboardOptions},
};
use serde_json::Value;
use std::net::SocketAddr;

const KEY: &str = "ADMIN_SECRET_SENTINEL_12345678901234567890";
struct Server {
    runtime: Campfire,
    dashboard: Option<Dashboard>,
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn start(enabled: bool) -> Self {
        let runtime = Campfire::new(Config::default()).unwrap();
        let dashboard = enabled.then(|| {
            Dashboard::start(
                runtime.observer(),
                DashboardOptions::from_lookup(|_| Some(KEY.into()))
                    .unwrap()
                    .unwrap(),
            )
            .unwrap()
        });
        let app = dashboard.as_ref().map_or_else(
            || runtime.router(),
            |dashboard| runtime.router().merge(dashboard.router()),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            runtime,
            dashboard,
            address,
            task,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
    async fn stop(mut self) {
        self.task.abort();
        if let Some(dashboard) = &mut self.dashboard {
            dashboard.shutdown().await;
            dashboard.shutdown().await;
        }
        self.runtime.shutdown().await;
    }
}

#[tokio::test]
async fn admin_data_requires_an_independent_bearer_key() {
    let server = Server::start(true).await;
    let client = reqwest::Client::new();
    for path in [
        "/admin/api/status",
        "/admin/api/rooms",
        "/admin/api/history",
        "/admin/api/unknown",
    ] {
        for method in [
            reqwest::Method::GET,
            reqwest::Method::HEAD,
            reqwest::Method::POST,
        ] {
            let response = client
                .request(method.clone(), server.url(path))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_security_headers(&response);
        }
    }
    for authorization in [
        "Basic abc",
        "bearer abc",
        "Bearer abc",
        "Bearer ",
        "Bearer  wrong",
        "Bearer game-session-token",
    ] {
        let response = client
            .get(server.url("/admin/api/status"))
            .header("Authorization", authorization)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert!(!response.text().await.unwrap().contains(KEY));
    }
    let duplicate = client
        .get(server.url("/admin/api/status"))
        .header("Authorization", format!("Bearer {KEY}"))
        .header("Authorization", format!("Bearer {KEY}"))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), reqwest::StatusCode::UNAUTHORIZED);
    let query = client
        .get(server.url(&format!("/admin/api/status?token={KEY}")))
        .header("Cookie", format!("token={KEY}"))
        .send()
        .await
        .unwrap();
    assert_eq!(query.status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = client
        .get(server.url("/admin/api/status"))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_security_headers(&response);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["schema_version"], 1);
    assert!(body["boot_id"].as_str().unwrap().len() >= 32);
    let response = client
        .head(server.url("/admin/api/status"))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_security_headers(&response);
    assert!(response.bytes().await.unwrap().is_empty());
    for method in [
        reqwest::Method::POST,
        reqwest::Method::PUT,
        reqwest::Method::DELETE,
        reqwest::Method::PATCH,
    ] {
        let response = client
            .request(method, server.url("/admin/api/status"))
            .bearer_auth(KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
        assert_security_headers(&response);
        let body = response.text().await.unwrap();
        assert!(body.contains("schema_version"));
        assert!(!body.contains(KEY));
    }
    server.stop().await;
}

fn assert_security_headers(response: &reqwest::Response) {
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(
        !response
            .headers()
            .contains_key("access-control-allow-origin")
    );
}

#[test]
fn environment_requires_untrimmed_graphic_secret_and_never_echoes_it() {
    for value in [None, Some("")] {
        assert!(
            DashboardOptions::from_lookup(|_| value.map(str::to_owned))
                .unwrap()
                .is_none()
        );
    }
    for value in [
        "x".repeat(31),
        "x".repeat(257),
        format!(" {KEY}"),
        format!("{KEY}\n"),
        "한".repeat(32),
    ] {
        let error = DashboardOptions::from_lookup(|_| Some(value.clone()))
            .err()
            .unwrap();
        assert!(!error.to_string().contains(&value));
    }
    for length in [32, 256] {
        assert!(
            DashboardOptions::from_lookup(|_| Some("x".repeat(length)))
                .unwrap()
                .is_some()
        );
    }
}

#[cfg(unix)]
#[test]
fn invalid_unicode_environment_is_rejected_without_printing_its_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = b"RAW_ENV_SECRET_SENTINEL_1234567890".to_vec();
    bytes.push(0xff);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_campfire-server"))
        .env("CAMPFIRE_ADMIN_TOKEN", std::ffi::OsString::from_vec(bytes))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("RAW_ENV_SECRET_SENTINEL"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("RAW_ENV_SECRET_SENTINEL"));
}

#[tokio::test]
async fn disabled_routes_are_missing_and_enabled_shell_contains_no_secret() {
    let client = reqwest::Client::new();
    let server = Server::start(false).await;
    for path in [
        "/admin",
        "/admin/",
        "/admin/app.js",
        "/admin/style.css",
        "/admin/api/status",
    ] {
        assert_eq!(
            client.get(server.url(path)).send().await.unwrap().status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }
    server.stop().await;
    let server = Server::start(true).await;
    let missing = client
        .get(server.url("/admin/missing"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    assert_security_headers(&missing);
    for path in ["/admin", "/admin/", "/admin/app.js", "/admin/style.css"] {
        let response = client.get(server.url(path)).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_security_headers(&response);
        let body = response.text().await.unwrap();
        assert!(!body.contains(KEY));
        assert!(!body.contains("initial_secret"));
    }
    for path in [
        "/admin/api/rooms?limit=101",
        "/admin/api/rooms?visibility=invalid",
        "/admin/api/rooms?cursor=garbage",
        "/admin/api/history?limit=901",
        "/admin/api/history?after_seq=nope",
    ] {
        let response = client
            .get(server.url(path))
            .bearer_auth(KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{path}"
        );
        assert_security_headers(&response);
        assert!(!response.text().await.unwrap().contains(KEY));
    }
    server.stop().await;
}

#[tokio::test]
async fn real_process_sampler_and_incremental_history_progress_independently() {
    let server = Server::start(true).await;
    let client = reqwest::Client::new();
    tokio::time::sleep(std::time::Duration::from_millis(2250)).await;
    let status: Value = client
        .get(server.url("/admin/api/status"))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["state"], "ready");
    assert!(status["sample_seq"].as_u64().unwrap() >= 2);
    assert_eq!(status["data"]["process"]["pid"], std::process::id());
    assert!(status["data"]["process"]["rss_bytes"].as_u64().unwrap() > 0);
    assert!(status["data"]["process"]["cpu_percent"].as_f64().unwrap() >= 0.0);
    let history: Value = client
        .get(server.url("/admin/api/history"))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history["data"]["reset"], true);
    let samples = history["data"]["samples"].as_array().unwrap();
    assert!(samples.len() >= 2);
    assert_eq!(samples[0]["rates"]["ticks_per_second"], Value::Null);
    let last = samples.last().unwrap()["sample_seq"].as_u64().unwrap();
    let path = format!(
        "/admin/api/history?after_seq={last}&boot_id={}",
        history["boot_id"].as_str().unwrap()
    );
    let increment: Value = client
        .get(server.url(&path))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(increment["data"]["reset"], false);
    assert!(
        increment["data"]["samples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|sample| sample["sample_seq"].as_u64().unwrap() > last)
    );
    server.stop().await;
}
