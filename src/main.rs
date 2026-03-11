use axum::{
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
    routing::get,
    Json, Router,
};
use maxminddb::geoip2;
use moka::future::Cache;
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::Arc,
};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct IpInfoResponse {
    ip: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
    time_zone: Option<String>,
    accuracy_radius: Option<u16>,
    country_code: Option<String>,
    country_name: Option<String>,
    country_name_zh: Option<String>,
    subdivision_code: Option<String>,
    subdivision_name: Option<String>,
    city_name: Option<String>,
    city_name_zh: Option<String>,
    postal_code: Option<String>,
    isp: Option<String>,
}

struct AppState {
    reader: maxminddb::Reader<Vec<u8>>,
    cache: Cache<String, IpInfoResponse>,
}

fn lookup_ip(reader: &maxminddb::Reader<Vec<u8>>, ip_str: &str) -> IpInfoResponse {
    let ip = match ip_str.parse() {
        Ok(ip) => ip,
        Err(_) => return empty_response(ip_str.to_string()),
    };

    let record: geoip2::City = match reader.lookup(ip) {
        Ok(r) => r,
        Err(_) => return empty_response(ip_str.to_string()),
    };

    IpInfoResponse {
        ip: ip_str.to_string(),
        latitude: record.location.as_ref().and_then(|l| l.latitude),
        longitude: record.location.as_ref().and_then(|l| l.longitude),
        time_zone: record.location.as_ref().and_then(|l| l.time_zone).map(String::from),
        accuracy_radius: record.location.as_ref().and_then(|l| l.accuracy_radius),
        country_code: record.country.as_ref().and_then(|c| c.iso_code).map(String::from),
        country_name: record
            .country.as_ref()
            .and_then(|c| c.names.as_ref())
            .and_then(|n| n.get("en"))
            .map(|s| s.to_string()),
        country_name_zh: record
            .country.as_ref()
            .and_then(|c| c.names.as_ref())
            .and_then(|n| n.get("zh-CN").or_else(|| n.get("en")))
            .map(|s| s.to_string()),
        subdivision_code: record
            .subdivisions.as_ref()
            .and_then(|s| s.first())
            .and_then(|s| s.iso_code)
            .map(String::from),
        subdivision_name: record
            .subdivisions.as_ref()
            .and_then(|s| s.first())
            .and_then(|s| s.names.as_ref())
            .and_then(|n| n.get("en"))
            .map(|s| s.to_string()),
        city_name: record
            .city.as_ref()
            .and_then(|c| c.names.as_ref())
            .and_then(|n| n.get("en"))
            .map(|s| s.to_string()),
        city_name_zh: record
            .city.as_ref()
            .and_then(|c| c.names.as_ref())
            .and_then(|n| n.get("zh-CN").or_else(|| n.get("en")))
            .map(|s| s.to_string()),
        postal_code: record.postal.as_ref().and_then(|p| p.code).map(String::from),
        isp: None,
    }
}

fn empty_response(ip: String) -> IpInfoResponse {
    IpInfoResponse {
        ip,
        latitude: None,
        longitude: None,
        time_zone: None,
        accuracy_radius: None,
        country_code: None,
        country_name: None,
        country_name_zh: None,
        subdivision_code: None,
        subdivision_name: None,
        city_name: None,
        city_name_zh: None,
        postal_code: None,
        isp: None,
    }
}

async fn get_ip(
    State(state): State<Arc<AppState>>,
    Path(ip): Path<String>,
) -> Json<IpInfoResponse> {
    let result = state
        .cache
        .get_with(ip.clone(), async { lookup_ip(&state.reader, &ip) })
        .await;
    Json(result)
}

async fn get_ip_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Json<IpInfoResponse> {
    let ip_str = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| addr.ip().to_string());

    let result = state
        .cache
        .get_with(ip_str.clone(), async { lookup_ip(&state.reader, &ip_str) })
        .await;
    Json(result)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let db_path = std::env::var("GEOIP_DB_PATH")
        .unwrap_or_else(|_| "db/GeoLite2-City.mmdb".to_string());

    let data = std::fs::read(&db_path)
        .unwrap_or_else(|e| panic!("Failed to read GeoIP database at {}: {}", db_path, e));
    let reader = maxminddb::Reader::from_source(data)
        .unwrap_or_else(|e| panic!("Failed to parse GeoIP database: {}", e));

    let cache: Cache<String, IpInfoResponse> = Cache::builder()
        .max_capacity(10_000)
        .time_to_live(std::time::Duration::from_secs(24 * 3600))
        .build();

    let state = Arc::new(AppState { reader, cache });

    let app = Router::new()
        .route("/ip/me", get(get_ip_me))
        .route("/ip/:ip", get(get_ip))
        .with_state(state);

    let addr = "0.0.0.0:8080";
    tracing::info!("Listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .unwrap();
}
