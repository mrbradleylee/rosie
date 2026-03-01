# dotenvy

dotenvy is a maintained fork of the `dotenv` crate that provides the same API but is actively maintained
and has a small bug‑free history. It supports standard `.env` loading and can be used with the `"toml"`
feature to automatically parse `.env` files that contain TOML syntax.

## `Cargo.toml`

```toml
[dependencies]
dotenvy = { version = "0.15", features = ["toml"] }
```

## Usage

```rust
use dotenvy::dotenv;

fn main() {
    dotenv().ok(); // Load environment variables from `.env`
    let api_key = std::env::var("OPENAI_API_KEY").expect("Missing API key");
    // ...
}
```

## Why switch?

* **Security** – the original `dotenv` crate is unmaintained and had a couple of past issues that
  could lead to memory safety bugs.
* **Modern Rust** – `dotenvy` keeps up with Rust's ecosystem, ensuring no de‑pricated dependencies.
* **Feature parity** – the API and behaviour are identical to `dotenv`, so the overall codebase is
  unchanged apart from the single `use` statement and updated dependency.

## Migrating

Add `dotenvy` to `Cargo.toml` as shown above, replace `use dotenv::dotenv;` with
`use dotenvy::dotenv;`, and then run `cargo check`. No other code changes are required.
