//! Tiny OAuth token/userinfo fixture for the rootless Docker acceptance harness.
//! It uses only std so it can be built offline into the disposable test image.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn response(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn decode_form_value(value: &str) -> String {
    let mut bytes = Vec::with_capacity(value.len());
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'+' => {
                bytes.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < raw.len() => {
                let hex = std::str::from_utf8(&raw[index + 1..index + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    bytes.push(byte);
                    index += 3;
                } else {
                    bytes.push(raw[index]);
                    index += 1;
                }
            }
            byte => {
                bytes.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn serve(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut request = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    let header_end;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                request.extend_from_slice(&chunk[..count]);
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    header_end = position + 4;
                    break;
                }
                if request.len() > 16 * 1024 {
                    response(&mut stream, "413 Payload Too Large", "{}");
                    return;
                }
            }
        }
    }
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let first = headers.lines().next().unwrap_or("");
    if first.starts_with("GET /authorize") {
        response(&mut stream, "200 OK", "{}");
        return;
    }
    if first.starts_with("GET /userinfo") {
        let token = headers
            .lines()
            .find_map(|line| line.strip_prefix("Authorization: Bearer runtime-provider-token-"))
            .or_else(|| {
                headers.lines().find_map(|line| {
                    line.strip_prefix("authorization: Bearer runtime-provider-token-")
                })
            })
            .unwrap_or("")
            .trim();
        if token.is_empty()
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            response(&mut stream, "401 Unauthorized", "{}");
        } else {
            response(
                &mut stream,
                "200 OK",
                &format!("{{\"sub\":\"{token}\",\"name\":\"Runtime Test\"}}"),
            );
        }
        return;
    }
    if first.starts_with("POST /token") {
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while request.len().saturating_sub(header_end) < content_length {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => request.extend_from_slice(&chunk[..count]),
            }
        }
        let form = String::from_utf8_lossy(&request[header_end..]);
        let code = form
            .split('&')
            .find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == "code").then(|| decode_form_value(value))
            })
            .unwrap_or_default();
        if code.is_empty()
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            response(&mut stream, "400 Bad Request", "{}");
        } else {
            response(
                &mut stream,
                "200 OK",
                &format!(
                    "{{\"access_token\":\"runtime-provider-token-{code}\",\"token_type\":\"Bearer\"}}"
                ),
            );
        }
        return;
    }
    response(&mut stream, "404 Not Found", "{}");
}

fn main() -> std::io::Result<()> {
    let port = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(18081);
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    for stream in listener.incoming().flatten() {
        thread::spawn(|| serve(stream));
    }
    Ok(())
}
