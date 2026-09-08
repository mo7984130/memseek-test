use memseek_test::ctxlibs::http_client::Client;
use serde_json::json;

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
