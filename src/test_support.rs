use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinHandle, time::timeout};
use tower::ServiceExt;

use crate::{
    app::router,
    config::{UpstreamEndpoints, upstream_client_builder},
    pricing::UpstreamSource,
    state::AppState,
    storage::init_db,
    util::Clock,
};

pub(crate) const TEST_NOW: i64 = 1_700_000_000;

pub(crate) struct ManualClock {
    unix: AtomicI64,
    monotonic: Mutex<Instant>,
}

impl ManualClock {
    pub(crate) fn advance(&self, duration: Duration) {
        self.unix.fetch_add(
            i64::try_from(duration.as_secs()).expect("test clock advance fits in i64"),
            Ordering::SeqCst,
        );
        self.advance_monotonic(duration);
    }

    pub(crate) fn advance_monotonic(&self, duration: Duration) {
        *self.monotonic.lock().unwrap() += duration;
    }

    pub(crate) fn set_unix(&self, now: i64) {
        self.unix.store(now, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_monotonic(&self) -> Instant {
        *self.monotonic.lock().unwrap()
    }

    fn now_unix(&self) -> i64 {
        self.unix.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
pub(crate) struct FixtureResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: String,
    gate: Option<Arc<Semaphore>>,
}

impl FixtureResponse {
    pub(crate) fn json(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            headers: HeaderMap::from_iter([(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )]),
            body: body.to_string(),
            gate: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct MockProvider {
    response: Arc<Mutex<FixtureResponse>>,
    requests: Arc<AtomicUsize>,
    started: Arc<Semaphore>,
}

impl MockProvider {
    fn new(body: Value) -> Self {
        Self {
            response: Arc::new(Mutex::new(FixtureResponse::json(body))),
            requests: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(Semaphore::new(0)),
        }
    }

    pub(crate) fn set_response(&self, response: FixtureResponse) {
        *self.response.lock().unwrap() = response;
    }

    pub(crate) fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    // A permit releases one response; tests do not need to sleep or race a timer.
    pub(crate) fn hold_responses(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        self.response.lock().unwrap().gate = Some(gate.clone());
        gate
    }

    pub(crate) async fn wait_for_request(&self) {
        timeout(Duration::from_secs(5), self.started.acquire())
            .await
            .expect("mock provider received a request")
            .unwrap()
            .forget();
    }

    async fn respond(&self) -> impl IntoResponse {
        let response = self.response.lock().unwrap().clone();
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.started.add_permits(1);
        if let Some(gate) = response.gate {
            gate.acquire().await.unwrap().forget();
        }
        (response.status, response.headers, response.body)
    }
}

pub(crate) struct MockUpstreams {
    providers: [MockProvider; 4],
    base_url: String,
    server: JoinHandle<()>,
}

impl MockUpstreams {
    async fn start() -> Self {
        let providers = [
            MockProvider::new(json!({"bitcoin": {"usd": 100_000.0}})),
            MockProvider::new(json!({"data": {
                "base": "BTC", "currency": "USD", "amount": "100100.00"
            }})),
            MockProvider::new(json!({"error": [], "result": {
                "XXBTZUSD": {"c": ["100200.00", "1"]}
            }})),
            MockProvider::new(json!({"symbol": "BTCUSD", "bid": "100300.00"})),
        ];
        let mut app = Router::new();
        for (path, provider) in ["/coingecko", "/coinbase", "/kraken", "/gemini"]
            .into_iter()
            .zip(providers.iter().cloned())
        {
            app = app.route(
                path,
                get(move || {
                    let provider = provider.clone();
                    async move { provider.respond().await.into_response() }
                }),
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            providers,
            base_url,
            server,
        }
    }

    pub(crate) fn provider(&self, source: UpstreamSource) -> &MockProvider {
        let index = match source {
            UpstreamSource::CoinGecko => 0,
            UpstreamSource::Coinbase => 1,
            UpstreamSource::Kraken => 2,
            UpstreamSource::Gemini => 3,
        };
        &self.providers[index]
    }

    pub(crate) fn request_counts(&self) -> [usize; 4] {
        std::array::from_fn(|index| self.providers[index].request_count())
    }

    fn endpoints(&self) -> UpstreamEndpoints {
        UpstreamEndpoints {
            coingecko: format!("{}/coingecko", self.base_url),
            coinbase: format!("{}/coinbase", self.base_url),
            kraken: format!("{}/kraken", self.base_url),
            gemini: format!("{}/gemini", self.base_url),
        }
    }
}

impl Drop for MockUpstreams {
    fn drop(&mut self) {
        self.server.abort();
    }
}

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "bitcoin-api-tests-{}-{unique}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) struct TestApp {
    pub(crate) state: AppState,
    pub(crate) clock: Arc<ManualClock>,
    pub(crate) upstreams: MockUpstreams,
    router: Router,
    _database: TestDatabase,
}

impl TestApp {
    pub(crate) async fn new() -> Self {
        Self::with_database_seed(|_| {}).await
    }

    pub(crate) async fn with_database_seed(seed: impl FnOnce(&std::path::Path)) -> Self {
        let database = TestDatabase::new();
        let db_path = database.0.join("prices.db");
        seed(&db_path);
        init_db(db_path.clone()).await.unwrap();
        let upstreams = MockUpstreams::start().await;
        let clock = Arc::new(ManualClock {
            unix: AtomicI64::new(TEST_NOW),
            monotonic: Mutex::new(Instant::now()),
        });
        let client = upstream_client_builder().no_proxy().build().unwrap();
        let state =
            AppState::with_dependencies(client, db_path, upstreams.endpoints(), clock.clone());
        Self {
            router: router(state.clone()),
            state,
            clock,
            upstreams,
            _database: database,
        }
    }

    pub(crate) async fn presence(&self, active: bool) {
        self.presence_for("test-viewer", active).await;
    }

    pub(crate) async fn presence_for(&self, session_id: &str, active: bool) {
        let (status, _) = self
            .request(
                Method::POST,
                "/api/presence",
                Some(json!({"session_id": session_id, "active": active})),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    // Rebuild process-local state around the same database and fixture endpoints.
    pub(crate) async fn restart(&mut self) {
        init_db(self.state.db_path.clone()).await.unwrap();
        self.state = AppState::with_dependencies(
            self.state.client.clone(),
            self.state.db_path.clone(),
            self.upstreams.endpoints(),
            self.clock.clone(),
        );
        self.router = router(self.state.clone());
    }

    pub(crate) async fn price(&self) -> (StatusCode, Value) {
        self.request(Method::GET, "/api/price", None).await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                body.map_or_else(String::new, |body| body.to_string()),
            ))
            .unwrap();
        let response = timeout(
            Duration::from_secs(10),
            self.router.clone().oneshot(request),
        )
        .await
        .expect("test route completed")
        .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
