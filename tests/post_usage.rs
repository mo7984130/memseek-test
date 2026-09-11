use memseek_test::ctxlibs::http_client::{CaptureOptions, Client, HttpError};
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
            .post("/auth/register", json!({ "email": "x@y.z" }))
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
            .post("/auth/register", json!({ "email": "x@y.z" }))
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
        let err = client.get("/x").await.unwrap_err();
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
fn post_accepts_json_value_directly() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let client = Client::new("http://127.0.0.1:1").unwrap();
        // 用户期望的写法:直接传 json! 的值(不再需要 &)
        // 连接 localhost:1 会失败,这里只验证编译与签名兼容
        let _ = client
            .post("/auth/login", json!({ "username": "x", "password": "y" }))
            .await;
    });
}

#[test]
fn post_still_accepts_reference() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let client = Client::new("http://127.0.0.1:1").unwrap();
        // 传引用的旧用法仍然兼容
        let _ = client
            .post("/auth/login", &json!({ "username": "x" }))
            .await;
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
            .post_multipart("/upload", build_form())
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
