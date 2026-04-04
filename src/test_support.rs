use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{Arc, Mutex},
    thread,
};

use serde_json::Value;

pub fn spawn_json_http_server(
    response_status: &str,
    response_body: &str,
) -> (SocketAddr, Arc<Mutex<String>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
    let address = listener.local_addr().expect("listener addr");
    let request = Arc::new(Mutex::new(String::new()));
    let captured_request = Arc::clone(&request);
    let status = response_status.to_string();
    let body = response_body.to_string();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut request_bytes = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let bytes_read = stream.read(&mut buffer).expect("read request");
            if bytes_read == 0 {
                break;
            }
            request_bytes.extend_from_slice(&buffer[..bytes_read]);
            if request_complete(&request_bytes) {
                break;
            }
        }
        *captured_request.lock().expect("capture request") =
            String::from_utf8_lossy(&request_bytes).into_owned();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });
    (address, request, handle)
}

pub fn request_json(raw_request: &str) -> Value {
    let body = raw_request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("request body");
    serde_json::from_str(body).expect("request json")
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = find_bytes(bytes, b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("content-length") {
                return None;
            }
            value.trim().parse::<usize>().ok()
        })
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
