use super::*;
use sqlx::postgres::PgConnectOptions;

#[test]
fn given_mismatched_admin_endpoint_when_creating_then_boundary_rejects_before_any_connection() {
    let target = PgConnectOptions::new_without_pgpass()
        .host("127.0.0.1")
        .port(55432);
    let other_host = target.clone().host("other.example");
    let other_port = target.clone().port(5432);
    let other_socket = target.clone().socket("/different/socket");
    assert!(database::admin_endpoint(&other_host, &target).is_err());
    assert!(database::admin_endpoint(&other_port, &target).is_err());
    assert!(database::admin_endpoint(&other_socket, &target).is_err());
    assert!(database::admin_endpoint(&target, &target).is_ok());
}
