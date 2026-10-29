use super::*;
use serde_json::json;

#[test]
fn artifact_verification_rejects_corruption_truncation_escape_and_symlink() -> Result<()> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let path = root.path().join("owned");
    write(&path, b"retained artifact")?;
    let descriptor = json!({"path":path,"bytes":17,"sha256":digest(b"retained artifact")});
    assert_eq!(artifact(&descriptor, root.path())?, path);
    fs::write(&path, b"changed artifact!")?;
    assert!(artifact(&descriptor, root.path()).is_err());
    fs::write(&path, b"short")?;
    assert!(artifact(&descriptor, root.path()).is_err());
    let escape = outside.path().join("external");
    write(&escape, b"retained artifact")?;
    let escaped = json!({"path":escape,"bytes":17,"sha256":digest(b"retained artifact")});
    assert!(artifact(&escaped, root.path()).is_err());
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&escape, &link)?;
    assert!(file_digest(&link, JSON_LIMIT).is_err());
    assert!(read(&path, 2).is_err());
    Ok(())
}

#[tokio::test]
async fn proof_http_refuses_nonloopback_without_connecting() -> Result<()> {
    let http = Http::new()?;
    assert!(
        http.request("https://example.com", "/", None, None)
            .await
            .is_err()
    );
    assert!(
        http.request("http://192.0.2.1", "/", None, None)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn admin_query_negotiates_json_instead_of_default_arrow_binary() -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let server = async {
        let (mut socket, _) = listener.accept().await?;
        let mut received = Vec::new();
        let mut accepts_json = false;
        for _ in 0..8 {
            let mut buffer = [0_u8; 1024];
            let count = socket.read(&mut buffer).await?;
            ensure!(count > 0, "fixture client closed before request headers");
            received.extend_from_slice(buffer.get(..count).context("fixture read invalid")?);
            let text = std::str::from_utf8(&received)?;
            if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                let expected = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map_or(Ok(0), |(_, value)| value.trim().parse::<usize>())?;
                if body.len() >= expected {
                    accepts_json = headers
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("accept: application/json"));
                    break;
                }
            }
        }
        let (content_type, body) = if accepts_json {
            ("application/json", r#"{"rows":[{"id":"inv_owned"}]}"#)
        } else {
            (
                "application/vnd.apache.arrow.stream",
                "ARROW1-owned-default-binary",
            )
        };
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
        socket.shutdown().await?;
        Ok::<_, anyhow::Error>(accepts_json)
    };
    let client = Http::new()?;
    let query = json!({"query":"SELECT id FROM sys_invocation"});
    let (accepted, (status, value)) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::try_join!(
            server,
            client.request(&origin, "/query", Some(&query), None)
        )
    })
    .await??;
    assert!(
        accepted,
        "the native admin helper omitted Accept: application/json"
    );
    assert_eq!(status, 200);
    assert_eq!(value, json!({"rows":[{"id":"inv_owned"}]}));
    Ok(())
}
