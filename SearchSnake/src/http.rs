//! A small blocking HTTP/1.1 server.
//!
//! The Battlesnake engine makes one short POST per turn and reuses the
//! connection, so a thread-per-connection accept loop with keep-alive is both
//! sufficient and predictable. Being dependency-free also means nothing in the
//! request path can allocate or block in ways that are hard to account for
//! against a 500 ms deadline.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Refuse bodies larger than this rather than allocating whatever a caller asks for.
const MAX_BODY: usize = 1 << 20;

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Response {
    pub fn json(body: String) -> Self {
        Response {
            status: 200,
            content_type: "application/json",
            body,
        }
    }

    pub fn text(status: u16, body: &str) -> Self {
        Response {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.to_string(),
        }
    }

    pub fn empty() -> Self {
        Response {
            status: 200,
            content_type: "application/json",
            body: String::new(),
        }
    }
}

pub struct Server {
    listener: TcpListener,
}

impl Server {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        Ok(Server {
            listener: TcpListener::bind(addr)?,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves until the process exits. `handler` receives
    /// `(method, path, body)` and must be safe to call from several threads.
    pub fn run<H>(self, threads: usize, handler: H) -> io::Result<()>
    where
        H: Fn(&str, &str, &str) -> Response + Send + Sync + 'static,
    {
        let listener = Arc::new(self.listener);
        let handler = Arc::new(handler);
        let mut workers = Vec::with_capacity(threads);

        for _ in 0..threads.max(1) {
            let listener = Arc::clone(&listener);
            let handler = Arc::clone(&handler);
            workers.push(thread::spawn(move || loop {
                match listener.accept() {
                    // A failed connection must never take the worker down.
                    Ok((stream, _)) => {
                        let _ = serve_connection(stream, handler.as_ref());
                    }
                    Err(_) => continue,
                }
            }));
        }
        for worker in workers {
            let _ = worker.join();
        }
        Ok(())
    }
}

fn serve_connection<H>(stream: TcpStream, handler: &H) -> io::Result<()>
where
    H: Fn(&str, &str, &str) -> Response,
{
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // client hung up
        }
        let request_line = line.trim_end().to_string();
        if request_line.is_empty() {
            continue; // tolerate stray blank lines between requests
        }

        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();

        let mut content_length = 0usize;
        let mut keep_alive = true;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 {
                return Ok(());
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                let value = value.trim();
                match name.as_str() {
                    "content-length" => content_length = value.parse().unwrap_or(0),
                    "connection" => keep_alive = !value.eq_ignore_ascii_case("close"),
                    _ => {}
                }
            }
        }

        if content_length > MAX_BODY {
            write_response(&mut writer, &Response::text(413, "payload too large"), false)?;
            return Ok(());
        }

        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body)?;
        }
        let body = String::from_utf8_lossy(&body);

        let path = target.split('?').next().unwrap_or("/");
        let response = handler(&method, path, &body);
        write_response(&mut writer, &response, keep_alive)?;

        if !keep_alive {
            return Ok(());
        }
    }
}

fn write_response(writer: &mut TcpStream, response: &Response, keep_alive: bool) -> io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    write!(
        writer,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    )?;
    writer.write_all(response.body.as_bytes())?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> SocketAddr {
        let server = Server::bind("127.0.0.1:0").expect("binds");
        let addr = server.local_addr().expect("has an address");
        thread::spawn(move || {
            let _ = server.run(2, |method, path, body| match (method, path) {
                ("GET", "/") => Response::json(r#"{"ok":true}"#.to_string()),
                ("POST", "/echo") => Response::json(format!(r#"{{"got":{}}}"#, body.len())),
                _ => Response::text(404, "nope"),
            });
        });
        addr
    }

    fn request(addr: SocketAddr, raw: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connects");
        stream.write_all(raw.as_bytes()).expect("writes");
        stream.flush().expect("flushes");
        let mut out = String::new();
        // The server closes the connection, so read to EOF.
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("sets timeout");
        let _ = stream.read_to_string(&mut out);
        out
    }

    #[test]
    fn answers_a_get() {
        let addr = start();
        let response = request(
            addr,
            "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {}", response);
        assert!(response.contains(r#"{"ok":true}"#));
        assert!(response.contains("Content-Length: 11"));
    }

    #[test]
    fn reads_a_post_body() {
        let addr = start();
        let payload = r#"{"a":1}"#;
        let response = request(
            addr,
            &format!(
                "POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            ),
        );
        assert!(response.contains(r#"{"got":7}"#), "got: {}", response);
    }

    #[test]
    fn serves_two_requests_on_one_connection() {
        let addr = start();
        let mut stream = TcpStream::connect(addr).expect("connects");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\nGET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .expect("writes both");
        let mut out = String::new();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("sets timeout");
        let _ = stream.read_to_string(&mut out);
        assert_eq!(
            out.matches("HTTP/1.1 200 OK").count(),
            2,
            "keep-alive should answer both: {}",
            out
        );
    }

    #[test]
    fn unknown_routes_are_404() {
        let addr = start();
        let response = request(
            addr,
            "GET /missing HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 404 Not Found"));
    }

    #[test]
    fn ignores_query_strings_when_routing() {
        let addr = start();
        let response = request(
            addr,
            "GET /?debug=1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn rejects_an_oversized_body() {
        let addr = start();
        let response = request(
            addr,
            &format!(
                "POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY + 1
            ),
        );
        assert!(response.starts_with("HTTP/1.1 413"), "got: {}", response);
    }
}
