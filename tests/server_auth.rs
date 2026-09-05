use arrow_mc::server::auth::{AuthClient, AuthError, AuthLimits};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{oneshot, watch},
};

fn limits() -> AuthLimits {
    AuthLimits {
        max_in_flight: 1,
        max_response_bytes: 4096,
        connect_timeout: Duration::from_secs(1),
        read_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(2),
    }
}

async fn mock(response: Vec<u8>) -> (AuthClient, oneshot::Receiver<String>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        tx.send(String::from_utf8(request).unwrap()).ok();
        socket.write_all(&response).await.ok();
    });
    (
        AuthClient::for_loopback_tests(address, limits()).unwrap(),
        rx,
    )
}

fn response(status: u16, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
}

#[tokio::test]
async fn queried_name_and_returned_uuid_properties_are_preserved_with_encoded_query() {
    let body = r#"{"id":"b50ad385829d3141a2167e7d7539ba7f","name":"ignored","properties":[{"name":"textures","value":"a+b","signature":"signed"}],"ignored":{"deep":[1,2]}}"#;
    let (client, request) = mock(response(200, body)).await;
    let (_cancel, mut cancelled) = watch::channel(false);
    let profile = client
        .has_joined(
            "A&B",
            "-123abc",
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            &mut cancelled,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(profile.name, "A&B");
    assert_eq!(profile.id[0], 0xb5);
    assert_eq!(profile.properties[0].value, "a+b");
    assert_eq!(profile.properties[0].signature.as_deref(), Some("signed"));
    let request = request.await.unwrap();
    assert!(request.contains("username=A%26B&serverId=-123abc&ip=127.0.0.1"));
}

#[tokio::test]
async fn absent_profiles_and_http_failures_remain_distinct() {
    for (status, body, expected) in [
        (204, "", Ok(None)),
        (200, "null", Ok(None)),
        (200, "{}", Ok(None)),
        (
            403,
            "{}",
            Err(AuthError::HttpStatus {
                status: 403,
                unavailable: false,
            }),
        ),
        (
            503,
            "{}",
            Err(AuthError::HttpStatus {
                status: 503,
                unavailable: true,
            }),
        ),
        (
            500,
            r#"{"error":"ForbiddenOperationException"}"#,
            Err(AuthError::HttpStatus {
                status: 500,
                unavailable: false,
            }),
        ),
        (200, r#"{"id":"invalid"}"#, Err(AuthError::InvalidProfile)),
    ] {
        let (client, _) = mock(response(status, body)).await;
        let (_cancel, mut cancelled) = watch::channel(false);
        assert_eq!(
            client
                .has_joined("Player", "123", None, &mut cancelled)
                .await,
            expected
        );
    }
}

#[tokio::test]
async fn streaming_body_limit_applies_without_content_length_and_redirects_are_not_followed() {
    let oversized = "a".repeat(5000);
    let chunked = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
        oversized.len(),
        oversized
    );
    let (client, _) = mock(chunked.into_bytes()).await;
    let (_cancel, mut cancelled) = watch::channel(false);
    assert_eq!(
        client
            .has_joined("Player", "123", None, &mut cancelled)
            .await,
        Err(AuthError::BodyTooLarge)
    );
    let redirect =
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/leak\r\nContent-Length: 0\r\n\r\n"
            .to_vec();
    let (client, _) = mock(redirect).await;
    assert_eq!(
        client
            .has_joined("Player", "123", None, &mut cancelled)
            .await,
        Err(AuthError::HttpStatus {
            status: 302,
            unavailable: false
        })
    );
}

#[tokio::test]
async fn property_count_and_utf16_limits_are_enforced() {
    for properties in [
        format!("[{}]", vec![r#"{"name":"x","value":"y"}"#; 17].join(",")),
        format!(r#"[{{"name":"{}","value":"y"}}]"#, "x".repeat(65)),
        format!(
            r#"[{{"name":"x","value":"y","signature":"{}"}}]"#,
            "x".repeat(1025)
        ),
    ] {
        let body =
            format!(r#"{{"id":"b50ad385829d3141a2167e7d7539ba7f","properties":{properties}}}"#);
        let (client, _) = mock(response(200, &body)).await;
        let (_cancel, mut cancelled) = watch::channel(false);
        assert_eq!(
            client
                .has_joined("Player", "123", None, &mut cancelled)
                .await,
            Err(AuthError::InvalidProfile)
        );
    }
}

#[tokio::test]
async fn cancellation_releases_admission_and_slow_body_hits_absolute_deadline() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let client = AuthClient::for_loopback_tests(listener.local_addr().unwrap(), limits()).unwrap();
    let (cancel, mut cancelled) = watch::channel(false);
    let cloned = client.clone();
    let job = tokio::spawn(async move {
        cloned
            .has_joined("Player", "123", None, &mut cancelled)
            .await
    });
    let (socket, _) = listener.accept().await.unwrap();
    let (_other, mut other) = watch::channel(false);
    assert_eq!(
        client.has_joined("Player", "123", None, &mut other).await,
        Err(AuthError::Busy)
    );
    cancel.send(true).unwrap();
    assert_eq!(job.await.unwrap(), Err(AuthError::Cancelled));
    drop(socket);
    // A subsequent request on the same client is admitted after cancellation.
    let (cancel_again, mut cancelled_again) = watch::channel(false);
    let cloned = client.clone();
    let second = tokio::spawn(async move {
        cloned
            .has_joined("Player", "123", None, &mut cancelled_again)
            .await
    });
    let (socket, _) = listener.accept().await.unwrap();
    cancel_again.send(true).unwrap();
    assert_eq!(second.await.unwrap(), Err(AuthError::Cancelled));
    drop(socket);
    let mut short = limits();
    short.request_timeout = Duration::from_millis(60);
    let timed = AuthClient::for_loopback_tests(listener.local_addr().unwrap(), short).unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n")
            .await
            .unwrap();
        // Activity stays below the idle timeout, but cannot reset the total
        // deadline. Stop when cancellation drops the client response stream.
        loop {
            if socket.write_all(b" ").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let result = timed.has_joined("Player", "123", None, &mut other).await;
    assert_eq!(result, Err(AuthError::Timeout));
    assert!(
        AuthClient::for_loopback_tests(SocketAddr::from(([192, 0, 2, 1], 80)), limits()).is_err()
    );
}
