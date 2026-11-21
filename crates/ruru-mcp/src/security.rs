//! Cross-cutting HTTP hardening: login throttling, a request timeout, and security headers.
//!
//! None of this replaces the network boundary (Ruru is meant to sit on a trusted LAN behind
//! a firewall); it limits what a hostile or broken client on that LAN can do.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::AppState;

/// Failed logins allowed per client IP inside [`FAILURE_WINDOW`] before it is locked out.
pub const MAX_FAILURES: u32 = 5;
/// How long failures are remembered, and how long a lockout lasts.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// argon2id costs ~19 MiB and tens of ms per verify; bound how many run at once so a login
/// flood can't exhaust memory or CPU.
const CONCURRENT_VERIFIES: usize = 2;
/// Upper bound on remembered IPs, so the table itself can't be grown without limit.
const MAX_TRACKED_IPS: usize = 4096;

/// Brute-force and resource guard for `/api/login`.
///
/// Keyed by the TCP peer address. Behind a reverse proxy every request shares the proxy's
/// address, so the limit becomes global — stricter, never looser.
pub struct LoginGuard {
    failures: Mutex<HashMap<IpAddr, Failures>>,
    verifies: std::sync::Arc<Semaphore>,
}

#[derive(Clone, Copy)]
struct Failures {
    count: u32,
    first: Instant,
}

pub enum LoginCheck {
    /// Go ahead; hold the permit for the duration of the password verification.
    Allowed(OwnedSemaphorePermit),
    /// Too many recent failures from this address; retry after this long.
    LockedOut(Duration),
    /// Too many verifications in flight right now.
    Busy,
}

impl Default for LoginGuard {
    fn default() -> Self {
        Self {
            failures: Mutex::new(HashMap::new()),
            verifies: std::sync::Arc::new(Semaphore::new(CONCURRENT_VERIFIES)),
        }
    }
}

impl LoginGuard {
    pub fn check(&self, ip: IpAddr) -> LoginCheck {
        {
            let mut map = self.failures.lock().expect("login guard lock");
            if let Some(f) = map.get(&ip).copied() {
                let age = f.first.elapsed();
                if age >= FAILURE_WINDOW {
                    map.remove(&ip);
                } else if f.count >= MAX_FAILURES {
                    return LoginCheck::LockedOut(FAILURE_WINDOW - age);
                }
            }
        }
        match self.verifies.clone().try_acquire_owned() {
            Ok(permit) => LoginCheck::Allowed(permit),
            Err(_) => LoginCheck::Busy,
        }
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut map = self.failures.lock().expect("login guard lock");
        if map.len() >= MAX_TRACKED_IPS && !map.contains_key(&ip) {
            map.retain(|_, f| f.first.elapsed() < FAILURE_WINDOW);
            if map.len() >= MAX_TRACKED_IPS {
                // Still full of live entries: drop the oldest to make room.
                if let Some(oldest) = map.iter().min_by_key(|(_, f)| f.first).map(|(k, _)| *k) {
                    map.remove(&oldest);
                }
            }
        }
        let entry = map.entry(ip).or_insert(Failures {
            count: 0,
            first: Instant::now(),
        });
        entry.count = entry.count.saturating_add(1);
    }

    pub fn record_success(&self, ip: IpAddr) {
        self.failures.lock().expect("login guard lock").remove(&ip);
    }
}

/// Fail a request that takes longer than `[server].request_timeout_secs` to produce a
/// response (a slow client trickling a body, or a wedged handler). Streaming bodies such as
/// the live preview are unaffected: the timer stops once response headers are ready.
pub async fn request_timeout(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let limit = Duration::from_secs(state.config.server.request_timeout_secs);
    match tokio::time::timeout(limit, next.run(req)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, "request timed out").into_response(),
    }
}

/// Conservative headers on every response. Only `frame-ancestors` is set in the CSP so the
/// SPA's own assets are never broken by it; the point is to stop the admin panel being
/// framed (clickjacking) and to stop content sniffing.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn locks_out_after_max_failures_and_only_that_ip() {
        let g = LoginGuard::default();
        for _ in 0..MAX_FAILURES {
            assert!(matches!(g.check(ip(1)), LoginCheck::Allowed(_)));
            g.record_failure(ip(1));
        }
        assert!(matches!(g.check(ip(1)), LoginCheck::LockedOut(_)));
        assert!(matches!(g.check(ip(2)), LoginCheck::Allowed(_)));
    }

    #[test]
    fn success_clears_the_failure_count() {
        let g = LoginGuard::default();
        for _ in 0..MAX_FAILURES - 1 {
            g.record_failure(ip(1));
        }
        g.record_success(ip(1));
        g.record_failure(ip(1));
        assert!(matches!(g.check(ip(1)), LoginCheck::Allowed(_)));
    }

    #[test]
    fn concurrent_verifies_are_bounded() {
        let g = LoginGuard::default();
        let held: Vec<_> = (0..CONCURRENT_VERIFIES)
            .map(|_| match g.check(ip(1)) {
                LoginCheck::Allowed(p) => p,
                _ => panic!("should be allowed"),
            })
            .collect();
        assert!(matches!(g.check(ip(2)), LoginCheck::Busy));
        drop(held);
        assert!(matches!(g.check(ip(2)), LoginCheck::Allowed(_)));
    }

    #[test]
    fn tracked_ips_are_bounded() {
        let g = LoginGuard::default();
        for i in 0..(MAX_TRACKED_IPS as u32 + 10) {
            g.record_failure(IpAddr::from(i.to_be_bytes()));
        }
        assert!(g.failures.lock().unwrap().len() <= MAX_TRACKED_IPS);
    }
}
