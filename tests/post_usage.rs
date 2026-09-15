use memseek_test::ctxlibs::http_client::{CaptureOptions, Client, HttpError, check_status};
use memseek_test::error::ScenarioError;
use reqwest::Method;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// 极简假服务器:读完请求头(到空行)后,返回固定 body 的 400 响应。
async fn fake_server_400(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = sock.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = writer.write_all(resp.as_bytes()).await;
            let _ = writer.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// 返回 500 但响应体不完整(声明 100 字节只发 4 字节后即断开)。
async fn fake_server_truncated_body() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = sock.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let head = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 100\r\nConnection: close\r\n\r\nboom";
            let _ = writer.write_all(head.as_bytes()).await;
            let _ = writer.shutdown().await;
        }
    });
    format!("http://{addr}")
}

#[test]
fn status_error_captures_request_and_response_body() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_400("{\"error\":\"bad email\"}").await;
        let client = Client::new(&base).unwrap();
        let err = client
            .request(reqwest::Method::POST, "/auth/register")
            .json_unwrap(&json!({ "email": "x@y.z" }))
            .send_checked()
            .await
            .unwrap_err();
        match err {
            HttpError::Status {
                status,
                method,
                url,
                request_body,
                response_body,
            } => {
                assert_eq!(status.as_u16(), 400);
                assert_eq!(method, Method::POST);
                assert!(url.ends_with("/auth/register"));
                // 请求体与响应体都捕获到了
                assert!(request_body.unwrap().contains("email"));
                let text = response_body.unwrap().unwrap();
                assert!(text.contains("bad email"));
            }
            other => panic!("期望 Status 错误, 实际: {other:?}"),
        }
    });
}

#[test]
fn capture_disabled_skips_body() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_400("{\"error\":\"bad email\"}").await;
        let client = Client::new(&base)
            .unwrap()
            .with_capture(CaptureOptions::disabled());
        let err = client
            .request(reqwest::Method::POST, "/auth/register")
            .json_unwrap(&json!({ "email": "x@y.z" }))
            .send_checked()
            .await
            .unwrap_err();
        match err {
            HttpError::Status {
                request_body: None,
                response_body: None,
                ..
            } => {}
            other => panic!("关闭捕获后不应有内容快照: {other:?}"),
        }
    });
}

#[test]
fn response_read_failure_is_preserved_as_error() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_truncated_body().await;
        let client = Client::new(&base).unwrap();
        let err = client
            .request(reqwest::Method::GET, "/x")
            .send_checked()
            .await
            .unwrap_err();
        match err {
            HttpError::Status {
                status,
                response_body: Some(Err(reason)),
                ..
            } => {
                assert_eq!(status.as_u16(), 500);
                assert!(!reason.is_empty());
            }
            other => panic!("期望 Status + 响应体读取失败, 实际: {other:?}"),
        }
    });
}

#[test]
fn deref_exposes_reqwest_client() {
    // 编译期验证: Client 经 Deref 可用作 reqwest::Client
    fn takes_reqwest(_: &reqwest::Client) {}
    let client = Client::new("http://127.0.0.1:1").unwrap();
    takes_reqwest(&client);
}

#[test]
fn reqwest_error_kind_classifies_connect() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // 无法连接(127.0.0.1:1 拒绝连接): 细分到 connect_refused
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/x")
            .send()
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "connect_refused");
    });
}

#[test]
fn http_error_reqwest_kind_is_specific() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // 经 HttpError 封装(Client::new 路径):连接失败应细分,而非笼统 reqwest
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let err = client
            .request(reqwest::Method::GET, "/x")
            .send()
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "connect_refused", "HttpError 应细分传输层原因");
    });
}

#[test]
fn reqwest_error_kind_classifies_status() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_400("{\"error\":\"bad email\"}").await;
        // error_for_status 丢弃响应体但保留状态码: 分类为 http_status_400
        let err = reqwest::Client::new()
            .get(format!("{base}/x"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err();
        assert_eq!(err.kind(), "http_status_400");
    });
}

#[test]
fn check_status_passthrough_ok() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_echo().await;
        let client = Client::new(&base).unwrap();
        // 直接使用底层 reqwest::Client + 自由函数 check_status
        let resp = client
            .get(client.resolve("/ok").unwrap())
            .send()
            .await
            .unwrap();
        let resp = check_status(resp, reqwest::Method::GET, &CaptureOptions::default())
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    });
}

#[test]
fn check_status_captures_error_body() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_400("{\"error\":\"bad email\"}").await;
        let client = Client::new(&base).unwrap();
        let resp = client
            .get(client.resolve("/x").unwrap())
            .send()
            .await
            .unwrap();
        let err = check_status(resp, reqwest::Method::GET, &CaptureOptions::default())
            .await
            .unwrap_err();
        match err {
            HttpError::Status {
                status,
                response_body: Some(Ok(text)),
                ..
            } => {
                assert_eq!(status.as_u16(), 400);
                assert!(text.contains("bad email"), "{text}");
            }
            other => panic!("期望 Status + 响应体快照, 实际: {other:?}"),
        }
    });
}

#[test]
fn from_reqwest_applies_custom_timeout() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // 假服务器: 收到请求后睡 500ms 再响应
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = sock.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            let _ = writer.write_all(resp.as_bytes()).await;
        });
        let base = format!("http://{addr}");

        // 50ms 超时: 原来 Client::new 无法配置, 现在经 from_reqwest 生效
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(50))
            .build()
            .unwrap();
        let client = Client::from_reqwest(http, &base).unwrap();

        let err = client
            .request(reqwest::Method::GET, "/slow")
            .send()
            .await
            .unwrap_err();
        match err {
            HttpError::Reqwest(e) => {
                assert!(e.is_timeout(), "期望超时, 实际: {e}");
                assert_eq!(e.kind(), "timeout");
            }
            other => panic!("期望 Reqwest 传输错误, 实际: {other:?}"),
        }
    });
}

/// 假服务器:读取请求头与请求体,回显 `Content-Type` 与 body 原文
/// (用于校验 multipart 编码)。
async fn fake_server_echo() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = sock.into_split();
            let mut reader = BufReader::new(reader);

            let mut content_type = String::new();
            let mut content_length = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("content-type:") {
                    // 从原始行取值,保留大小写
                    content_type = line["content-type:".len()..].trim().to_string();
                } else if let Some(v) = lower.strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }

            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                reader.read_exact(&mut body).await.unwrap();
            }
            let preview = String::from_utf8_lossy(&body).to_string();
            let resp_body = format!("{content_type}\n{preview}");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                resp_body.len()
            );
            let _ = writer.write_all(resp.as_bytes()).await;
            let _ = writer.shutdown().await;
        }
    });
    format!("http://{addr}")
}

fn build_form() -> reqwest::multipart::Form {
    use reqwest::multipart::{Form, Part};

    Form::new().text("username", "alice").part(
        "avatar",
        Part::bytes(b"PNGDATA".to_vec())
            .file_name("a.png")
            .mime_str("image/png")
            .unwrap(),
    )
}

#[test]
fn post_multipart_sends_form_data() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_echo().await;
        let client = Client::new(&base).unwrap();

        let resp = client
            .request(reqwest::Method::POST, "/upload")
            .multipart(build_form())
            .send_checked()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let text = resp.text().await.unwrap();
        assert!(
            text.starts_with("multipart/form-data; boundary="),
            "Content-Type 应为 multipart/form-data: {text}"
        );
        assert!(text.contains("name=\"username\""), "{text}");
        assert!(text.contains("alice"), "{text}");
        assert!(
            text.contains("name=\"avatar\"; filename=\"a.png\""),
            "{text}"
        );
        assert!(text.contains("Content-Type: image/png"), "{text}");
        assert!(text.contains("PNGDATA"), "{text}");
    });
}

#[test]
fn request_builder_multipart_send_checked() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let base = fake_server_echo().await;
        let client = Client::new(&base).unwrap();

        let resp = client
            .request(Method::POST, "/upload")
            .multipart(build_form())
            .send_checked()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let text = resp.text().await.unwrap();
        assert!(text.contains("name=\"username\""), "{text}");
        assert!(text.contains("PNGDATA"), "{text}");
    });
}
