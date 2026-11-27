//! Container healthcheck for the distroless runtime image, which has no shell
//! or curl. Usage: `invokr-healthcheck <port> [path]` — exits 0 iff
//! `GET http://127.0.0.1:<port><path>` (default path `/health`) answers 200.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

fn status_is_ok(port: u16, path: &str) -> std::io::Result<bool> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    // One write: `write!` on an unbuffered stream sends each piece separately.
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;

    // Only the status line matters ("HTTP/1.1 200 OK"); read until it ends.
    let mut buf = [0u8; 64];
    let mut len = 0;
    while len < buf.len() && !buf[..len].contains(&b'\n') {
        match stream.read(&mut buf[len..])? {
            0 => break,
            n => len += n,
        }
    }
    let status_line = String::from_utf8_lossy(&buf[..len]);
    Ok(status_line.split_whitespace().nth(1) == Some("200"))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(port) = args.next().and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("usage: invokr-healthcheck <port> [path]");
        return ExitCode::from(2);
    };
    let path = args.next().unwrap_or_else(|| "/health".to_owned());

    match status_is_ok(port, &path) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("unhealthy: non-200 from 127.0.0.1:{port}{path}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("unhealthy: 127.0.0.1:{port}{path}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Serves one request with `response`, returning the port and the
    /// request line the client sent.
    fn serve_once(response: &'static str) -> (u16, thread::JoinHandle<String>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut req = [0u8; 256];
            let n = conn.read(&mut req).unwrap();
            conn.write_all(response.as_bytes()).unwrap();
            let req = String::from_utf8_lossy(&req[..n]).into_owned();
            req.lines().next().unwrap_or_default().to_owned()
        });
        (port, handle)
    }

    #[test]
    fn healthy_on_200() {
        let (port, server) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        assert!(status_is_ok(port, "/invokr/health").unwrap());
        assert_eq!(server.join().unwrap(), "GET /invokr/health HTTP/1.1");
    }

    #[test]
    fn unhealthy_on_non_200() {
        let (port, server) = serve_once("HTTP/1.1 503 Service Unavailable\r\n\r\n");
        assert!(!status_is_ok(port, "/health").unwrap());
        server.join().unwrap();
    }

    #[test]
    fn unhealthy_when_status_merely_contains_200() {
        let (port, server) = serve_once("HTTP/1.1 500 Error-200\r\n\r\n");
        assert!(!status_is_ok(port, "/health").unwrap());
        server.join().unwrap();
    }

    #[test]
    fn error_when_nothing_listens() {
        let port = {
            let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(status_is_ok(port, "/health").is_err());
    }
}
