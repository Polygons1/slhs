use std::{
    cmp::Ordering,
    collections::HashMap,
    fmt::{self, Display},
    io::{self, Read, Write},
    marker::PhantomData,
    net::{TcpListener, TcpStream, ToSocketAddrs},
    str::{Bytes, FromStr},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
};

// my crate :)
use midsync::*;

struct Join {
    inner: String,
}

impl FromIterator<String> for Join {
    fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
        let mut s: String = Default::default();

        for item in iter {
            s.push_str(&item);
            s.push_str("\r\n")
        }

        Self { inner: s }
    }
}

impl fmt::Display for Join {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.inner)
    }
}

#[macro_export]
macro_rules! headers {
    ($($key:expr => $value:expr,)*) => {
        {
            let mut map = ::std::collections::HashMap::new();
            $(
                map.insert(stringify!($key), format!("{}", $value));
            )*
            map
        }
    };
}

#[derive(Clone, PartialEq, Eq)]
enum CompressionMethod {
    Gzip,
    Brotli,
}

enum HandlerNotFound {
    NoMethod,
    NoPath,
}

impl Ord for CompressionMethod {
    fn cmp(&self, other: &Self) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }
        match (self.clone(), other.clone()) {
            (Self::Brotli, Self::Gzip) => Ordering::Less,
            (Self::Gzip, Self::Brotli) => Ordering::Greater,
            _ => unreachable!(),
        }
    }
}

impl PartialOrd for CompressionMethod {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(any(feature = "gzip", feature = "brotli"))]
mod rw {
    use std::{
        io::{self, Read, Write},
        ops::{Deref, DerefMut},
    };

    pub struct RW {
        inner: Vec<u8>,
        pos: usize,
    }

    impl RW {
        pub fn new(inner: Vec<u8>) -> Self {
            Self { inner, pos: 0 }
        }
    }

    impl Read for RW {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.inner.len() {
                return Ok(0);
            }
            let remaining = &self.inner[self.pos..];
            let amt = buf.len().min(remaining.len());
            buf[..amt].copy_from_slice(&remaining[..amt]);
            self.pos += amt;
            Ok(amt)
        }
    }

    impl Write for RW {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Deref for RW {
        type Target = Vec<u8>;
        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl DerefMut for RW {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }
}

#[cfg(any(feature = "gzip", feature = "brotli"))]
use rw::*;

impl FromStr for CompressionMethod {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "gzip" => Ok(CompressionMethod::Gzip),
            "br" | "brotli" => Ok(CompressionMethod::Brotli),
            _ => Err(()),
        }
    }
}

impl Display for CompressionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Brotli => "br",
            Self::Gzip => "gzip",
        })
    }
}

pub struct Response {
    headers: HashMap<&'static str, String>,
    status: Option<i32>,
    __allowed_compressions: Option<Vec<CompressionMethod>>,
}

impl Response {
    pub fn status(&mut self, status: i32) {
        self.status = Some(status)
    }
    pub fn headers(&mut self, headers: HashMap<&'static str, String>) {
        self.headers = headers;
    }
    pub fn end<B: AsRef<[u8]> + 'static>(self, body: B) -> (String, Vec<u8>) {
        let s = self.status.unwrap_or(200);
        let (data, method) = self.compress(Box::new(body));
        build_response(
            s,
            match s {
                200 => "OK",
                201 => "Created",
                202 => "Accepted",
                204 => "No Content",
                301 => "Moved Permanently",
                302 => "Found",
                304 => "Not Modified",
                400 => "Bad Request",
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "Not Found",
                405 => "Method Not Allowed",
                408 => "Request Timeout",
                409 => "Conflict",
                410 => "Gone",
                418 => "I'm a teapot", // my personal favorite
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                501 => "Not Implemented",
                502 => "Bad Gateway",
                503 => "Service Unavailable",
                504 => "Gateway Timeout",
                _ => "Unknown Status Code",
            },
            self.headers.clone(),
            data,
            method,
        )
    }
    fn compress(&self, mut body: Box<dyn AsRef<[u8]>>) -> (Vec<u8>, Option<CompressionMethod>) {
        let server_methods = self.__allowed_compressions.clone();
        let mut allowed_methods: Vec<CompressionMethod> = vec![
            #[cfg(feature = "gzip")]
            CompressionMethod::Gzip,
            #[cfg(feature = "brotli")]
            CompressionMethod::Brotli,
        ];
        allowed_methods.sort();

        match server_methods {
            Some(methods) => {
                if allowed_methods.is_empty() {
                    (body.as_mut().as_ref().to_vec(), None)
                } else {
                    for method in allowed_methods {
                        if methods.contains(&method) {
                            return (self.compress_with(body, method.clone()), Some(method));
                        }
                    }
                    (body.as_mut().as_ref().to_vec(), None)
                }
            }
            None => (body.as_mut().as_ref().to_vec(), None),
        }
    }
    #[cfg(feature = "gzip")]
    fn compress_with_gzip(
        &self,
        mut body: Box<dyn AsRef<[u8]>>,
        _method: CompressionMethod,
    ) -> Vec<u8> {
        use std::io::BufReader;

        let enc = flate2::write::GzEncoder::new(RW::new(Vec::new()), flate2::Compression::best());
        let mut writer = BufReader::new(enc);
        let mut body = body.as_mut().as_ref().to_vec();
        writer.read_exact(body.as_mut_slice()).unwrap_or(());
        let enc = writer.into_inner();
        match enc.finish() {
            Ok(v) if !v.is_empty() => v.clone(),
            _ => body.to_vec(),
        }
    }
    #[cfg(feature = "brotli")]
    fn compress_with_brotli(
        &self,
        mut body: Box<dyn AsRef<[u8]>>,
        _method: CompressionMethod,
    ) -> Vec<u8> {
        let body_bytes = body.as_mut().as_ref().to_vec();
        let mut compressed = Vec::with_capacity(body_bytes.len());
        match brotli::BrotliCompress(
            &mut RW::new(body_bytes.clone()),
            &mut compressed,
            &brotli::enc::BrotliEncoderInitParams(),
        ) {
            Ok(_) if !compressed.is_empty() => compressed,
            _ => body_bytes,
        }
    }
    #[cfg(any(feature = "gzip", feature = "brotli"))]
    fn compress_with(&self, body: Box<dyn AsRef<[u8]>>, method: CompressionMethod) -> Vec<u8> {
        match method {
            #[cfg(feature = "gzip")]
            CompressionMethod::Gzip => self.compress_with_gzip(body, method),
            #[cfg(feature = "brotli")]
            CompressionMethod::Brotli => self.compress_with_brotli(body, method),
            #[cfg(any(not(feature = "brotli"), not(feature = "gzip")))]
            _ => unreachable!(),
        }
    }
    #[cfg(all(not(feature = "gzip"), not(feature = "brotli")))]
    fn compress_with(&self, _body: Box<dyn AsRef<[u8]>>, _method: CompressionMethod) -> Vec<u8> {
        unreachable!()
    }

    pub(crate) fn new(methods: Option<Vec<CompressionMethod>>) -> Self {
        Self {
            headers: HashMap::new(),
            status: None,
            __allowed_compressions: methods,
        }
    }
}

fn build_response<S: ToString + Display>(
    status: i32,
    status_text: S,
    mut headers: HashMap<&str, String>,
    body: Vec<u8>,
    used_enc: Option<CompressionMethod>,
) -> (String, Vec<u8>) {
    if let Some(used_enc) = used_enc {
        let s = used_enc.to_string();
        headers.insert("Content-Encoding", s);
    }
    (
        format!(
            "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\nServer: BootLeg-HTTP\r\n{}\r\n",
            body.len(),
            headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Join>(),
        ),
        body,
    )
}

#[doc(hidden)]
fn _build_streaming_response<S: ToString + Display>(
    status: i32,
    status_text: S,
    headers: HashMap<&str, String>,
) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {status_text}\r\nServer: BootLeg-HTTP\r\n{}\r\n",
        headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Join>(),
    )
    .into_bytes()
}

#[doc(hidden)]
fn __exp_data_stream(stream: &mut TcpStream) -> io::Result<(Sender<String>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();

    stream.write_all(&_build_streaming_response(
        200,
        "OK",
        headers! {
            Content-Type => "text/event-stream",
            Connection => "close",
            Cache-Control => "no-cache",
            Access-Control-Allow-Origin => "*",
        },
    ))?;

    let mut stream_clone = stream.try_clone()?;

    let h = thread::spawn(move || {
        while let Ok(data) = rx.recv() {
            let sse_data = format!("data: {}\n\n", data);

            if stream_clone.write_all(sse_data.as_bytes()).is_err() {
                break;
            }
            if stream_clone.flush().is_err() {
                break;
            }
        }

        let _ = stream_clone.flush();
        let _ = stream_clone.shutdown(std::net::Shutdown::Both);
        drop(stream_clone)
    });

    Ok((tx, h))
}

fn read_stream(stream: &mut TcpStream) -> Vec<u8> {
    let mut buf = [0; 1024];
    let mut req = vec![];

    loop {
        match stream.read(&mut buf) {
            Ok(n) if n > 0 => {
                req.extend_from_slice(&buf[..n]);
                if n < 1024 {
                    break;
                }
            }
            Ok(_) => break,
            Err(_) => break,
        }
    }

    req
}

fn parse_headers(headers: &str) -> HashMap<&str, &str> {
    let headers = headers.split("\r\n");
    let mut hm = HashMap::new();

    for (k, v) in headers
        .filter(|h| !h.trim().is_empty())
        .flat_map(|h| h.split_once(": "))
    {
        hm.insert(k, v);
    }

    hm
}

// input: a=b&c=d&e=f
fn parse_url_params(input: &str) -> Option<HashMap<String, String>> {
    if input.is_empty() {
        return Some(Default::default());
    }
    let mut params = HashMap::new();

    for param in input.split("&") {
        let (k, v) = param.split_once("=")?;
        params.insert(k.to_string(), v.to_string());
    }

    Some(params)
}

async fn http<
    H: Fn(&str, String, Request, Response) -> Result<(String, Vec<u8>), HandlerNotFound>,
>(
    mut stream: TcpStream,
    invoke_handler: H,
) -> io::Result<()> {
    let request = String::from_utf8(read_stream(&mut stream)).map_err(io::Error::other)?;
    let (head, body) = request
        .split_once("\r\n\r\n")
        .unwrap_or((request.as_str(), ""));
    let (first_line, headers) = head.split_once("\r\n").ok_or(io::Error::other(""))?;
    let (method, path) = first_line
        .split_once(" ")
        .map(|(method, path_with_http_ver)| Some((method, path_with_http_ver.split_once(" ")?.0)))
        .ok_or(io::Error::other(""))?
        .ok_or(io::Error::other(""))?;

    let (path, url_params) = path.split_once("?").unwrap_or((path, ""));
    let url_params = parse_url_params(url_params).ok_or(io::Error::other(""))?;

    let headers = parse_headers(headers);

    let response = invoke_handler(
        method.to_ascii_lowercase().as_str(),
        path.to_string(),
        Request::new(
            headers.clone(),
            if body.is_empty() {
                None
            } else {
                Some(body.to_string())
            },
            url_params,
        ),
        Response::new(
            headers
                .get("accept-encoding")
                .or(headers.get("Accept-Encoding"))
                .or(headers.get("Accept-encoding"))
                .map(|h| h.split(",").filter_map(|s| s.trim().parse().ok()).collect()),
        ),
    );

    let (head, body) = match response {
        Ok(ref r) => r,
        Err(HandlerNotFound::NoPath) => {
            let mut res = Response::new(None);
            res.status(404);
            &res.end("Not Found")
        }
        Err(HandlerNotFound::NoMethod) => {
            let mut res = Response::new(None);
            res.status(405);
            &res.end("Method Not Allowed")
        }
    };

    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    stream.write_all(body)?;
    stream.flush()?;
    stream.shutdown(std::net::Shutdown::Both)?;

    Ok(())
}

type ReqFn = dyn Fn(Request, Response) -> (String, Vec<u8>);

pub struct Request<'lt> {
    pub headers: HashMap<&'lt str, &'lt str>,
    pub body: Option<String>,
    pub url_params: HashMap<String, String>,
}

impl<'lt> Request<'lt> {
    fn new(
        headers: HashMap<&'lt str, &'lt str>,
        body: Option<String>,
        url_params: HashMap<String, String>,
    ) -> Self {
        Self {
            headers,
            body,
            url_params,
        }
    }
}

type MethodMap = HashMap<&'static str, Box<ReqFn>>;

#[derive(Default)]
pub struct Router {
    routes: HashMap<String, MethodMap>,
}

impl Router {
    pub fn get<P: ToString, H: Fn(Request, Response) -> (String, Vec<u8>) + 'static>(
        &mut self,
        path: P,
        h: H,
    ) -> &mut Self {
        self.routes
            .entry(path.to_string())
            .or_default()
            .entry("get")
            .or_insert(Box::new(h));
        self
    }
    pub fn post<P: ToString, H: Fn(Request, Response) -> (String, Vec<u8>) + 'static>(
        &mut self,
        path: P,
        h: H,
    ) -> &mut Self {
        self.routes
            .entry(path.to_string())
            .or_default()
            .entry("post")
            .or_insert(Box::new(h));
        self
    }
    pub fn start<A: ToSocketAddrs>(&mut self, addr: A) {
        let s = TcpListener::bind(addr).unwrap();
        let mut pool = Pool::default();

        while let Ok((strm, _)) = s.accept() {
            pool.add(async {
                drop(http(strm, |m, p, req, res| self.invoke_handler(m, p, req, res)).await);
            });
            pool.poll();
        }
    }
    fn invoke_handler(
        &self,
        m: &str,
        p: String,
        req: Request,
        res: Response,
    ) -> Result<(String, Vec<u8>), HandlerNotFound> {
        Ok((self
            .routes
            .get(&p)
            .ok_or(HandlerNotFound::NoPath)?
            .get(&m)
            .ok_or(HandlerNotFound::NoMethod)?)(req, res))
    }
}
