# axum-tariff

**_“Some countries... they need to wait.” — You, probably_**

🚦 Middleware for [Axum](https://github.com/tokio-rs/axum) that introduces configurable request delays based on the client's country (IP geolocation).

Inspired by the chaotic beauty of international trade wars, this crate uses the [MaxMind GeoIP2 database](https://dev.maxmind.com/geoip/docs/databases) to detect IPs by country and apply a delay ("tariff") per your configuration.

---

## ✨ Features

- ⏱️ Delay requests from specific countries
- 🌍 Uses MaxMind DB for IP-to-country mapping
- 🧱 Simple `tower::Layer` and `tower::Service` integration
- 🧪 Tested with mock MaxMind DB and real IPs

---

## 🚀 Usage

### 1. Add to your project

```base
cargo add axum-tariff
```

### 2. Add the middleware

```rust,no_run
use axum::{routing::get, Router};
use std::{net::IpAddr, time::Duration};
use axum_tariff::{Config, Reader};

#[tokio::main]
async fn main() {
    let reader = Reader::open_readfile("assets/GeoLite2-Country-Test.mmdb").unwrap();

    let layer = Config::new(reader)
        .with("US", Duration::from_secs(1))
        .with("FR", Duration::from_millis(500))
        .into_layer();

    let app: Router<()> = Router::new()
        .route("/", get(|| async { "Hello, world!" }))
        .layer(layer);


    let listener = tokio::net::TcpListener::bind("127.0.1.1:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

---

## 🧪 Running the Tests

This crate includes tests that use the [GeoLite2-Country-Test.mmdb](https://dev.maxmind.com/geoip/docs/databases/test-data). You'll need to place it at:

```ignore
assets/GeoLite2-Country-Test.mmdb
```

Then:

```bash
cargo test
```

---

## 📦 Example: Applying a 2s delay to China

```rust,ignore
let config = Config::new(reader)
    .with("CN", Duration::from_secs(2));
```

---

## 📄 License

MIT
