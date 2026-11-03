use super::*;

#[test]
fn session_keys_keep_one_key_per_provider() {
    let keys = SessionKeys::default();
    assert_eq!(keys.get("anthropic"), Ok(None));
    keys.set("anthropic", &Secret::new("sk-ant-one")).unwrap();
    keys.set("anthropic", &Secret::new("sk-ant-two")).unwrap();
    keys.set("local", &Secret::new("token")).unwrap();
    assert_eq!(keys.get("anthropic").unwrap().map(|k| k.expose().to_owned()), Some("sk-ant-two".into()));
    keys.delete("anthropic").unwrap();
    assert_eq!(keys.get("anthropic"), Ok(None));
    assert!(keys.get("local").unwrap().is_some());
    for bad in ["", "has space", "tab\there", "é-not-ascii"] {
        assert_eq!(keys.set("x", &Secret::new(bad)), Err(SecretError::Invalid), "{bad:?}");
    }
    assert_eq!(keys.set("x", &Secret::new("k".repeat(MAX_KEY_BYTES + 1))), Err(SecretError::Invalid));
}

#[test]
fn the_stored_key_comes_first_then_the_environment() {
    let keys = SessionKeys::default();
    let env = |value: &'static str| move |name: &str| (name == API_KEY_ENV).then(|| value.to_owned());
    assert_eq!(api_key(&keys, "anthropic", env(" sk-ant-env \n")).map(|k| k.expose().to_owned()), Some("sk-ant-env".into()));
    assert_eq!(api_key(&keys, "anthropic", env("not a key")), None);
    assert_eq!(api_key(&keys, "anthropic", |_| None), None);
    keys.set("anthropic", &Secret::new("sk-ant-stored")).unwrap();
    assert_eq!(api_key(&keys, "anthropic", env("sk-ant-env")).map(|k| k.expose().to_owned()), Some("sk-ant-stored".into()));
}

/// The real login keychain: run by hand (`cargo test -p pdfcraft-platform -- --ignored`), since
/// it changes the user's keychain and may ask to allow access.
#[test]
#[ignore = "uses the real login keychain"]
fn the_keychain_round_trips_a_key() {
    let account = "pdfcraft-test-account";
    let store = Keychain;
    if !Keychain::available() {
        assert_eq!(store.get(account), Err(SecretError::Unavailable));
        return;
    }
    store.set(account, &Secret::new("sk-ant-test-key")).unwrap();
    assert_eq!(store.get(account).unwrap().map(|k| k.expose().to_owned()), Some("sk-ant-test-key".into()));
    store.delete(account).unwrap();
    assert_eq!(store.get(account), Ok(None));
    store.delete(account).unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
mod https {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::ops::ControlFlow;
    use std::thread::JoinHandle;

    use pdfcraft_ai::transport::{HeaderValue, HttpRequest, Transport, TransportError};

    use super::super::HttpsTransport;
    use super::*;

    /// A one-shot HTTP server on the loopback interface: it reads one request, answers with
    /// `response` and hands back the request it read.
    fn serve(response: Vec<u8>) -> (String, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length =
                        head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            // The client may hang up first (a cap or a cancel): that is what some tests check.
            let _ = stream.write_all(&response);
            request
        });
        (format!("http://127.0.0.1:{port}/v1/messages"), handle)
    }

    fn post(url: &str) -> HttpRequest {
        HttpRequest {
            url: url.to_owned(),
            headers: vec![
                ("x-api-key".into(), HeaderValue::Secret(Secret::new("sk-ant-test"))),
                ("content-type".into(), HeaderValue::Plain("application/json".into())),
            ],
            body: br#"{"q":1}"#.to_vec(),
        }
    }

    fn send(url: &str, stop_after: Option<usize>) -> (Result<u16, TransportError>, Vec<u8>) {
        let transport = HttpsTransport::new("PdfCraft/test");
        let mut got = Vec::new();
        let mut chunks = 0usize;
        let result = transport.post_stream(&post(url), &mut |bytes| {
            got.extend_from_slice(bytes);
            chunks += 1;
            if stop_after.is_some_and(|n| chunks >= n) { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        (result, got)
    }

    #[test]
    fn the_answer_streams_back_and_the_request_is_sent_as_built() {
        let body = "event: ping\ndata: {\"type\":\"ping\"}\n\n".repeat(50);
        let (url, server) = serve(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}").into_bytes());
        let (result, got) = send(&url, None);
        assert_eq!(result, Ok(200));
        assert_eq!(got, body.as_bytes());
        let request = String::from_utf8_lossy(&server.join().unwrap()).into_owned();
        assert!(request.starts_with("POST /v1/messages HTTP/1.1"), "{request}");
        assert!(request.to_ascii_lowercase().contains("x-api-key: sk-ant-test"), "the key goes to the provider: {request}");
        assert!(request.ends_with(r#"{"q":1}"#), "{request}");
    }

    #[test]
    fn error_statuses_come_back_with_their_body() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let (url, server) = serve(format!("HTTP/1.1 401 Unauthorized\r\ncontent-length: {}\r\n\r\n{body}", body.len()).into_bytes());
        let (result, got) = send(&url, None);
        assert_eq!(result, Ok(401));
        assert_eq!(got, body.as_bytes());
        server.join().unwrap();
    }

    #[test]
    fn redirects_are_refused() {
        let (url, server) = serve(b"HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:1/steal\r\ncontent-length: 0\r\n\r\n".to_vec());
        let (result, got) = send(&url, None);
        assert!(matches!(result, Err(TransportError::Refused(_))), "{result:?}");
        assert!(got.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn an_answer_past_the_cap_stops() {
        let big = vec![b'x'; MAX_RESPONSE_BYTES + READ_EXTRA];
        let mut response = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(&big);
        let (url, server) = serve(response);
        let (result, got) = send(&url, None);
        assert_eq!(result, Err(TransportError::TooLarge(MAX_RESPONSE_BYTES)));
        assert!(got.len() <= MAX_RESPONSE_BYTES);
        server.join().unwrap();
    }
    const READ_EXTRA: usize = 64 * 1024;

    #[test]
    fn the_caller_can_cancel_between_chunks() {
        let body = "data: x\n\n".repeat(100_000);
        let (url, server) = serve(format!("HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n{body}").into_bytes());
        let (result, got) = send(&url, Some(1));
        assert_eq!(result, Err(TransportError::Cancelled));
        assert!(got.len() < body.len());
        server.join().unwrap();
    }

    #[test]
    fn nothing_is_sent_to_a_refused_address() {
        // Plain HTTP to another host is refused before any connection is tried.
        let (result, _) = send("http://api.anthropic.com/v1/messages", None);
        assert!(matches!(result, Err(TransportError::Refused(_))), "{result:?}");
        let (result, _) = send("file:///etc/passwd", None);
        assert!(matches!(result, Err(TransportError::Refused(_))), "{result:?}");
    }

    #[test]
    fn a_closed_port_is_a_connection_error() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (result, _) = send(&format!("http://127.0.0.1:{port}/v1/messages"), None);
        assert!(matches!(result, Err(TransportError::Connect(_) | TransportError::Io(_))), "{result:?}");
    }
}
