#![doc = include_str!("../readme.md")]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::http::Request;
use axum::response::Response;
use futures_util::future::BoxFuture;
use maxminddb::geoip2;
use tower::Service;

pub use maxminddb::Reader;

/// Configuration for applying request delays (tariffs) based on IP country.
///
/// This struct maps ISO country codes to delay durations,
/// and uses a MaxMind DB to determine the country for a given IP address.
#[derive(Debug)]
pub struct Config {
    // Mapping of ISO country codes (e.g., "US", "FR") to delay durations
    tariffs: HashMap<Box<str>, Duration>,
    // MaxMind database reader used to look up IP address locations
    reader: Reader<Vec<u8>>,
}

impl Config {
    /// Create a new `Config` with an empty tariff map and a provided MaxMind DB reader.
    ///
    /// # Arguments
    ///
    /// * `reader` - A MaxMind DB reader, e.g., from GeoLite2-Country.mmdb
    ///
    /// # Example
    ///
    /// ```
    /// let reader = axum_tariff::Reader::open_readfile("assets/GeoLite2-Country-Test.mmdb").unwrap();
    /// let config = axum_tariff::Config::new(reader);
    /// ```
    pub fn new(reader: Reader<Vec<u8>>) -> Self {
        Self {
            tariffs: Default::default(),
            reader,
        }
    }

    /// Add a country code and associated delay to the tariff configuration.
    ///
    /// This uses the ISO alpha-2 country code (e.g., "US", "DE", "IN").
    ///
    /// # Arguments
    ///
    /// * `code` - A 2-letter ISO country code.
    /// * `delay` - A duration representing how long to delay requests from that country.
    ///
    /// # Example
    ///
    /// ```
    /// let reader = axum_tariff::Reader::open_readfile("assets/GeoLite2-Country-Test.mmdb").unwrap();
    /// let config = axum_tariff::Config::new(reader)
    ///     .with("US", tokio::time::Duration::from_secs(2))  // Delay US traffic by 2 seconds
    ///     .with("CN", tokio::time::Duration::from_millis(500)); // Delay CN traffic by 500ms
    /// ```
    pub fn with(mut self, code: &str, delay: Duration) -> Self {
        self.tariffs.insert(Box::from(code.to_uppercase()), delay);
        self
    }

    /// Convert the configuration into a middleware `TariffLayer`
    /// that can be applied to an Axum router.
    ///
    /// # Example
    ///
    /// ```
    /// let reader = axum_tariff::Reader::open_readfile("assets/GeoLite2-Country-Test.mmdb").unwrap();
    /// let layer = axum_tariff::Config::new(reader)
    ///     .with("FR", tokio::time::Duration::from_secs(1))
    ///     .into_layer();
    ///
    /// async fn handler() -> axum::http::StatusCode {
    ///     axum::http::StatusCode::NO_CONTENT
    /// }
    ///
    /// let app: axum::Router<()> = axum::Router::new()
    ///     .route("/", axum::routing::get(handler))
    ///     .layer(layer);
    /// ```
    pub fn into_layer(self) -> TariffLayer {
        TariffLayer {
            config: Arc::new(self),
        }
    }

    /// Get the configured delay duration for a given IP address,
    /// based on its resolved country code.
    ///
    /// Returns `Some(duration)` if the country has a configured tariff,
    /// otherwise returns `None`.
    fn get_delay_for_ip(&self, ip: IpAddr) -> Option<Duration> {
        self.reader
            .lookup::<geoip2::Country>(ip)
            .ok()
            .flatten()
            .and_then(|geo| geo.country)
            .and_then(|country| country.iso_code)
            .and_then(|code| self.tariffs.get(code.to_uppercase().as_str()))
            .cloned()
    }
}

/// A `tower::Layer` that wraps services to apply country-based request delays.
///
/// Can be applied to an Axum router using `.layer(...)`.
#[derive(Clone)]
pub struct TariffLayer {
    config: Arc<Config>,
}

impl<S> tower::Layer<S> for TariffLayer {
    type Service = TariffService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TariffService {
            inner,
            config: self.config.clone(),
        }
    }
}

/// A `tower::Service` that introduces delay based on the client IP address's country.
///
/// It uses the MaxMind GeoIP database to look up the country, and delays the request
/// if the country has a configured tariff.
#[derive(Clone)]
pub struct TariffService<S> {
    inner: S,
    config: Arc<Config>,
}

impl<S, B> Service<Request<B>> for TariffService<S>
where
    B: Send + 'static,
    S: Clone,
    S: Service<Request<B>, Response = Response<B>> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let config = Arc::clone(&self.config);
        let client_ip = extract_client_ip(&req);

        Box::pin(async move {
            if let Some(delay) = client_ip.and_then(|ip| config.get_delay_for_ip(ip)) {
                tokio::time::sleep(delay).await;
            }

            inner.call(req).await
        })
    }
}

/// Extract the client's IP address from headers or socket address.
///
/// Tries `X-Forwarded-For` header first, then falls back to `ConnectInfo`.
fn extract_client_ip<B>(req: &Request<B>) -> Option<IpAddr> {
    if let Some(header) = req.headers().get("x-forwarded-for") {
        if let Ok(ip_str) = header.to_str() {
            if let Some(ip_str) = ip_str.split(',').next() {
                return ip_str.trim().parse().ok();
            }
        }
    }

    req.extensions()
        .get::<axum::extract::connect_info::ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Instant;

    use axum::Router;
    use axum::body::Body;
    use axum::extract::connect_info::ConnectInfo;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*; // for `oneshot`

    const IP_REGION: &str = "GB";
    const IP_TEST: &str = "2.125.160.218";

    fn test_reader() -> Reader<Vec<u8>> {
        Reader::open_readfile("assets/GeoLite2-Country-Test.mmdb")
            .expect("You need the test MaxMind DB at assets/GeoLite2-Country-Test.mmdb")
    }

    #[tokio::test]
    async fn test_tariff_config_basic_mapping() {
        let config = Config::new(test_reader()).with(IP_REGION, Duration::from_millis(1234));

        let ip: IpAddr = IP_TEST.parse().unwrap();
        let delay = config.get_delay_for_ip(ip);

        assert_eq!(delay, Some(Duration::from_millis(1234)));
    }

    #[tokio::test]
    async fn test_middleware_applies_delay() {
        let layer = Config::new(test_reader())
            .with(IP_REGION, Duration::from_millis(200))
            .into_layer();

        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(layer)
            .with_state(());

        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let req = Request::builder()
            .uri("/")
            .header("x-forwarded-for", IP_TEST) // FR IP
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();

        let start = Instant::now();
        let response = app.clone().oneshot(req).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert!(elapsed >= Duration::from_millis(180)); // Allow for small overhead
    }

    #[tokio::test]
    async fn test_extract_ip_header_and_fallback() {
        // Header parsing
        let req = Request::builder()
            .header("x-forwarded-for", "8.8.8.8")
            .body(())
            .unwrap();

        assert_eq!(
            extract_client_ip(&req),
            Some("8.8.8.8".parse::<IpAddr>().unwrap())
        );

        // Fallback to ConnectInfo
        let mut req = Request::builder().body(()).unwrap();
        let addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, Some(addr.ip()));
    }
}
