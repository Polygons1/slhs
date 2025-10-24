# SLHS (Simple Light-weight HTTP Server)

SLHS is a simple, no-frills HTTP server library written in Rust. It aims to provide a minimalistic yet functional base for building web services without the overhead of larger frameworks.

## Features

- **Simple Routing**: Define `GET` and `POST` endpoints with ease.
- **Basic Request/Response Handling**: Access headers, body, URL parameters, and set response status and headers.
- **Optional Compression**: Support for `gzip` and `brotli` compression (requires feature flags).
- **Thread Pool**: Uses a simple thread pool for handling concurrent requests.

## Installation

Add SLHS to your `Cargo.toml` with:

```bash
cargo add slhs
```

To enable compression, add the respective features with:

```bash
cargo add slhs --features "gzip,brotli"
```

## Examples

### 1. Basic Server

This example sets up a server that listens on `127.0.0.1:8080` and responds to a `GET` request at the root path `/` and a `POST` request at `/submit`.

```rust
use slhs::*;

fn main() {
    let mut router = Router::default();

    router.get("/", |req, mut res| {
        println!("Headers: {:?}", req.headers);
        println!("URL Params: {:?}", req.url_params);
        res.end("Hello from SLHS!")
    });

    router.post("/submit", |req, mut res| {
        println!("Received POST request with body: {:?}", req.body);
        res.status(201); // Created
        res.end(format!("Thanks for the data: {:?}", req.body.unwrap_or_default()))
    });

    println!("Server listening on 127.0.0.1:8080");
    router.start("127.0.0.1:8080");
}
```

### 2. Handling URL Parameters and Custom Headers

This example demonstrates how to access URL query parameters and set custom response headers using the `headers!` macro.

```rust
use slhs::*;

fn main() {
    let mut router = Router::default();

    router.get("/greet", |req, mut res| {
        let name = req.url_params.get("name").map(|s| s.as_str()).unwrap_or("Guest");

        res.headers(headers!{
            X-Powered-By => "SLHS",
            Content-Type => "text/plain",
        });
        res.end(format!("Hello, {}!", name))
    });

    println!("Try navigating to http://127.0.0.1:8080/greet?name=Alice");
    router.start("127.0.0.1:8080");
}
```

### 3. Response Status Codes and Body Compression

SLHS automatically handles `404 Not Found` and `405 Method Not Allowed`. You can explicitly set other statuses. If compression features are enabled and the client supports it via `Accept-Encoding` header, the response body will be compressed.

```rust
use slhs::*;

fn main() {
    let mut router = Router::default();

    router.get("/forbidden", |_, mut res| {
        res.status(403); // Forbidden
        res.end("You don't have permission to view this page.")
    });

    router.get("/teapot", |_, mut res| {
        // This will be compressed if the client supports it and `brotli` or `gzip` features are enabled.
        res.status(418); // I'm a teapot
        res.end("I'm a little teapot, short and stout...")
    });

    println!("Server listening on 127.0.0.1:8080");
    router.start("127.0.0.1:8080");
}
```

## Contributing

Feel free to open issues or submit pull requests!

## License

This project is licensed under the MIT License.
