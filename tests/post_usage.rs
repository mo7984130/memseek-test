use memseek_test::ctxlibs::http_client::{CaptureOptions, Client, HttpError};
use reqwest::Method;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
