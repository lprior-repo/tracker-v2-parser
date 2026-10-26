# BDD Enforcer Reference

## Behavior Catalog Example

```markdown
## Behavior Catalog

### Scenario 1: User login with valid credentials
- Given: user account exists with email "alice@example.com", password "secret"
- When: POST /login { email: "alice@example.com", password: "secret" }
- Then: status 200, response body contains token

### Scenario 2: User login with wrong password
- Given: user account exists with email "alice@example.com", password "secret"
- When: POST /login { email: "alice@example.com", password: "wrong" }
- Then: status 401, response body contains error message

### Scenario 3: User profile retrieval
- Given: authenticated user with valid session token
- When: GET /profile with Authorization header
- Then: status 200, response body contains user data
```

## Scenario Example (Rust)

```rust
// Scenario: User login with valid credentials
#[test]
fn scenario_user_login_valid_credentials() {
    // Given: a user account exists
    let app = TestApp::new()
        .with_user("alice@example.com", "secret");

    // When: submitting login with valid credentials
    let response = app.post("/login", json!({
        "email": "alice@example.com",
        "password": "secret"
    }));

    // Then: response contains a valid session token
    assert_eq!(response.status, 200);
    let token = response.json()["token"].as_str()
        .expect("token must be a string");
    assert!(!token.is_empty(), "token must not be empty");

    // And: the token works for authenticated requests
    let profile = app.get("/profile").with_auth(token).send();
    assert_eq!(profile.status, 200);
    assert_eq!(profile.json()["email"], "alice@example.com");
}
```

## Assertion Depth Guide

Every scenario MUST contain assertions that would FAIL if the behavior broke.

**Valid assertions:**
- Exact value checks: `assert_eq!(status, 200)`
- Specific field presence: `assert!(json["token"].is_string())`
- State verification: query database after write, confirm data persisted
- Error message content: `assert!(body.contains("invalid credentials"))`

**Invalid assertions (trivially passing):**
- `assert!(true)` — proves nothing
- `assert!(response.is_some())` — doesn't check correctness
- Type-only checks without values: `assert!(val.is_string())` on its own

**Minimum per scenario:** 1 primary outcome assertion + 1 specific value assertion.

## Coverage Map Format

```markdown
## Scenario Coverage Map
| # | Scenario | Test | Status | Proof |
|---|----------|------|--------|-------|
| 1 | Valid login | scenario_user_login_valid_credentials | GREEN | Line 42 |
| 2 | Wrong password | scenario_user_login_wrong_password | GREEN | Line 48 |
| 3 | Profile retrieval | scenario_user_profile_retrieval | GREEN | Line 55 |
| 4 | Unauthorized | scenario_user_profile_unauthorized | GREEN | Line 61 |
```

All GREEN or DO NOT SHIP.

## Final Report Template

```markdown
# BDD Scenario Report: {bead/feature}

## Behavior Catalog
| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| 1 | Valid login | User exists | POST /login valid creds | 200 + token |
| 2 | Wrong password | User exists | POST /login bad creds | 401 + error |

## Scenario Coverage Map
| # | Scenario | Test Location | Status |
|---|----------|---------------|--------|
| 1 | Valid login | tests/auth::scenario_user_login_valid | GREEN |
| 2 | Wrong password | tests/auth::scenario_user_login_wrong | GREEN |

## Execution Evidence
### Scenario suite
```
$ cargo test -- --nocapture
running 5 scenarios
test scenario_user_login_valid_credentials ... ok
test scenario_user_login_wrong_password ... ok
test result: ok. 5 passed; 0 failed
```

### Existing suite
```
$ cargo test 2>&1
running 47 tests
test result: ok. 47 passed; 0 failed
```

## Scenarios Written
1. tests/auth.rs — scenario_user_login_valid_credentials
2. tests/auth.rs — scenario_user_login_wrong_password
3. tests/auth.rs — scenario_user_profile_unauthorized

## Fixes Applied
1. Added empty input handling (scenario caught a panic on empty email)
2. Fixed 401 response to include error body (scenario expected it)

## Verdict: READY TO SHIP
All behaviors covered by passing Given/When/Then scenarios. No regressions.
```

## Target Type Adaptations

### Rust (Cargo)
- Scenarios in `tests/` directory as integration tests
- Use `testcontainers` or test databases for real dependencies
- `cargo test --test scenarios -- --nocapture`
- Each `#[test]` function maps to one scenario with Given/When/Then comments
- Use `#[serial_test::serial]` for scenarios that need exclusive resource access

### TypeScript/JavaScript
- Scenarios using Playwright, Cypress, or Jest with describe/it blocks
- Describe block = scenario name, it blocks = Given/When/Then steps
- `npx playwright test` or `npm test`

### Go
- Table-driven tests with scenario names as cases
- `func TestScenarioName(t *testing.T)` with Given/When/Then comments
- `go test ./... -v`

### API/Server
- Scenarios exercise real HTTP requests through the full middleware stack
- Use supertest, reqwest, or similar — not isolated handler tests
- Assert on complete responses: status, body, headers

### CLI/Binary
- Scenarios invoke the actual binary with real arguments
- Assert on exit code, stdout, stderr
- Use `assert_cmd` (Rust), `subprocess` (Python), or `exec` (Go)

### Bug Fix
- Write a scenario that reproduces the bug (proves it existed)
- Apply fix
- Prove the scenario passes
- Prove no regressions in existing scenarios

## Troubleshooting

- **Won't compile**: Fix it. Show the fix compiling.
- **Needs features**: `cargo test --all-features`
- **Uses nextest**: `cargo nextest run` if `.config/nextest.toml` exists
- **Workspace**: `cargo test -p crate_name`
- **Needs service**: Spin it up (testcontainers, docker-compose). Can't run = GRAY.
- **Flaky scenario**: Investigate. Flakiness is a real bug — usually a race or missing cleanup in Given. Inject controlled values for time/randomness.
- **Slow suite**: Share expensive Given setup across scenarios. Use immutable fixtures, reset state between scenarios.
