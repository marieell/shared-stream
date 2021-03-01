`shared-stream` is a crate for easily cloneable streams.

# Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
shared-stream = "0.1"
```

Now, you can use shared-stream:

```rust
use shared_stream::Share;

let shared = stream::iter(1..=3).shared();
```

# License

This crate is published under the terms of the GNU Affero General Public License as
published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version.
