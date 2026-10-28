use super::*;
use crate::{
    model::{PageContext, PageKind, Source},
    transport::{Mode, Options},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn content_length(request: &str) -> Result<Option<usize>> {
    request
        .split("\r\n\r\n")
        .next()
        .context("request headers")?
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse().context("request content length"))
        .transpose()
}

async fn read_request(socket: &mut TcpStream) -> Result<String> {
    let mut bytes = Vec::new();
    for _ in 0..8 {
        let mut chunk = [0; 512];
        let received = socket.read(&mut chunk).await?;
        ensure!(received > 0, "fixture request ended before its body");
        bytes.extend_from_slice(&chunk[..received]);
        let request = std::str::from_utf8(&bytes)?;
        if let Some(header_end) = request.find("\r\n\r\n") {
            let expected = content_length(request)?.unwrap_or(0);
            if bytes.len() >= header_end + 4 + expected {
                return Ok(request.to_owned());
            }
        }
    }
    anyhow::bail!("fixture request exceeded 4096 bytes")
}

async fn serve(listener: TcpListener) -> Result<Vec<String>> {
    let mut requests = Vec::new();
    for _ in 0..4 {
        let (mut socket, _) = listener.accept().await?;
        let request = read_request(&mut socket).await?;
        let status = if request.starts_with("POST ") && content_length(&request)?.is_none() {
            411
        } else {
            200
        };
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: 2\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nok"
        );
        socket.write_all(response.as_bytes()).await?;
        requests.push(request);
    }
    Ok(requests)
}

fn transport(url: Url) -> Result<(Transport, Target)> {
    let mut transport = Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: std::path::PathBuf::new(),
        headless: true,
        rps: 1.0,
    })?;
    transport.http_test_url = Some(url);
    transport.clients.insert(
        "https://schools.wiaawi.org".into(),
        Client::builder().no_proxy().build()?,
    );
    let mut target = Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=1".into(),
        PageContext::default(),
    );
    target.method = Some("POST".into());
    Ok((transport, target))
}

async fn requests(url: Url, address: std::net::SocketAddr) -> Result<()> {
    let mut negative = TcpStream::connect(address).await?;
    negative
        .write_all(b"POST /fixture HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut rejected = String::new();
    negative.read_to_string(&mut rejected).await?;
    assert!(rejected.starts_with("HTTP/1.1 411 "));
    let (mut transport, mut target) = transport(url)?;
    let result = async {
        let capture = transport.fetch_http(&target, &BTreeMap::new()).await?;
        assert_eq!(capture.status, 200);
        assert_eq!(capture.body, b"ok");
        target.body = Some(serde_json::json!({"schoolId": 1}));
        let capture = transport.fetch_http(&target, &BTreeMap::new()).await?;
        assert_eq!(capture.status, 200);
        target.method = None;
        target.body = None;
        let capture = transport.fetch_http(&target, &BTreeMap::new()).await?;
        assert_eq!(capture.status, 200);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = transport.close().await;
    result.and(closed)
}

#[tokio::test]
async fn given_directory_requiring_post_length_when_empty_post_is_sent_then_zero_length_is_explicit_and_json_and_get_are_unchanged()
-> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let url = Url::parse(&format!("http://{address}/fixture"))?;
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(30), serve(listener))
            .await
            .context("fixture watchdog")?
    });
    let result = tokio::time::timeout(Duration::from_secs(30), requests(url, address))
        .await
        .context("client watchdog");
    let frames = server.await??;
    result??;
    assert_eq!(frames.len(), 4);
    assert!(frames[1].starts_with("POST /fixture HTTP/1.1\r\n"));
    assert_eq!(content_length(&frames[1])?, Some(0));
    assert_eq!(
        frames[1]
            .split_once("\r\n\r\n")
            .context("empty POST separator")?
            .1,
        ""
    );
    assert!(frames[2].starts_with("POST /fixture HTTP/1.1\r\n"));
    let json = frames[2]
        .split_once("\r\n\r\n")
        .context("JSON POST separator")?
        .1;
    assert_eq!(content_length(&frames[2])?, Some(json.len()));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(json)?,
        serde_json::json!({"schoolId":1})
    );
    assert!(frames[3].starts_with("GET /fixture HTTP/1.1\r\n"));
    assert_eq!(content_length(&frames[3])?, None);
    assert_eq!(
        frames[3].split_once("\r\n\r\n").context("GET separator")?.1,
        ""
    );
    Ok(())
}
