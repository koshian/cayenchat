//! Deterministic image fixtures and a local HTTP server for tests. Nothing
//! here contacts the network.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Cursor, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use image::{Frame, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage, codecs::gif::GifEncoder};

/// A solid red image in `format`.
pub fn encode(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    if format == ImageFormat::Jpeg {
        RgbImage::from_pixel(width, height, Rgb([255, 0, 0]))
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
    } else if format == ImageFormat::Bmp {
        // Only the signature matters: BMP is recognized and refused.
        bytes.extend_from_slice(b"BM");
        bytes.resize(64, 0);
    } else {
        RgbaImage::from_pixel(width, height, Rgba([255, 0, 0, 255]))
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
    }
    bytes
}

/// A 4×4 animated GIF: a red frame, then a blue one.
pub fn gif_two_frames() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            encoder
                .encode_frame(Frame::new(RgbaImage::from_pixel(4, 4, Rgba(color))))
                .unwrap();
        }
    }
    bytes
}

/// A canned HTTP response.
#[derive(Clone)]
pub struct Route {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Pause after the headers and between 1 KiB body chunks.
    pub delay: Duration,
    /// Send no Content-Length; the body ends when the connection closes.
    pub omit_length: bool,
}

impl Route {
    pub fn image(content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
            delay: Duration::ZERO,
            omit_length: false,
        }
    }

    pub fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
            delay: Duration::ZERO,
            omit_length: false,
        }
    }
}

/// A local HTTP/1.1 server on 127.0.0.1 serving fixed routes. Each request
/// is logged with its headers so tests can check what was sent.
pub struct FixtureServer {
    pub port: u16,
    pub requests: Arc<Mutex<Vec<String>>>,
    hits: Arc<AtomicUsize>,
}

impl FixtureServer {
    pub fn start(routes: HashMap<String, Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let hits = Arc::new(AtomicUsize::new(0));
        let routes = Arc::new(routes);
        {
            let (requests, hits) = (requests.clone(), hits.clone());
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let (routes, requests, hits) = (routes.clone(), requests.clone(), hits.clone());
                    thread::spawn(move || serve(stream, &routes, &requests, &hits));
                }
            });
        }
        Self {
            port,
            requests,
            hits,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn serve(
    stream: TcpStream,
    routes: &HashMap<String, Route>,
    requests: &Mutex<Vec<String>>,
    hits: &AtomicUsize,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    hits.fetch_add(1, Ordering::SeqCst);
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
    requests.lock().unwrap().push(head);
    let route = routes.get(&path).cloned().unwrap_or(Route {
        status: 404,
        headers: vec![("Content-Type".into(), "text/plain".into())],
        body: b"not found".to_vec(),
        delay: Duration::ZERO,
        omit_length: false,
    });
    let mut stream = stream;
    let mut response = format!("HTTP/1.1 {} Fixture\r\nConnection: close\r\n", route.status);
    let declares_length = route
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"));
    for (name, value) in &route.headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    if !declares_length && !route.omit_length {
        response.push_str(&format!("Content-Length: {}\r\n", route.body.len()));
    }
    response.push_str("\r\n");
    if stream.write_all(response.as_bytes()).is_err() {
        return;
    }
    for chunk in route.body.chunks(1024) {
        thread::sleep(route.delay);
        if stream.write_all(chunk).is_err() {
            return;
        }
    }
    let _ = stream.flush();
}
